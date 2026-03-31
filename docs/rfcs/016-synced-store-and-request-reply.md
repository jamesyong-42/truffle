# RFC 016: SyncedStore and Request/Reply Primitives

**Status:** Draft
**Author:** James + Claude
**Date:** 2026-03-30

---

## 1. Problem Statement

Truffle provides the networking primitives (send, broadcast, subscribe, raw TCP) but lacks two higher-level building blocks that nearly every mesh application needs:

1. **Shared state across devices.** Apps need to know what sessions exist, what config each device has, who's doing what. Today this requires hand-rolling a sync protocol on top of `send()`/`subscribe()` every time — message types, version tracking, join/leave handling, conflict resolution. The original Claude-on-the-Go project implemented this as `LocalDeviceStore` + `StoreSyncAdapter` in ~600 lines of TypeScript. Every future app would reimplement the same thing.

2. **Request/reply with correlation.** "Send a message, wait for a specific reply, time out if it doesn't arrive." File transfer already does this manually three times — `send_file` sends Offer and waits for Accept/Reject by matching a token in a subscribe loop. `pull_file` does it again. `handle_pull_request` does it a third time. That's ~40 lines of correlation boilerplate per call site, and any new feature (terminal sharing, remote commands, config requests) would copy-paste it.

Both patterns are **general-purpose** and **reusable across all apps**, which means they belong in `truffle-core` alongside `FileTransfer` (same reasoning as RFC 014).

---

## 2. Non-Goals

- **Event emitter abstraction.** The codebase already uses `tokio::broadcast` channels throughout — `on_peer_change()`, `subscribe()`, `ft.subscribe()`. This is the Rust-idiomatic event emitter: typed enums, compile-time exhaustiveness, multi-consumer. Adding a string-keyed `EventEmitter<E>` would be less type-safe, not more.

- **Message bus abstraction.** The Node API already provides namespace-based pub/sub via `send()`, `broadcast()`, and `subscribe()`. A separate `MessageBus` type would be a thin wrapper with no added value.

- **CRDTs.** Device-owned slices with last-write-wins cover the majority of mesh app use cases (session lists, config, presence). Apps that need true collaborative editing can bring Automerge or Yrs. Truffle shouldn't impose that complexity.

- **Full RPC framework.** No method names, service registration, or IDL. Just a minimal `send_and_wait` utility that covers the correlation pattern.

- **Topic hierarchies or message filtering.** Namespace strings are sufficient. Apps can adopt conventions (`"store:sessions"`, `"store:config"`) without framework support.

---

## 3. Design

### 3.1 Request/Reply Utility

A single function that eliminates the correlation boilerplate:

```rust
// truffle-core/src/request_reply.rs

/// Send a namespaced message and wait for a reply that matches a predicate.
///
/// 1. Subscribes to `namespace`
/// 2. Sends `message` to `peer_id`
/// 3. Loops on incoming messages, calling `filter` on each
/// 4. Returns `Some(R)` from the filter on match, or times out
///
/// The filter receives every message on the namespace, not just from the
/// target peer — the caller decides what constitutes a valid reply.
pub async fn send_and_wait<N, F, R>(
    node: &Node<N>,
    peer_id: &str,
    namespace: &str,
    msg_type: &str,
    payload: &serde_json::Value,
    timeout: Duration,
    filter: F,
) -> Result<R, RequestError>
where
    N: NetworkProvider,
    F: Fn(&NamespacedMessage) -> Option<R>,
{
    let mut rx = node.subscribe(namespace);

    node.send_typed(peer_id, namespace, msg_type, payload).await
        .map_err(RequestError::Send)?;

    tokio::time::timeout(timeout, async {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if let Some(result) = filter(&msg) {
                        return Ok(result);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(RequestError::ChannelClosed);
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "request_reply: lagged");
                }
            }
        }
    })
    .await
    .map_err(|_| RequestError::Timeout)?
}

#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error("send failed: {0}")]
    Send(NodeError),
    #[error("timed out waiting for reply")]
    Timeout,
    #[error("subscription channel closed")]
    ChannelClosed,
}
```

**Why a function, not a method on Node?**

