# RFC 005: truffle-core Refactor Plan

**Status**: Implemented (Changes 1-3, 5-6; Change 4 skipped)
**Created**: 2026-03-08
**Implemented**: 2026-03-08
**Motivation**: Real-world validation via Cheeseboard (RFC 004) exposed API gaps and dead code in truffle-core. This RFC captures all validated changes needed in the library itself.

---

## Summary

The Cheeseboard architecture review (RFC 004) identified 8 issues in truffle-core's public APIs. After validation against the actual source code (186 passing tests, 13k LOC across 3 crates), this RFC defines 6 changes to implement, in dependency order.

Two proposed changes were rejected during validation:
- **TruffleRuntime builder** — too application-specific; replaced by standalone integration helpers
- **Tauri plugin update for TruffleRuntime** — dependent on rejected change

---

## Changes

### Change 1: Document SyncableStore async-safety

**Priority**: Group A (can parallelize)
**Effort**: Trivial
**Breaking**: No

**Problem**: The `SyncableStore` trait uses `&self` (not async). All methods are synchronous, but `StoreSyncAdapter` calls them from async contexts. If an implementor uses `tokio::sync::RwLock` internally, calling `.blocking_read()` inside a tokio runtime will panic or deadlock.

**File**: `crates/truffle-core/src/store_sync/adapter.rs` lines 9-28

**Current code**:
```rust
pub trait SyncableStore: Send + Sync {
    fn store_id(&self) -> &str;
    fn get_local_slice(&self) -> Option<DeviceSlice>;
    fn apply_remote_slice(&self, slice: DeviceSlice);
    fn remove_remote_slice(&self, device_id: &str, reason: &str);
    fn clear_remote_slices(&self);
}
```

**Change**: Add doc comments warning implementors to use `std::sync` locks, not `tokio::sync`:

```rust
/// Trait that stores must implement to be syncable.
///
/// # Async Safety
///
/// All methods use `&self` and are called synchronously from within async
/// contexts (inside `StoreSyncAdapter`). Implementations MUST use
/// `std::sync::RwLock` or `std::sync::Mutex` for interior mutability,
/// NOT `tokio::sync` variants. Using `tokio::sync::RwLock::blocking_read()`
/// inside a tokio runtime context will panic or deadlock.
///
/// The existing `MockStore` in tests demonstrates the correct pattern
/// using `std::sync::Mutex`.
pub trait SyncableStore: Send + Sync {
```

**Tests**: No new tests needed. Existing tests already use `std::sync::Mutex`.

---

### Change 2: Fix `handle_bus_message` signature

**Priority**: Group A (can parallelize)
**Effort**: Small
**Breaking**: Yes — `FileTransferAdapter::handle_bus_message()` public API changes

**Problem**: `FileTransferAdapter::handle_bus_message()` takes `payload: &str`, but callers typically have `serde_json::Value` (from `IncomingMeshMessage::payload`). This forces an unnecessary `serde_json::to_string()` → `serde_json::from_str()` round-trip on every message.

**File**: `crates/truffle-core/src/file_transfer/adapter.rs` line 266

**Current signature**:
```rust
pub async fn handle_bus_message(&self, msg_type: &str, payload: &str)
```

**New signature**:
```rust
pub async fn handle_bus_message(&self, msg_type: &str, payload: &serde_json::Value)
```

**Internal changes**: Replace `serde_json::from_str::<T>(payload)` with `serde_json::from_value::<T>(payload.clone())` for each match arm (OFFER, ACCEPT, REJECT, CANCEL at lines 268-290).

**Downstream impact**:
- **truffle-napi** (`crates/truffle-napi/src/file_transfer.rs`): Currently passes `payload: String` from JS. Must change to deserialize the JS string into `serde_json::Value` first, then pass the reference.
- **truffle-tauri-plugin**: Not currently using `handle_bus_message` directly.

**Tests**: Update any tests calling `handle_bus_message` with string payloads to pass `serde_json::Value` instead.

---

### Change 3: Auto-dispatch incoming messages to MessageBus

**Priority**: Group A (can parallelize)
**Effort**: Medium
**Breaking**: No (additive)

**Problem**: `MeshMessageBus` is created in `MeshNode::new()` (line 150) and exposed via `message_bus()` (line 388-390), but `dispatch()` is never called. Incoming messages go to `event_tx` as `MeshNodeEvent::Message` — the MessageBus pub/sub layer is completely dead code. Every consumer must manually build their own dispatch logic by reading `event_rx` and routing by namespace.

**File**: `crates/truffle-core/src/mesh/node.rs` lines 1002-1009 and 1020-1028

