# RFC 018: CRDT Documents

**Status:** Draft
**Author:** James + Claude
**Date:** 2026-04-12

---

## 1. Problem Statement

Truffle's SyncedStore (RFC 016) provides device-owned slices with last-write-wins conflict resolution. Each device owns exactly one slice per store; writes replace the entire slice atomically. This covers ~90% of mesh app use cases: presence, config, session lists, status dashboards.

But a growing class of applications needs **shared, multi-writer state** where any peer can edit any part of the same document and edits merge automatically:

- **Collaborative boards** — multiple users dragging cards, editing text, reordering lists concurrently.
- **Shared game state** — inventory, score, world state modified by multiple players simultaneously.
- **Collaborative text editing** — real-time co-authoring with cursors and selections.
- **Shared configuration** — multiple admins editing the same settings without stomping each other.
- **Distributed task lists** — any device can add, reorder, check off items.

With SyncedStore, concurrent edits to the same data are **lossy** — the last arrival wins and the other edit vanishes. If device A sets `{x: 1}` and device B sets `{y: 2}` concurrently, whichever arrives second erases the other completely. There is no field-level merging.

Applications that need true multi-writer convergence today must bring their own CRDT library, wire it through Truffle's `send()`/`subscribe()`, handle peer join/leave, implement persistence, and re-implement the same integration in NAPI and React bindings. This is the same "everyone reimplement the same thing" problem RFC 016 solved for device-owned state.

**CRDTs belong in truffle-core** for the same reason FileTransfer and SyncedStore do: they are general-purpose, reusable across all mesh apps, and the integration with Truffle's peer lifecycle is non-trivial to get right.

---

## 2. Non-Goals

- **Replacing SyncedStore.** SyncedStore is the right tool for device-owned state (presence, config, session lists). CrdtDoc is for shared multi-writer state. They coexist on different namespaces.

- **Building our own CRDT.** CRDT algorithms (YATA, Fugue, RGA, Peritext) are deeply researched and extremely complex to implement correctly. We embed an existing library.

- **Server-mediated sync.** CrdtDoc is peer-to-peer, like everything in Truffle. No central server, relay, or authority.

- **End-to-end encryption.** Tailscale provides transport encryption. If apps need payload-level E2E encryption, they can encrypt before writing to the CRDT. A future RFC can address this.

- **Access control.** Any peer in the mesh can edit any document. Document-level permissions are an application concern for now.

- **Offline-first persistence guarantees.** We provide persistence hooks (snapshot + WAL), but crash recovery semantics depend on the backend implementation. We don't guarantee zero-loss durability — that's what databases are for.

---

## 3. Decision: Why Loro

We evaluated every significant CRDT library in the Rust ecosystem:

| Library | Data Types | Rust API | Sync Model | Status |
|---------|-----------|----------|------------|--------|
| **Loro** | Text, Map, List, MovableList, Tree, Counter | Excellent (Rust-native) | Version vector deltas | 1.x stable, active |
| Automerge | Map, List, Text, Counter | Poor (JS-first) | Bloom filter DAG | 3.x, active |
| Yrs (Yjs) | Text, Array, Map, XML | Good | State vector diffs | 0.21 pre-1.0, slowed |
| Diamond Types | Text only | Outdated crate | Basic | WIP |
| rust-crdt | Primitives (GCounter, ORSet, LWWReg) | Excellent | Manual | Dormant |

**Loro wins for Truffle because:**

1. **Rust-native with excellent API.** Unlike Automerge (JS-oriented, poor Rust docs) or Yrs (development slowed, pre-1.0), Loro was built Rust-first with clean `LoroDoc` → container API.

2. **Rich, nestable containers.** `LoroMap` (LWW per key), `LoroList` (Fugue), `LoroMovableList` (drag-and-drop), `LoroTree` (hierarchical), `LoroText` (Fugue + Peritext rich text), `LoroCounter`. Containers nest — a Map inside a List inside a Map. Full JSON-like document model with CRDT semantics at every level.

3. **Perfect mesh fit.** Version-vector-based delta sync with zero central-server assumption. `doc.export(ExportMode::updates(&their_vv))` exports only missing ops. Each peer independently tracks what it knows.