- Node is the public API boundary. Adding `request()` and `serve()` methods would bloat it with RPC concepts that don't belong at that layer.
- A standalone function composes naturally — callers import it when they need it, ignore it when they don't.
- It can be used by both core subsystems (FileTransfer, SyncedStore) and application code.

**Why a filter function, not a correlation ID?**

- FileTransfer matches on `token` field. SyncedStore will match on `store_id` + `msg_type`. Future features may match on anything. A generic filter covers all cases.
- Correlation IDs are a convention, not a protocol requirement. The filter lets each subsystem define its own matching logic.

#### Refactoring FileTransfer to use it

Before (sender.rs, ~40 lines per call site):
```rust
let token = Uuid::new_v4().to_string();
let msg = FtMessage::Offer { file_name, size, sha256, token: token.clone(), .. };
let data = serde_json::to_vec(&Envelope::new("ft", "offer", serde_json::to_value(&msg)?))?;
node.send(peer_id, "ft", &data).await?;

let mut rx = node.subscribe("ft");
let response = tokio::time::timeout(Duration::from_secs(30), async {
    loop {
        if let Ok(msg) = rx.recv().await {
            let ft_msg: FtMessage = serde_json::from_value(msg.payload.clone())?;
            match ft_msg {
                FtMessage::Accept { token: t, .. } if t == token => return Ok(AcceptInfo { .. }),
                FtMessage::Reject { token: t, .. } if t == token => return Err(..),
                _ => continue,
            }
        }
    }
}).await??;
```

After:
```rust
let token = Uuid::new_v4().to_string();
let offer = FtMessage::Offer { file_name, size, sha256, token: token.clone(), .. };

let response = send_and_wait(
    node, peer_id, "ft", "offer", &serde_json::to_value(&offer)?,
    Duration::from_secs(30),
    |msg| match serde_json::from_value::<FtMessage>(msg.payload.clone()).ok()? {
        FtMessage::Accept { token: t, port } if t == token => Some(Ok(AcceptInfo { port })),
        FtMessage::Reject { token: t, reason } if t == token => Some(Err(reason)),
        _ => None,
    },
).await?;
```

Cuts each call site from ~40 lines to ~10.

### 3.2 SyncedStore

A device-owned-slice state synchronization subsystem, following the same pattern as `FileTransfer`:
- Lives in `truffle-core/src/synced_store/`
- Uses Node primitives (`send`, `broadcast`, `subscribe`, `on_peer_change`)
- Owns namespace `"ss:{store_id}"` (e.g., `"ss:sessions"`, `"ss:config"`)
- Exposed via `Node::synced_store()`

#### Data Model

```rust
/// A versioned slice of data owned by a single device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slice<T> {
    /// Device that owns this slice.
    pub device_id: String,
    /// The data.
    pub data: T,
    /// Monotonically increasing version (per-device).
    pub version: u64,
    /// When this version was created (Unix ms).
    pub updated_at: u64,
}
```

Each device owns exactly one slice per store. Writes only touch your own slice. Reads can access any device's slice.

**Conflict resolution: Last-Write-Wins by version number.**

- Each `set()` increments the local version counter.
- Incoming updates are accepted only if `incoming.version > last_received_version`.
- Stale updates are silently dropped.
- No merging — the entire slice is replaced on a newer version.

This is intentionally simple. Device-owned slices with LWW are sufficient for:
- Session lists ("what terminals does each device have?")
- Config sync ("what are each device's settings?")
- Presence ("what is each device currently doing?")
- Status dashboards ("CPU load, disk space per device")

Apps that need field-level merging or conflict-free concurrent writes can layer CRDTs on top or use a different approach entirely.

#### Wire Protocol

Namespace: `"ss:{store_id}"` (e.g., `"ss:sessions"`)

| msg_type | Direction | When | Payload |
|---|---|---|---|
| `update` | broadcast | Local data changes | `Slice<T>` (full slice with version) |
| `full` | targeted | Response to `request`, or on peer join | `Slice<T>` |
| `request` | targeted | We discover a new peer | `{}` |
| `clear` | broadcast | We observe a peer go offline | `{ "device_id": "..." }` |

Message flow:

