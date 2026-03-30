# RFC 015: NAPI-RS and Tauri Plugin Rewrite

**Status:** Draft
**Author:** James + Claude
**Date:** 2026-03-30

---

## 1. Problem Statement

The NAPI-RS bindings (`truffle-napi`) and Tauri plugin (`truffle-tauri-plugin`) were written against the **pre-RFC-012 architecture** (MeshNode, TruffleRuntime, services/, mesh/, protocol/, types/). The current truffle-core is a clean-room rewrite with a completely different module tree. **Neither crate compiles.**

Every `use truffle_core::...` in both crates references paths that no longer exist. This is not an incremental update — it requires a full rewrite of both binding layers.

## 2. Scope

### What needs rewriting

| Crate | Files | Old API Surface | New API Surface |
|-------|-------|----------------|-----------------|
| `truffle-napi` | 5 files, ~1.2k LOC | NapiMeshNode (16 methods), NapiFileTransferAdapter (10 methods), NapiStoreSyncAdapter (9 methods) | NapiNode (~15 methods), NapiFileTransfer (~8 methods) |
| `truffle-tauri-plugin` | 4 files, ~600 LOC | 13 commands + 2 event types | ~15 commands + PeerEvent forwarding |

### What's dropped

- **Store sync** (`NapiStoreSyncAdapter`) — no equivalent in current core. Defer to future RFC.
- **Proxy manager** (`proxy_add/remove/list`) — no equivalent in current core. Defer.
- **Sidecar lifecycle events** (`SidecarStarted`, `SidecarCrashed`, etc.) — gone from API. Only `PeerEvent` available.
- **Old type system** (`BaseDevice`, `DeviceStatus`, `TailnetPeer`, `MeshEnvelope`) — replaced by `Peer`, `PeerState`, `NamespacedMessage`.

## 3. Current Node API (What Bindings Must Wrap)

### Node<TailscaleProvider>

| Method | Async | JS/Tauri Equivalent |
|--------|-------|---------------------|
| `Node::builder()` | No | Constructor pattern |
| `local_info()` | No | `getLocalInfo()` |
| `peers()` | Yes | `getPeers()` |
| `on_peer_change()` | No | Event emitter / callback |
| `resolve_peer_id()` | Yes | `resolvePeerId()` |
| `ping()` | Yes | `ping()` |
| `health()` | Yes | `getHealth()` |
| `send()` | Yes | `send()` |
| `broadcast()` | Yes | `broadcast()` |
| `subscribe()` | No | Event emitter / callback per namespace |
| `open_tcp()` | Yes | Not exposed (raw TCP hard to bridge to JS) |
| `listen_tcp()` | Yes | Not exposed |
| `stop()` | Yes | `stop()` |

### FileTransfer<TailscaleProvider>

| Method | Async | JS/Tauri Equivalent |
|--------|-------|---------------------|
| `send_file()` | Yes | `sendFile()` |
| `pull_file()` | Yes | `pullFile()` |
| `auto_accept()` | Yes | `autoAccept()` |
| `auto_reject()` | Yes | `autoReject()` |
| `offer_channel()` | Yes | Event emitter for offers |
| `subscribe()` | No | Event emitter for progress/completion |
| `set_max_transfer_size()` | No | `setMaxTransferSize()` |

### Key Types to Bridge

| Rust Type | JS/Tauri Representation |
|-----------|------------------------|
| `Peer` | `{ id, name, ip, online, connected, connectionType, os, lastSeen }` |
| `PeerEvent` | Discriminated union / event name + payload |
| `NamespacedMessage` | `{ from, namespace, msgType, payload, timestamp }` |
| `NodeIdentity` | `{ id, name, hostname }` |
| `PingResult` | `{ latencyMs, connection, peerAddr }` |
| `FileOffer` | `{ fromPeer, fromName, fileName, size, sha256, suggestedPath, token }` |
| `FileTransferEvent` | Discriminated union (Hashing, WaitingForAccept, Progress, Completed, Failed, Rejected) |
| `TransferResult` | `{ bytesTransferred, sha256, elapsedSecs }` |

## 4. NAPI-RS Binding Design

### Architecture