4. **No built-in networking.** Loro is purely a data-structure library. Truffle brings WebSocket transport, namespace routing, and peer lifecycle. Loro handles merge logic. Clean separation.

5. **Stable binary format.** 1.0+ with guaranteed no breaking changes to encoding. Monthly releases. 5,500+ stars.

6. **Extras we get for free.** Time travel (`checkout`), branching (`fork`), undo/redo (`UndoManager`), `EphemeralStore` for presence/awareness.

7. **Performance leader.** Fastest in crdt-benchmarks across operation throughput. Competitive encoding sizes.

### What about Automerge?

Automerge is more established and academically backed (Ink & Switch), but its Rust API is explicitly described as "low level and not well documented" — it's oriented around being a backend for the JavaScript wrapper. For a Rust-first library like Truffle, this is a dealbreaker.

### What about Yrs?

Yrs would give us wire-level compatibility with the Yjs ecosystem (ProseMirror, Monaco, CodeMirror providers). However, development has slowed significantly (last tagged release March 2024), it's still pre-1.0, and we don't have a concrete need for Yjs interop. If that need arises, a future RFC can add a Yrs-backed variant alongside the Loro one.

---

## 4. Design

### 4.1 Architecture Overview

CrdtDoc follows the same subsystem pattern as FileTransfer and SyncedStore:

- Lives in `truffle-core/src/crdt_doc/`
- Uses Node primitives (`send_typed`, `broadcast_typed`, `subscribe`, `on_peer_change`)
- Owns namespace `"cd:{doc_id}"` (e.g., `"cd:shared-board"`, `"cd:game-state"`)
- Exposed via `Node::crdt_doc()` and `Node::crdt_doc_with_backend()`

```
┌──────────────────────────────────────────────────────────┐
│                    Application Layer                      │
├────────────┬───────────────┬─────────────────────────────┤
│ SyncedStore│  CrdtDoc (NEW)│    FileTransfer             │
│ "ss:{id}"  │  "cd:{id}"   │    "ft"                     │
├────────────┴───────────────┴─────────────────────────────┤
│                    Node API                               │
│  send_typed · broadcast_typed · subscribe · on_peer_change│
├──────────────────────────────────────────────────────────┤
│              Envelope (namespace routing)                  │
├──────────────────────────────────────────────────────────┤
│              Session (PeerRegistry, WS)                   │
├──────────────────────────────────────────────────────────┤
│              Transport (WebSocket, TCP, UDP)               │
├──────────────────────────────────────────────────────────┤
│              Network (Tailscale, WatchIPNBus)              │
└──────────────────────────────────────────────────────────┘
```

### 4.2 Data Model

Each CrdtDoc wraps a `loro::LoroDoc` — a document containing named CRDT containers:

```rust
use loro::{LoroDoc, LoroText, LoroMap, LoroList, LoroMovableList, LoroTree, LoroCounter};

let doc = node.crdt_doc("shared-board");

// Get typed containers (created on first access)
let cards: LoroMap       = doc.map("cards");
let order: LoroList      = doc.list("card-order");
let title: LoroText      = doc.text("board-title");
let tree:  LoroTree      = doc.tree("folder-structure");
let votes: LoroCounter   = doc.counter("vote-count");

// Containers nest
cards.insert("card-1", loro_value!({ "title": "Task A", "done": false }))?;
order.push("card-1")?;
title.insert(0, "Sprint Board")?;
votes.increment(1.0)?;
```

**Any peer can edit any container.** Loro's CRDT algorithms guarantee convergence regardless of edit ordering, concurrency, or network partitions.

**Conflict resolution per container type:**

| Container | Algorithm | Conflict behavior |
|-----------|-----------|-------------------|
| `LoroMap` | LWW per key (Lamport timestamp) | Concurrent writes to same key: higher Lamport wins |
| `LoroList` | Fugue | Concurrent inserts at same position: deterministic interleaving |
| `LoroMovableList` | Fugue + move | Concurrent moves: both applied, last move wins |
| `LoroText` | Fugue + Peritext | Concurrent inserts: interleaved; marks: Peritext rules |
| `LoroTree` | Movable Tree CRDT | Concurrent moves: both applied, cycle-safe |
| `LoroCounter` | PN-Counter | Concurrent increments: all applied (exact sum) |