**Current flow**:
```
incoming WebSocket message → parse MeshEnvelope → event_tx.try_send(MeshNodeEvent::Message(...))
```

**New flow**:
```
incoming WebSocket message → parse MeshEnvelope → {
    event_tx.try_send(MeshNodeEvent::Message(...));  // keep existing behavior
    message_bus.dispatch(&BusMessage { ... });        // NEW: also dispatch to pub/sub
}
```

**Implementation**: In the transport event loop (inside `start_event_loop()`), after each `event_tx.try_send(MeshNodeEvent::Message(...))` call, add:

```rust
// Also dispatch to MessageBus for pub/sub subscribers
let bus_msg = BusMessage {
    from: Some(from_device_id.clone()),
    namespace: envelope.namespace.clone(),
    msg_type: envelope.msg_type.clone(),
    payload: envelope.payload.clone(),
};
this_message_bus.dispatch(&bus_msg).await;
```

This must be done at two locations:
1. **Line 1003** — Local delivery (routed messages to local device)
2. **Line 1022** — Direct application-level messages

**Consideration**: The event loop runs in a spawned task. `message_bus` is already stored as a field on `MeshNode` and accessible via `Arc`. The spawned task needs a clone of `Arc<MeshMessageBus>` — need to check if `message_bus` is stored as `Arc` or inline. Currently it's inline in `MeshNode` (line 150: `let message_bus = MeshMessageBus::new(...)`). May need to wrap in `Arc` for the spawned task to reference it.

**Tests**: Add test that subscribes to a namespace on the MessageBus and verifies messages are dispatched when received through the transport layer.

---

### Change 4: Add change notification to StoreSyncAdapter

**Priority**: Group A (can parallelize)
**Effort**: Medium
**Breaking**: No (additive)

**Problem**: `StoreSyncAdapter` requires the application to call `handle_local_changed(store_id, slice)` manually after every local mutation. There's no built-in mechanism for a store to signal that it has changed. This is error-prone — forgetting to call it means changes are never broadcast.

**File**: `crates/truffle-core/src/store_sync/adapter.rs`

**Change**: Add a `start_change_listener()` method that spawns a background task polling registered stores for changes via a notification channel.

```rust
impl StoreSyncAdapter {
    /// Start listening for local store changes via a notification channel.
    ///
    /// The returned `mpsc::UnboundedSender<String>` accepts store IDs.
    /// When a store ID is sent, the adapter will call `get_local_slice()`
    /// on that store and broadcast the update if changed.
    ///
    /// This is an alternative to calling `handle_local_changed()` directly.
    pub fn start_change_listener(
        self: &Arc<Self>,
    ) -> mpsc::UnboundedSender<String> {
        let (change_tx, mut change_rx) = mpsc::unbounded_channel::<String>();
        let adapter = Arc::clone(self);
        tokio::spawn(async move {
            while let Some(store_id) = change_rx.recv().await {
                let stores = adapter.stores.read().await;
                if let Some(store) = stores.get(&store_id) {
                    if let Some(slice) = store.get_local_slice() {
                        adapter.handle_local_changed(&store_id, slice).await;
                    }
                }
            }
        });
        change_tx
    }
}
```

**Usage**: Application stores call `change_tx.send("clipboard-history".to_string())` after mutations. The adapter handles the rest.

**Tests**: Test that sending a store ID through the channel triggers `handle_local_changed()` and produces an outgoing sync message.

---

### Change 5: Change `event_tx` from mpsc to broadcast

**Priority**: Group B (after Group A)
**Effort**: Large
**Breaking**: Yes — `MeshNode::new()` return type changes

**Problem**: `MeshNode::new()` returns `mpsc::Receiver<MeshNodeEvent>` — a single-consumer channel. If an app needs multiple subsystems consuming events (e.g., StoreSyncAdapter, FileTransferAdapter, UI layer), it must manually clone and fan out. With `broadcast`, each subscriber gets its own receiver.

**Prerequisite**: `MeshNodeEvent` must implement `Clone`. Check current derive:

**File**: `crates/truffle-core/src/mesh/node.rs` line 121

**Current**:
```rust
let (event_tx, event_rx) = mpsc::channel(256);
```

**New**:
```rust
let (event_tx, _) = broadcast::channel(256);
// Don't return a receiver from new() — callers use subscribe()
```

**API change on MeshNode**:
```rust
// OLD
pub fn new(config, conn_mgr) -> (Self, mpsc::Receiver<MeshNodeEvent>)

// NEW
pub fn new(config, conn_mgr) -> Self

// NEW method
pub fn subscribe_events(&self) -> broadcast::Receiver<MeshNodeEvent>
```