```
Device A starts, joins mesh
  → subscribes to PeerEvent::Joined
  → for each existing peer: send "request" to that peer
  → peer responds with "full" (their current slice)

Device A calls store.set(new_data)
  → version incremented
  → broadcast "update" with Slice { data, version }
  → all peers receive and apply if version > their last_received

Device B goes offline (PeerEvent::Left)
  → Device A removes B's slice from local state
  → emits StoreEvent::PeerRemoved { device_id: B }
```

#### Public API

```rust
/// A synchronized key-value store with device-owned slices.
///
/// Each device owns one slice of type `T`. Writes update only the local
/// slice and broadcast the change. Remote slices are received and cached
/// automatically.
pub struct SyncedStore<T> {
    store_id: String,
    // internal state...
}

impl<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> SyncedStore<T> {
    // ── Write (local slice only) ──

    /// Update this device's data. Increments version and broadcasts.
    pub async fn set(&self, data: T);

    // ── Read ──

    /// This device's current data.
    pub fn local(&self) -> Option<T>;

    /// A specific peer's data.
    pub fn get(&self, device_id: &str) -> Option<Slice<T>>;

    /// All slices (all devices including self).
    pub fn all(&self) -> HashMap<String, Slice<T>>;

    /// List of device IDs that have data in this store.
    pub fn device_ids(&self) -> Vec<String>;

    // ── Subscribe ──

    /// Subscribe to store change events.
    pub fn subscribe(&self) -> broadcast::Receiver<StoreEvent<T>>;

    // ── Lifecycle ──

    /// Stop syncing and clean up background tasks.
    pub async fn stop(&self);
}

/// Events emitted by a SyncedStore.
#[derive(Debug, Clone)]
pub enum StoreEvent<T> {
    /// Local data was updated via set().
    LocalChanged(T),
    /// A remote peer's data was received or updated.
    PeerUpdated {
        device_id: String,
        data: T,
        version: u64,
    },
    /// A remote peer's data was removed (peer went offline).
    PeerRemoved {
        device_id: String,
    },
}
```

#### Node Integration

```rust
// In Node:
pub fn synced_store<T>(&self, store_id: &str) -> SyncedStore<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static;
```

Usage:
```rust
let node = Node::builder()
    .name("my-app")
    .sidecar_path("/usr/local/bin/sidecar-slim")
    .build()
    .await?;

// Create a synced store for terminal sessions
let sessions: SyncedStore<Vec<Session>> = node.synced_store("sessions");
sessions.set(vec![Session { id: "abc", title: "bash" }]);

// Read all peers' sessions
for (device_id, slice) in sessions.all() {
    println!("{}: {:?}", device_id, slice.data);
}

// React to changes
let mut rx = sessions.subscribe();
tokio::spawn(async move {
    while let Ok(event) = rx.recv().await {
        match event {
            StoreEvent::PeerUpdated { device_id, data, .. } => {
                println!("{device_id} updated their sessions: {data:?}");
            }
            StoreEvent::PeerRemoved { device_id } => {
                println!("{device_id} went offline");
            }
            _ => {}
        }
    }
});
```

#### Persistence (Optional)

```rust
/// Backend for persisting store data across restarts.
pub trait StoreBackend: Send + Sync {
    /// Load a device's slice. Returns (serialized_data, version) or None.
    fn load(&self, store_id: &str, device_id: &str) -> Option<(Vec<u8>, u64)>;
    /// Save a device's slice.
    fn save(&self, store_id: &str, device_id: &str, data: &[u8], version: u64);
    /// Remove a device's slice.
    fn remove(&self, store_id: &str, device_id: &str);
}
```

Two built-in implementations:
- `MemoryBackend` (default) — `HashMap` in memory, no disk I/O
- `FileBackend` — one JSON file per store per device in a configurable directory

Apps can implement `StoreBackend` for SQLite, sled, or whatever they prefer.

With persistence, a device can restart and retain both its own state (resuming at the correct version) and cached remote state (avoiding a full resync).

Node integration with persistence:
```rust
let store: SyncedStore<Config> = node.synced_store_with_backend(
    "config",
    FileBackend::new("/var/lib/truffle/stores"),
);
```

---

## 4. Internal Architecture