### 4.3 Wire Protocol

Namespace: `"cd:{doc_id}"` (e.g., `"cd:shared-board"`)

```rust
/// Messages exchanged between peers for CRDT document sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CrdtSyncMessage {
    /// Incremental update — broadcast on every local edit.
    /// Payload: Loro binary update bytes (base64-encoded for JSON transport).
    #[serde(rename = "update")]
    Update {
        data: Vec<u8>,
    },

    /// Sync request — sent to a peer to request missing ops.
    /// Payload: our version vector so the peer can compute the delta.
    #[serde(rename = "sync_request")]
    SyncRequest {
        version_vector: Vec<u8>,
    },

    /// Sync response — targeted reply with missing ops.
    #[serde(rename = "sync_response")]
    SyncResponse {
        data: Vec<u8>,
    },

    /// Full snapshot — for new peers or when delta would be larger than snapshot.
    #[serde(rename = "snapshot")]
    Snapshot {
        data: Vec<u8>,
    },
}
```

**Binary payloads.** Unlike SyncedStore's JSON messages, CrdtDoc messages carry Loro's binary encoding directly. The `data` fields are Loro export bytes — opaque to Truffle, meaningful only to `LoroDoc::import()`. They are base64-encoded when serialized as the JSON envelope payload, and decoded on receipt.

### 4.4 Sync Protocol

**Delta-state sync using version vectors.** This is the right model for Truffle's mesh topology because:
- No causal delivery requirement (messages can arrive out of order)
- Idempotent merge (redelivery is harmless)
- No central coordination
- Bandwidth-efficient (only missing ops cross the wire)

#### Steady-state: local edit broadcast

```
Device A edits document:
  1. User calls cards.insert("card-2", value)
  2. LoroDoc auto-commits the operation
  3. subscribe_local_update() fires with byte[] delta
  4. Sync task broadcasts CrdtSyncMessage::Update { data: delta }
     on namespace "cd:{doc_id}"

Device B receives Update:
  1. loro_doc.import(&data) → automatic CRDT merge
  2. Emit CrdtDocEvent::RemoteChange with diff details
```

#### Peer join: catch-up sync

```
Device C comes online (was offline for a while):
  1. PeerEvent::Joined(C) triggers sync task
  2. Our sync task sends SyncRequest { version_vector: our_vv }
     targeted to C
  3. C also sends SyncRequest { version_vector: c_vv } to us
  4. We compute: doc.export(ExportMode::updates(&c_vv)) → delta
  5. If delta.len() < SNAPSHOT_THRESHOLD:
       Send SyncResponse { data: delta }
     Else:
       Send Snapshot { data: doc.export(ExportMode::snapshot()) }
  6. C imports our response → caught up
  7. C sends us their delta → we're caught up
  8. Both peers are now in sync
```

#### Peer leave

```
Device B goes offline:
  1. PeerEvent::Left(B) received
  2. No data cleanup needed — B's edits are already merged into the document
  3. Emit CrdtDocEvent::PeerLeft { peer_id: B }
  4. When B reconnects, catch-up sync handles the gap
```

**Key difference from SyncedStore:** When a peer leaves, their data stays. CRDT documents have no concept of "device-owned slices" — all edits are merged into a single shared document. This is the fundamental distinction.

### 4.5 Persistence

#### CrdtBackend trait

```rust
/// Backend for persisting CRDT document state across restarts.
///
/// The persistence model is snapshot + incremental updates (WAL-style):
/// - Snapshots capture the full document state (expensive to create, fast to load)
/// - Updates are appended incrementally (cheap to write, accumulate over time)
/// - Compaction replaces accumulated updates with a fresh snapshot
///
/// Methods are synchronous — same reasoning as StoreBackend (RFC 016 §3.2).
pub trait CrdtBackend: Send + Sync + 'static {
    /// Load the latest snapshot for a document.
    fn load_snapshot(&self, doc_id: &str) -> Option<Vec<u8>>;

    /// Load incremental updates saved after the last snapshot.
    /// Returns updates in write order.
    fn load_updates(&self, doc_id: &str) -> Vec<Vec<u8>>;

    /// Append an incremental update.
    fn save_update(&self, doc_id: &str, data: &[u8]);

    /// Save a full snapshot and clear accumulated updates (compaction).
    fn save_snapshot(&self, doc_id: &str, data: &[u8]);

    /// Remove all persisted data for a document.
    fn remove(&self, doc_id: &str);
}
```