**Or** keep the tuple return but change the type:
```rust
pub fn new(config, conn_mgr) -> (Self, broadcast::Receiver<MeshNodeEvent>)
```

The second approach is less disruptive. Additional receivers can be obtained via `mesh_node.subscribe_events()`.

**Internal changes**:
- Replace `event_tx.try_send()` with `event_tx.send()` (broadcast returns `Result`, not `Option`)
- `broadcast::send()` returns `Err` only if there are no receivers — handle gracefully
- Add `pub fn subscribe_events(&self) -> broadcast::Receiver<MeshNodeEvent>` method

**Downstream impact**:

**truffle-napi** (`crates/truffle-napi/src/mesh_node.rs` line 27):
```rust
// OLD
event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<MeshNodeEvent>>>,

// NEW
event_rx: tokio::sync::Mutex<Option<broadcast::Receiver<MeshNodeEvent>>>,
```
The event loop (line 240) must change from `while let Some(event) = rx.recv().await` to:
```rust
loop {
    match rx.recv().await {
        Ok(event) => { /* handle */ }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!("Event receiver lagged by {n} messages");
        }
        Err(broadcast::error::RecvError::Closed) => break,
    }
}
```

**truffle-tauri-plugin** (`crates/truffle-tauri-plugin/src/lib.rs` line 19):
- `TruffleState` stores `RwLock<Option<Arc<MeshNode>>>` but does NOT consume `event_rx` itself — the app is responsible. Minimal changes needed: just adjust how the app creates its receiver.

**Tests**: Update all tests that use `event_rx.recv().await` to handle `Result<T, RecvError>`. Verify that multiple subscribers each receive all events.

---

### Change 6: Integration helpers module

**Priority**: Group C (after Change 5)
**Effort**: Medium
**Breaking**: No (new module)

**Problem**: Wiring `StoreSyncAdapter` and `FileTransferAdapter` to `MeshNode` requires identical boilerplate in every application (Cheeseboard, vibe-ctl, etc.): spawn pump tasks for outgoing messages, subscribe to the event broadcast, dispatch incoming messages to the right adapter. This is the #1 source of "MISSING" items in RFC 004.

**Rejected alternative**: Adding `connect_to_mesh()` methods on each adapter would couple the adapter layer to `MeshNode`, breaking the transport-agnostic design.

**Solution**: Create a new `truffle_core::integration` module with standalone helper functions.

**New file**: `crates/truffle-core/src/integration.rs`

```rust
//! Integration helpers for wiring truffle-core components together.
//!
//! These functions handle the boilerplate of connecting StoreSyncAdapter
//! and FileTransferAdapter to a MeshNode's event broadcast and message
//! routing. They spawn background tasks that run until the MeshNode
//! or adapter is dropped.

use std::sync::Arc;
use tokio::sync::broadcast;
use crate::mesh::node::{MeshNode, MeshNodeEvent};
use crate::store_sync::adapter::StoreSyncAdapter;
use crate::store_sync::types::OutgoingSyncMessage;
use crate::file_transfer::adapter::FileTransferAdapter;

/// Wire a StoreSyncAdapter to a MeshNode.
///
/// Spawns two background tasks:
/// 1. **Incoming pump**: Subscribes to MeshNode events, routes `sync` namespace
///    messages to the adapter, and handles device discovery/offline events.
/// 2. **Outgoing pump**: Reads from the adapter's outgoing channel and
///    broadcasts via MeshNode.
///
/// Returns a `JoinHandle` pair for the spawned tasks.
pub fn wire_store_sync(
    mesh_node: &Arc<MeshNode>,
    adapter: &Arc<StoreSyncAdapter>,
    outgoing_rx: tokio::sync::mpsc::UnboundedReceiver<OutgoingSyncMessage>,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let node = Arc::clone(mesh_node);
    let adapt = Arc::clone(adapter);
    let mut event_rx = node.subscribe_events();

    // Incoming: route sync messages and device events to adapter
    let incoming_handle = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(MeshNodeEvent::Message(msg)) if msg.namespace == "sync" => {
                    // Route to StoreSyncAdapter
                    adapt.handle_sync_message_from_mesh(&msg).await;
                }
                Ok(MeshNodeEvent::DeviceDiscovered(device)) => {
                    adapt.handle_device_discovered(&device.id).await;
                }
                Ok(MeshNodeEvent::DeviceOffline(id)) => {
                    adapt.handle_device_offline(&id).await;
                }
                Ok(_) => {} // ignore non-sync events
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Store sync event receiver lagged by {n}");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Outgoing: pump adapter messages to MeshNode
    let node2 = Arc::clone(mesh_node);
    let outgoing_handle = tokio::spawn(async move {
        let mut rx = outgoing_rx;
        while let Some(msg) = rx.recv().await {
            node2.broadcast_sync_message(&msg).await;
        }
    });

    (incoming_handle, outgoing_handle)
}

/// Wire a FileTransferAdapter to a MeshNode.
///
/// Similar to `wire_store_sync()` but routes `file-transfer` namespace
/// messages and pumps outgoing targeted messages (not broadcasts).
pub fn wire_file_transfer(
    mesh_node: &Arc<MeshNode>,
    adapter: &Arc<FileTransferAdapter>,
    outgoing_rx: tokio::sync::mpsc::UnboundedReceiver<(String, String, String)>,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let node = Arc::clone(mesh_node);
    let adapt = Arc::clone(adapter);
    let mut event_rx = node.subscribe_events();

    let incoming_handle = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(MeshNodeEvent::Message(msg)) if msg.namespace == "file-transfer" => {
                    adapt.handle_bus_message(&msg.msg_type, &msg.payload).await;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("File transfer event receiver lagged by {n}");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let node2 = Arc::clone(mesh_node);
    let outgoing_handle = tokio::spawn(async move {
        let mut rx = outgoing_rx;
        while let Some((target_id, msg_type, payload_json)) = rx.recv().await {
            let payload: serde_json::Value = serde_json::from_str(&payload_json)
                .unwrap_or(serde_json::Value::Null);
            node2.send_to_device(&target_id, "file-transfer", &msg_type, payload).await;
        }
    });

    (incoming_handle, outgoing_handle)
}
```