```
NapiNode (JS class)
├── constructor(config: NapiNodeConfig) — calls NodeBuilder
├── start() → Promise<void> — calls builder.build().await
├── stop() → Promise<void>
├── getLocalInfo() → NapiNodeIdentity
├── getPeers() → Promise<NapiPeer[]>
├── ping(peerId: string) → Promise<NapiPingResult>
├── getHealth() → Promise<NapiHealthInfo>
├── send(peerId: string, namespace: string, data: Buffer) → Promise<void>
├── broadcast(namespace: string, data: Buffer) → Promise<void>
├── onPeerChange(callback: (event: NapiPeerEvent) => void)
├── onMessage(namespace: string, callback: (msg: NapiMessage) => void)
├── fileTransfer() → NapiFileTransfer
└── destroy() — cleanup

NapiFileTransfer (JS class)
├── sendFile(peerId, localPath, remotePath) → Promise<NapiTransferResult>
├── pullFile(peerId, remotePath, localPath) → Promise<NapiTransferResult>
├── autoAccept(outputDir: string) → Promise<void>
├── autoReject() → Promise<void>
├── onOffer(callback: (offer: NapiFileOffer, responder: NapiOfferResponder) => void)
├── onEvent(callback: (event: NapiFileTransferEvent) => void)
└── setMaxTransferSize(bytes: number)
```

### Async Pattern

NAPI-RS v3 supports async natively. Use `#[napi]` with `async fn` for Promise-returning methods. For event callbacks, use `ThreadsafeFunction<Blocking>` to call JS from Rust tasks.

### State Management

```rust
#[napi]
pub struct NapiNode {
    node: Option<Arc<Node<TailscaleProvider>>>,
    // Keep handles to spawned event-forwarding tasks
    peer_event_handle: Option<JoinHandle<()>>,
    message_handles: HashMap<String, JoinHandle<()>>,
}
```

The `Arc<Node<TailscaleProvider>>` is created in `start()` and stored. `destroy()` drops it.

## 5. Tauri Plugin Design

### Architecture

```rust
// State
pub struct TruffleState {
    node: RwLock<Option<Arc<Node<TailscaleProvider>>>>,
}

// Commands
#[tauri::command] async fn start(config: TruffleConfig, ...) → Result<NodeIdentity>
#[tauri::command] async fn stop(...)
#[tauri::command] async fn get_local_info(...) → NodeIdentity
#[tauri::command] async fn get_peers(...) → Vec<Peer>
#[tauri::command] async fn ping(peer_id: String, ...) → PingResult
#[tauri::command] async fn send(peer_id: String, namespace: String, data: Vec<u8>, ...)
#[tauri::command] async fn broadcast(namespace: String, data: Vec<u8>, ...)
#[tauri::command] async fn send_file(peer_id: String, local_path: String, ...)  → TransferResult
#[tauri::command] async fn pull_file(peer_id: String, remote_path: String, ...) → TransferResult
#[tauri::command] async fn auto_accept(output_dir: String, ...)
#[tauri::command] async fn auto_reject(...)

// Events (emitted to frontend via app.emit())
"truffle://peer-event" → PeerEvent
"truffle://message" → NamespacedMessage
"truffle://file-transfer-event" → FileTransferEvent
"truffle://file-offer" → FileOffer (frontend responds via accept_offer/reject_offer commands)
```

### Accept/Reject Flow for Tauri

The frontend receives `"truffle://file-offer"` events and calls back:

```rust
#[tauri::command] async fn accept_offer(token: String, save_path: String, ...)
#[tauri::command] async fn reject_offer(token: String, reason: String, ...)
```

The plugin stores pending `OfferResponder`s in a `HashMap<String, OfferResponder>` keyed by token.

## 6. Implementation Phases

### Phase 1: NAPI-RS rewrite
- Delete old source files (mesh_node.rs, types.rs, file_transfer.rs, store_sync.rs)
- Create new NapiNode + NapiFileTransfer classes
- Bridge all Node methods with proper async/ThreadsafeFunction patterns
- Add to workspace, verify `cargo check`

### Phase 2: Tauri plugin rewrite
- Delete old source files (commands.rs, events.rs, types.rs)
- Create new commands mapping to Node API
- Event forwarding via `app.emit()`
- Offer responder storage for accept/reject
- Add to workspace, verify `cargo check`

### Phase 3: TypeScript type generation
- Generate TypeScript types for the NAPI bindings
- Generate TypeScript types for the Tauri commands/events
- Update `@vibecook/truffle` npm package

### Phase 4: React hooks update
- Update `@vibecook/truffle-react` hooks to use the new NAPI API
- `useMesh()` → peers, send, broadcast
- `useSyncedStore()` → deferred (no store sync in core)

## 7. What's NOT Included

- Store sync (no core implementation exists)
- Reverse proxy (no core implementation exists)
- Raw TCP/UDP exposure to JS (too low-level)
- QUIC (stub in core)

## 8. Open Questions

- Should the NAPI bindings expose file transfer events as Node.js EventEmitter or callback pattern?
- Should the Tauri plugin support the offer channel for interactive accept/reject, or just auto-accept?
- Should we regenerate the TypeScript bindings automatically from the NAPI definitions?