**Why snapshot + updates, not just snapshots?**

A full Loro snapshot export is O(document size). For a large document with frequent edits, re-snapshotting on every keystroke is prohibitive. Instead:
- Each local edit appends a small incremental update (~bytes to KB)
- Periodically (or on explicit request), a full snapshot replaces the accumulated updates
- On startup: load snapshot, replay updates — document is fully restored

This is the same WAL pattern used by SQLite, Automerge-repo, and Yjs persistence adapters.

#### Built-in backends

**MemoryCrdtBackend (default):**

```rust
pub struct MemoryCrdtBackend;

impl CrdtBackend for MemoryCrdtBackend {
    fn load_snapshot(&self, _doc_id: &str) -> Option<Vec<u8>> { None }
    fn load_updates(&self, _doc_id: &str) -> Vec<Vec<u8>> { vec![] }
    fn save_update(&self, _doc_id: &str, _data: &[u8]) {}
    fn save_snapshot(&self, _doc_id: &str, _data: &[u8]) {}
    fn remove(&self, _doc_id: &str) {}
}
```

No-op. Data lives only in memory. Suitable for ephemeral documents.

**CrdtFileBackend:**

```rust
pub struct CrdtFileBackend {
    base_dir: PathBuf,
}
```

On-disk layout:
```
{base_dir}/
  {doc_id}/
    snapshot.bin            ← latest full Loro export (atomic write)
    updates/
      000001.bin            ← incremental delta (append-only)
      000002.bin
      ...
```

- `save_update`: append-only write to `updates/{seq}.bin`
- `save_snapshot`: atomic write to `snapshot.bin`, then delete `updates/` directory
- `load_snapshot`: read `snapshot.bin` if it exists
- `load_updates`: read all `updates/*.bin` in filename order
- Path sanitization: same `sanitize()` function as FileBackend

**Compaction policy:**

The sync task auto-compacts when the total size of accumulated updates exceeds the snapshot size. This is the same heuristic used by Automerge-repo. Applications can also trigger manual compaction via `doc.compact()`.

#### Startup restoration flow

```
1. backend.load_snapshot(doc_id)
   → If Some(bytes): doc = LoroDoc::from_snapshot(&bytes)
   → If None: doc = LoroDoc::new()

2. for update in backend.load_updates(doc_id):
       doc.import(&update)

3. Document is fully restored at the exact state it was in when the process exited.
```

### 4.6 Node Integration

```rust
impl<N: NetworkProvider + 'static> Node<N> {
    /// Create a CRDT document (no persistence).
    pub fn crdt_doc(self: &Arc<Self>, doc_id: &str) -> Arc<CrdtDoc> { ... }

    /// Create a CRDT document with persistence.
    pub fn crdt_doc_with_backend(
        self: &Arc<Self>,
        doc_id: &str,
        backend: Arc<dyn CrdtBackend>,
    ) -> Arc<CrdtDoc> { ... }
}
```

**Exposing state_dir.** Today, Node's `state_dir` is buried inside `TailscaleConfig` and not accessible after construction. We add a public accessor so applications can derive persistence paths:

```rust
impl<N: NetworkProvider + 'static> Node<N> {
    /// The node's state directory path.
    /// Default: `{dirs::data_dir}/truffle/{app_id}/{slug(device_name)}/`
    pub fn state_dir(&self) -> &Path { ... }
}
```

This enables the ergonomic pattern:

```rust
let doc = node.crdt_doc_with_backend(
    "shared-board",
    Arc::new(CrdtFileBackend::new(node.state_dir().join("crdt"))),
);
```