```
truffle-core/src/
├── node.rs                    ← adds synced_store() method
├── request_reply.rs           ← send_and_wait() utility (NEW)
├── synced_store/              ← (NEW)
│   ├── mod.rs                 ← SyncedStore<T>, StoreEvent<T>
│   ├── types.rs               ← Slice<T>, SyncMessage, StoreBackend trait
│   ├── sync.rs                ← Background sync task (peer join/leave handling)
│   ├── backend.rs             ← MemoryBackend, FileBackend
│   └── tests.rs
├── file_transfer/             ← (existing, refactor to use send_and_wait)
│   ├── mod.rs
│   ├── types.rs
│   ├── sender.rs              ← refactor: use send_and_wait
│   └── receiver.rs
└── ...
```

The SyncedStore background task:
1. Subscribes to `node.subscribe("ss:{store_id}")` for incoming sync messages
2. Subscribes to `node.on_peer_change()` for join/leave events
3. On `PeerEvent::Joined` → sends `request` to new peer
4. On `PeerEvent::Left` → removes peer's slice, emits `PeerRemoved`
5. On incoming `update`/`full` → applies if version > last seen, emits `PeerUpdated`
6. On `store.set()` → increments version, broadcasts `update`, persists if backend present

---

## 5. Implementation Plan

### Phase 1: request_reply utility

1. Create `truffle-core/src/request_reply.rs` with `send_and_wait()`
2. Add helper `Node::send_typed()` (send with explicit msg_type and Value payload — avoids manual Envelope construction)
3. Add tests using mock network provider
4. Refactor `file_transfer/sender.rs` to use `send_and_wait` for the Offer→Accept flow
5. Refactor `file_transfer/mod.rs` `pull_file` to use `send_and_wait` for the PullRequest→Offer flow

**Estimated scope:** ~150 LOC new, ~80 LOC removed from file_transfer.

### Phase 2: SyncedStore core

1. Create `synced_store/types.rs` — `Slice<T>`, `SyncMessage`, `StoreEvent<T>`
2. Create `synced_store/backend.rs` — `StoreBackend` trait, `MemoryBackend`
3. Create `synced_store/mod.rs` — `SyncedStore<T>` with `set`, `local`, `get`, `all`, `subscribe`, `stop`
4. Create `synced_store/sync.rs` — background task handling peer events and incoming messages
5. Add `Node::synced_store()` method
6. Tests: two mock nodes, set/get, sync on join, cleanup on leave, version ordering, stale rejection

**Estimated scope:** ~500 LOC.

### Phase 3: Persistence

1. Add `FileBackend` to `synced_store/backend.rs`
2. Add `Node::synced_store_with_backend()` method
3. Test: set data, "restart" (drop and recreate store), verify data loaded from disk at correct version

**Estimated scope:** ~150 LOC.

### Phase 4: CLI integration (optional)

Add `truffle store` commands for debugging:
- `truffle store ls` — list all stores and their device count
- `truffle store get <store_id>` — dump all slices as JSON
- `truffle store watch <store_id> --json` — stream store events

---

## 6. What We Explicitly Don't Build

| Feature | Why not |
|---|---|
| EventEmitter abstraction | `broadcast::Receiver<T>` is already the Rust-idiomatic pattern; every subsystem uses it |
| MessageBus type | Node API already provides namespace pub/sub via send/broadcast/subscribe |
| CRDTs | Device-owned slices cover 90% of cases; apps can layer CRDTs on top if needed |
| RPC framework | `send_and_wait()` with a filter covers the pattern without method registries or IDLs |
| Topic hierarchies | Namespace strings + conventions (`"ss:sessions"`) are sufficient |
| Message ordering guarantees | TCP provides per-connection ordering; cross-peer ordering is an app concern |
| Partial slice updates (deltas) | Entire slice replacement keeps the protocol simple; apps can structure data to minimize payload size |

---

## 7. Future Considerations

**Not in scope for this RFC, but worth noting:**

- **Merge function.** A future version could accept an optional `merge: Fn(old: &T, new: &T) -> T` for smarter conflict resolution than full replacement.
- **TTL / expiry.** Slices could have a configurable time-to-live for ephemeral data like presence or typing indicators.
- **Schema evolution.** Version numbers track data freshness, not schema versions. Apps are responsible for backward-compatible serialization.
- **Encryption.** Tailscale provides transport encryption. If end-to-end encryption of store data is needed, apps should encrypt `T` before calling `set()`.