**Note**: The exact method names on `MeshNode` (`broadcast_sync_message`, `send_to_device`, `subscribe_events`) and `StoreSyncAdapter` (`handle_sync_message_from_mesh`) may need adjustment during implementation based on actual API review. The integration helpers depend on Change 5 (broadcast) being complete.

**Register module**: Add `pub mod integration;` to `crates/truffle-core/src/lib.rs`.

**Tests**: Integration tests that create a MeshNode + StoreSyncAdapter, wire them together, and verify end-to-end message flow.

---

## Rejected Changes

### TruffleRuntime Builder (originally Change 1)

**Rejected because**: A unified builder that creates MeshNode + GoShim + BridgeManager + ConnectionManager + StoreSyncAdapter + FileTransferAdapter is too application-specific. Each app (vibe-ctl, Cheeseboard) has different requirements for which components to include and how to configure them. The standalone integration helpers (Change 6) solve the actual pain point without over-abstracting.

### Update truffle-tauri-plugin for TruffleRuntime (originally Change 10)

**Rejected because**: Dependent on TruffleRuntime builder which was rejected.

---

## Implementation Order

```
Group A (parallel, no dependencies):
├── Change 1: Document SyncableStore async-safety
├── Change 2: Fix handle_bus_message signature
├── Change 3: Auto-dispatch to MessageBus
└── Change 4: Add change notification to StoreSyncAdapter

Group B (depends on Group A):
└── Change 5: mpsc → broadcast for event_tx

Group C (depends on Change 5):
└── Change 6: Integration helpers module
```

**Critical path**: Group A → Change 5 → Change 6

---

## Breaking Changes Summary

| Change | Old API | New API | Affected Crates |
|--------|---------|---------|-----------------|
| 2 | `handle_bus_message(&self, &str, &str)` | `handle_bus_message(&self, &str, &serde_json::Value)` | truffle-napi |
| 5 | `MeshNode::new() -> (Self, mpsc::Receiver)` | `MeshNode::new() -> (Self, broadcast::Receiver)` + `subscribe_events()` | truffle-napi, truffle-tauri-plugin |

---

## Test Plan

All 186 existing tests must continue to pass after each change. New tests per change:

1. **Change 1**: No new tests (documentation only)
2. **Change 2**: Update existing `handle_bus_message` tests to use `serde_json::Value`
3. **Change 3**: Test MessageBus dispatch on incoming transport messages
4. **Change 4**: Test `start_change_listener()` triggers sync broadcast
5. **Change 5**: Test multiple broadcast subscribers, test `Lagged` handling
6. **Change 6**: Integration test for `wire_store_sync()` and `wire_file_transfer()` end-to-end flow

---

## Post-Refactor

Once this refactor is complete:
- Cheeseboard (RFC 004) can be implemented against the improved APIs
- The integration helpers eliminate ~100 lines of boilerplate per consumer app
- MessageBus becomes actually functional (no longer dead code)
- Multiple event consumers are natively supported via broadcast