Resulting on-disk layout:
```
~/Library/Application Support/truffle/{app_id}/{device_name}/
  device-id.txt           ← existing (RFC 017)
  tsnet/                  ← existing (sidecar state)
  crdt/                   ← NEW
    shared-board/
      snapshot.bin
      updates/
        000001.bin
```

### 4.7 Public API

```rust
/// A CRDT document synchronized across the mesh.
///
/// Wraps a `loro::LoroDoc` with automatic peer sync, event handling,
/// and optional persistence. Any peer can edit any part of the document;
/// all edits merge automatically via Loro's CRDT algorithms.
pub struct CrdtDoc {
    doc_id: String,
    doc: Arc<Mutex<LoroDoc>>,
    event_tx: broadcast::Sender<CrdtDocEvent>,
    task_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    backend: Arc<dyn CrdtBackend>,
}

impl CrdtDoc {
    // ── Container access ──

    /// Get a CRDT map container. Created on first access.
    pub fn map(&self, name: &str) -> LoroMap;

    /// Get a CRDT list container.
    pub fn list(&self, name: &str) -> LoroList;

    /// Get a CRDT movable list container.
    pub fn movable_list(&self, name: &str) -> LoroMovableList;

    /// Get a CRDT text container (rich text with marks).
    pub fn text(&self, name: &str) -> LoroText;

    /// Get a CRDT tree container (hierarchical, cycle-safe).
    pub fn tree(&self, name: &str) -> LoroTree;

    /// Get a CRDT counter (distributed increment/decrement).
    pub fn counter(&self, name: &str) -> LoroCounter;

    // ── Document-level operations ──

    /// Get the full document state as a JSON-like value.
    pub fn get_deep_value(&self) -> LoroValue;

    /// Explicit commit (auto-commit is on by default).
    pub fn commit(&self);

    // ── Sync ──

    /// Export a full snapshot (for manual backup or transfer).
    pub fn export_snapshot(&self) -> Result<Vec<u8>, CrdtDocError>;

    /// Export updates since a version vector (for manual delta sync).
    pub fn export_updates_since(&self, vv: &[u8]) -> Result<Vec<u8>, CrdtDocError>;

    /// Import raw Loro bytes (snapshot or updates).
    pub fn import(&self, data: &[u8]) -> Result<(), CrdtDocError>;

    /// This document's current version vector (serialized).
    pub fn version_vector(&self) -> Vec<u8>;

    // ── History ──

    /// Checkout to a historical version (enters detached mode).
    pub fn checkout(&self, frontiers: &Frontiers) -> Result<(), CrdtDocError>;

    /// Return to the latest version.
    pub fn checkout_to_latest(&self);

    /// Whether the document is in detached (time-travel) mode.
    pub fn is_detached(&self) -> bool;

    /// Fork the document (deep clone with new peer ID).
    pub fn fork(&self) -> LoroDoc;

    // ── Undo/Redo ──

    /// Create an UndoManager for this document.
    pub fn undo_manager(&self) -> UndoManager;

    // ── Events ──

    /// Subscribe to document change events.
    pub fn subscribe(&self) -> broadcast::Receiver<CrdtDocEvent>;

    // ── Persistence ──

    /// Force a compaction (snapshot + clear WAL).
    pub fn compact(&self) -> Result<(), CrdtDocError>;

    // ── Lifecycle ──

    /// The document ID.
    pub fn doc_id(&self) -> &str;

    /// Stop syncing and clean up background tasks.
    pub async fn stop(&self);
}

/// Events emitted by a CrdtDoc.
#[derive(Debug, Clone)]
pub enum CrdtDocEvent {
    /// Local edit was committed and broadcast to peers.
    LocalChange,

    /// Remote edit was received and merged.
    RemoteChange {
        from: String,  // peer's device_id
    },

    /// A peer joined and catch-up sync completed.
    PeerSynced {
        peer_id: String,
    },

    /// A peer left the mesh.
    PeerLeft {
        peer_id: String,
    },
}
```

### 4.8 Relationship to SyncedStore

| Aspect | SyncedStore | CrdtDoc |
|--------|-------------|---------|
| **Use case** | Device-owned state: presence, config, cursors, session lists | Shared multi-writer state: boards, docs, game state |
| **Ownership** | Each device owns exactly one slice | Any peer edits any part |
| **Conflict model** | LWW (lossy, simple) | CRDT merge (lossless, convergent) |
| **On peer leave** | Peer's slice removed | Peer's edits remain in document |
| **Namespace** | `"ss:{store_id}"` | `"cd:{doc_id}"` |
| **Data format** | JSON (serde_json::Value) | Loro binary |
| **Generic over T** | Yes (`SyncedStore<T>`) | No (LoroDoc is the type) |
| **Persistence unit** | One JSON file per device per store | Snapshot + WAL per document |

**They coexist.** A typical app might use:
- `SyncedStore<UserPresence>` on `"ss:presence"` for cursor positions and online status
- `CrdtDoc` on `"cd:shared-board"` for collaborative board state

### 4.9 EphemeralStore / Presence

Loro provides `EphemeralStore` — a key-value store with TTL-based expiry, designed for presence/awareness data (cursors, selections, typing indicators). This is very similar to SyncedStore's purpose.

**We do NOT replace SyncedStore with EphemeralStore in this RFC.** SyncedStore works, is well-tested, and has NAPI + React bindings. However, for CrdtDoc users who want CRDT-aware presence (e.g., cursor positions referenced by CRDT element IDs rather than absolute offsets), we expose Loro's EphemeralStore alongside the document:

```rust
impl CrdtDoc {
    /// Get the ephemeral store for this document (presence/awareness).
    /// Keys are string-typed; values expire after the configured timeout.
    pub fn ephemeral(&self) -> &EphemeralStore;
}
```

This is optional — apps can continue using SyncedStore for presence if they prefer.

---

## 5. Internal Architecture

```
truffle-core/src/
├── node.rs                     ← adds crdt_doc(), crdt_doc_with_backend(), state_dir()
├── crdt_doc/                   ← (NEW)
│   ├── mod.rs                  ← CrdtDoc, CrdtDocEvent, public API
│   ├── sync.rs                 ← Background sync task (peer lifecycle, delta exchange)
│   ├── types.rs                ← CrdtSyncMessage, CrdtDocError
│   ├── backend.rs              ← CrdtBackend trait, MemoryCrdtBackend, CrdtFileBackend
│   └── tests.rs
├── synced_store/               ← (unchanged)
├── file_transfer/              ← (unchanged)
└── ...
```

### Background sync task

Spawned at `CrdtDoc` creation. Three event sources in `tokio::select!`:

1. **Local updates** — `doc.subscribe_local_update()` fires with binary delta:
   - Persist to backend via `save_update(doc_id, &bytes)`
   - Broadcast `CrdtSyncMessage::Update { data }` to all connected peers
   - Check compaction threshold; compact if needed
   - Emit `CrdtDocEvent::LocalChange`

2. **Incoming messages** — `node.subscribe("cd:{doc_id}")`:
   - `Update`: call `doc.import(&data)`, emit `CrdtDocEvent::RemoteChange`
   - `SyncRequest`: compute delta via `doc.export(ExportMode::updates(&their_vv))`, send `SyncResponse` or `Snapshot`
   - `SyncResponse` / `Snapshot`: call `doc.import(&data)`, emit `CrdtDocEvent::PeerSynced`

3. **Peer events** — `node.on_peer_change()`:
   - `Joined`: send `SyncRequest { version_vector: our_vv }` to new peer
   - `Left`: emit `CrdtDocEvent::PeerLeft`

### Locking strategy

`LoroDoc` is internally thread-safe (behind `Arc<Mutex>` internally). However, export/import operations need a consistent view, so we wrap the doc in our own `Mutex` to serialize access between the sync task and application calls. This is the same pattern as Automerge-repo's `DocHandle`.

For high-frequency writes (e.g., real-time text editing), the auto-commit batching (`set_change_merge_interval`) reduces the frequency of sync messages. The default merge interval of 1000ms means rapid edits are batched into ~1 broadcast per second.

---

## 6. NAPI Bindings

Following the same pattern as `NapiSyncedStore`:

```rust
#[napi]
pub struct NapiCrdtDoc {
    inner: Arc<CrdtDoc>,
    task_handles: Vec<JoinHandle<()>>,
}

#[napi]
impl NapiCrdtDoc {
    // Container access — return opaque handles or JSON snapshots
    #[napi]
    pub fn get_map(&self, name: String) -> NapiCrdtMap;
    #[napi]
    pub fn get_list(&self, name: String) -> NapiCrdtList;
    #[napi]
    pub fn get_text(&self, name: String) -> NapiCrdtText;
    // ... etc

    // Document-level
    #[napi]
    pub fn get_deep_value(&self) -> serde_json::Value;
    #[napi]
    pub fn commit(&self);

    // Events
    #[napi]
    pub fn on_change(&self, callback: Function) -> Result<()>;

    // Lifecycle
    #[napi]
    pub async fn stop(&self) -> Result<()>;
}
```

**Container handles vs JSON snapshots:** The NAPI layer can expose Loro containers as opaque handles with typed methods (`map.insert(key, value)`, `text.insert(pos, str)`, `list.push(value)`), or it can expose the document as a JSON snapshot that applications read/write. The handle approach is more ergonomic for real-time editing; the snapshot approach is simpler for less interactive use cases. We implement both:

```typescript
// Handle-based (real-time editing)
const map = doc.getMap("cards");
map.insert("card-1", { title: "Task", done: false });

// Snapshot-based (read full state)
const state = doc.getDeepValue();
// state = { cards: { "card-1": { title: "Task", done: false } }, ... }
```

---

## 7. React Hook

```typescript
import { useCrdtDoc } from '@vibecook/truffle-react';

function SharedBoard({ node }: { node: NapiNode }) {
  const { doc, value, isReady } = useCrdtDoc(node, "shared-board");

  // `value` is a reactive snapshot of doc.getDeepValue()
  // Re-renders on every local or remote change.

  const addCard = useCallback((title: string) => {
    const map = doc.getMap("cards");
    const id = crypto.randomUUID();
    map.insert(id, { title, done: false });

    const order = doc.getList("card-order");
    order.push(id);
  }, [doc]);

  if (!isReady) return <div>Syncing...</div>;

  return (
    <div>
      {value.cards && Object.entries(value.cards).map(([id, card]) => (
        <Card key={id} card={card} />
      ))}
      <button onClick={() => addCard("New Card")}>Add Card</button>
    </div>
  );
}
```

**Hook signature:**

```typescript
function useCrdtDoc(
  node: NapiNode | null,
  docId: string,
): {
  doc: NapiCrdtDoc | null;
  value: Record<string, any>;  // reactive snapshot
  isReady: boolean;
};
```

The hook:
1. Creates the CrdtDoc via `node.crdtDoc(docId)` on mount
2. Subscribes to change events via `doc.onChange()`
3. On every change (local or remote): calls `doc.getDeepValue()` and updates `value` state
4. Cleans up on unmount via `doc.stop()`

**Performance consideration:** `getDeepValue()` on every change is acceptable for small-to-medium documents. For large documents with high-frequency edits, a future optimization can use Loro's `DiffEvent` to apply incremental patches to the React state instead of re-reading the full document.

---

## 8. Tauri Plugin

Following the same pattern as the existing Tauri commands:

```rust
// New commands
#[tauri::command]
async fn crdt_doc_create(app: AppHandle, doc_id: String) -> Result<(), String>;

#[tauri::command]
async fn crdt_doc_get_value(app: AppHandle, doc_id: String) -> Result<serde_json::Value, String>;

#[tauri::command]
async fn crdt_doc_map_insert(
    app: AppHandle, doc_id: String, container: String,
    key: String, value: serde_json::Value,
) -> Result<(), String>;

// ... similar commands for list, text, tree, counter operations
```

Events forwarded to frontend:
- `"truffle://crdt-change"` — document changed (local or remote)
- `"truffle://crdt-peer-synced"` — catch-up sync completed with a peer

---

## 9. Implementation Plan

### Phase 1: Core CrdtDoc with sync (truffle-core)

1. Add `loro` dependency to `truffle-core/Cargo.toml`
2. Create `crdt_doc/types.rs` — `CrdtSyncMessage`, `CrdtDocEvent`, `CrdtDocError`
3. Create `crdt_doc/backend.rs` — `CrdtBackend` trait, `MemoryCrdtBackend`
4. Create `crdt_doc/mod.rs` — `CrdtDoc` struct, container accessors, public API
5. Create `crdt_doc/sync.rs` — background sync task (local update broadcast, peer join catch-up, incoming message handling)
6. Add `Node::crdt_doc()` method
7. Expose `Node::state_dir()` accessor
8. Tests: single-node CRUD, two mock nodes syncing, version vector delta exchange, peer join catch-up, concurrent edits converging

**Estimated scope:** ~800 LOC. Depends on `loro` crate.

### Phase 2: Persistence

1. Add `CrdtFileBackend` to `crdt_doc/backend.rs`
2. Add `Node::crdt_doc_with_backend()` method
3. Implement compaction logic (threshold-based)
4. Tests: create doc, make edits, "restart" (drop + recreate), verify state restored; compaction test

**Estimated scope:** ~300 LOC.

### Phase 3: NAPI bindings

1. Add `NapiCrdtDoc` to `truffle-napi`
2. Container handle wrappers (`NapiCrdtMap`, `NapiCrdtList`, `NapiCrdtText`, etc.)
3. Event forwarding via `ThreadsafeFunction`
4. Add `NapiNode::crdt_doc()` method

**Estimated scope:** ~500 LOC.

### Phase 4: React hook

1. Add `useCrdtDoc` to `@vibecook/truffle-react`
2. Reactive snapshot with `getDeepValue()` on change
3. Tests: mount/unmount lifecycle, reactivity on remote change

**Estimated scope:** ~150 LOC.

### Phase 5: Tauri plugin

1. Add CrdtDoc commands to `truffle-tauri-plugin`
2. Event forwarding via `app.emit()`

**Estimated scope:** ~200 LOC.

### Phase 6: Advanced features (future)

- EphemeralStore integration for CRDT-aware presence
- `LoroDoc` JSON path queries (`jsonpath` feature)
- Document index (a SyncedStore listing available doc IDs per device)
- On-demand document sync (subscribe to `"cd:{doc_id}"` only when the document is opened)
- Shallow snapshots for large documents with long history
- CLI integration: `truffle crdt ls`, `truffle crdt get <doc_id>`, `truffle crdt watch <doc_id>`

---

## 10. What We Explicitly Don't Build

| Feature | Why not |
|---------|---------|
| Custom CRDT algorithms | Loro provides Text, Map, List, MovableList, Tree, Counter — comprehensive enough for any mesh app |
| Server relay | Truffle is peer-to-peer. If an app needs a persistent relay, that's an application concern |
| Conflict UI | CRDTs are conflict-free by definition. No merge dialogs needed |
| Schema validation | Applications define their own container structure. Truffle doesn't enforce schema |
| Multi-document transactions | Each document is independent. Cross-document atomicity is an application concern |
| Yjs/Automerge backends | Loro is the single CRDT engine. A future RFC can add alternatives if a concrete need arises |

---

## 11. Future Considerations

**Not in scope for this RFC, but worth noting:**

- **Document-level permissions.** Currently any peer can edit any document. A future RFC could add per-document ACLs (e.g., read-only peers, owner-only fields) using Tailscale identities as the trust anchor.

- **Selective sync.** Not all peers need all documents. A document index + subscription model would let peers declare interest and only sync documents they care about. This is how Ditto and Automerge-repo handle scale.

- **End-to-end encryption.** Loro's binary format is opaque to Truffle's transport layer. E2E encrypting the `data` field in CrdtSyncMessage before broadcast and decrypting on import would add payload-level encryption without changing the sync protocol.

- **Large document optimization.** For documents exceeding ~10MB, Loro's shallow snapshots (`ExportMode::ShallowSnapshot`) can GC old history while preserving the current state. The sync protocol would need a negotiation step to handle peers at different history depths.

- **Conflict-free file system.** `LoroTree` + `LoroMap` (per-node metadata) could model a collaborative file tree where peers create, move, and rename files/folders concurrently — a natural extension for a mesh networking library.
