# RFC 007: Comprehensive Refactor Plan for truffle-core

**Status**: Implemented
**Created**: 2026-03-17
**Implemented**: 2026-03-18
**Depends on**: RFC 006 (Audit Findings)

---

## Overview

This RFC specifies an ordered sequence of 10 atomic changesets that address all 23 issues from RFC 006. Each changeset is independently testable. The changesets are organized into a dependency graph so that teams can parallelize where possible.

Total scope: ~14 files modified, ~2 new files, ~30 new tests, 0 breaking public API changes (NAPI bindings remain compatible).

> **Implementation Note (2026-03-18)**: All 10 changesets have been implemented. See the "Post-Implementation API Summary" section at the end of this document for a consolidated view of all public API changes. The code snippets in each changeset section below reflect the original proposals and may differ slightly from the final implementation (e.g., `crate::mesh::node::current_timestamp_ms()` in proposals became `crate::util::current_timestamp_ms()` in the actual code).

---

## Dependency Graph

```
CS-1 (serde fixes)                       [BUG-2, ARCH-13]
  |
CS-2 (election redesign)                 [BUG-3, BUG-4]
  |
CS-3 (dial pipeline unification)         [BUG-1, BUG-9]
  |
CS-4 (shutdown + goodbye)                [BUG-5, BUG-6, BUG-7, M-3]
  |
CS-5 (event loop extraction)             [ARCH-1, ARCH-2, ARCH-14]
  |
CS-6 (MeshNode state consolidation)      [ARCH-3, ARCH-11]
  |
CS-7 (runtime.rs extraction)             [ARCH-4, ARCH-8]
  |
CS-8 (event drop logging + channel)      [ARCH-6, ARCH-10]
  |
CS-9 (code quality sweep)                [ARCH-5, ARCH-7, ARCH-9, ARCH-12]
  |
CS-10 (sidecar robustness)               [BUG-8, M-2]
```

Parallelization opportunities:
- CS-1 and CS-2 can run in parallel (no shared files).
- CS-8 and CS-9 can run in parallel.
- CS-10 is independent of CS-8/CS-9 but depends on CS-7.

---

## Changeset 1: Serde Case Fixes + TailnetPeer Deduplication

**Issues**: BUG-2, ARCH-13
**Risk**: LOW (additive aliases, no logic change)
**Files to modify**:
- `crates/truffle-core/src/types/mod.rs`
- `crates/truffle-core/src/bridge/protocol.rs`
- `crates/truffle-core/src/mesh/device.rs`

### Problem

`#[serde(rename_all = "camelCase")]` transforms `tailscale_ip` to `tailscaleIp`, but Go/TS send `tailscaleIP` (uppercase acronym). Same for `tailscale_dns_name` -> `tailscaleDnsName` vs `tailscaleDNSName`, and `tailscale_ips` -> `tailscaleIps` vs `tailscaleIPs`.

Additionally, `TailnetPeer` is defined in both `types/mod.rs` (with `os: Option<String>`) and `bridge/protocol.rs` (with `os: String`). The protocol version deserializes from Go, so it needs `String`; the types version is used by DeviceManager, so it uses `Option<String>`.

### Changes

#### 1a. Fix `BaseDevice` in `types/mod.rs`

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseDevice {
    pub id: String,
    #[serde(rename = "type")]
    pub device_type: String,
    pub name: String,
    pub tailscale_hostname: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "tailscaleDNSName"
    )]
    pub tailscale_dns_name: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "tailscaleIP"
    )]
    pub tailscale_ip: Option<String>,
    // ... remaining fields unchanged
}
```

#### 1b. Fix `TailnetPeer` in `types/mod.rs`

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TailnetPeer {
    pub id: String,
    pub hostname: String,
    #[serde(alias = "dnsName")]
    pub dns_name: String,
    #[serde(rename = "tailscaleIPs", alias = "tailscaleIps")]
    pub tailscale_ips: Vec<String>,
    pub online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
}
```

#### 1c. Eliminate duplicate `TailnetPeer` in `bridge/protocol.rs`

Delete `TailnetPeer` from `bridge/protocol.rs`. Update `PeersEventData` to use the canonical type:

```rust
use crate::types::TailnetPeer as CanonicalTailnetPeer;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeersEventData {
    pub peers: Vec<BridgeTailnetPeer>,
}

/// Wire-format peer from Go sidecar. Converted to canonical TailnetPeer by the shim.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeTailnetPeer {
    pub id: String,
    pub hostname: String,
    pub dns_name: String,
    #[serde(rename = "tailscaleIPs")]
    pub tailscale_ips: Vec<String>,
    pub online: bool,
    #[serde(default)]
    pub os: String,
}

impl BridgeTailnetPeer {
    pub fn to_canonical(&self) -> CanonicalTailnetPeer {
        CanonicalTailnetPeer {
            id: self.id.clone(),
            hostname: self.hostname.clone(),
            dns_name: self.dns_name.clone(),
            tailscale_ips: self.tailscale_ips.clone(),
            online: self.online,
            os: if self.os.is_empty() { None } else { Some(self.os.clone()) },
        }
    }
}
```

#### 1d. Update NAPI peer conversion in `napi/mesh_node.rs`

Remove the manual `TailnetPeer` construction in the lifecycle event handler (lines 272-287). Instead call `bridge_peer.to_canonical()`.

### New Tests

```rust
#[test]
fn deserialize_base_device_with_acronym_casing() {
    let json = r#"{
        "id":"d1","type":"desktop","name":"D",
        "tailscaleHostname":"h",
        "tailscaleDNSName":"d.ts.net",
        "tailscaleIP":"100.64.0.1",
        "status":"online","capabilities":[]
    }"#;
    let dev: BaseDevice = serde_json::from_str(json).unwrap();
    assert_eq!(dev.tailscale_dns_name, Some("d.ts.net".into()));
    assert_eq!(dev.tailscale_ip, Some("100.64.0.1".into()));
}

#[test]
fn deserialize_tailnet_peer_from_go_format() {
    let json = r#"{
        "id":"1","hostname":"h","dnsName":"h.ts.net",
        "tailscaleIPs":["100.64.0.2"],"online":true,"os":"linux"
    }"#;
    let peer: BridgeTailnetPeer = serde_json::from_str(json).unwrap();
    let canonical = peer.to_canonical();
    assert_eq!(canonical.os, Some("linux".into()));
    assert_eq!(canonical.tailscale_ips, vec!["100.64.0.2"]);
}
```

### Verification

```bash
cargo test -p truffle-core -- serde
cargo test -p truffle-core -- tailnet_peer
```

---

## Changeset 2: Election Redesign (Local-Only Events)

**Issues**: BUG-3, BUG-4
**Risk**: MEDIUM (changes state machine semantics)
**Files to modify**:
- `crates/truffle-core/src/mesh/election.rs`
- `crates/truffle-core/src/mesh/node.rs` (election event handler)

### Problem

1. **BUG-3**: `election:timeout` is sent as `ElectionEvent::Broadcast`, which causes it to be forwarded to all peers. Peers call `decide_election()` on receipt, causing double/conflicting decisions. In TS, `setTimeout(() => this.decideElection())` is a local-only call.

2. **BUG-4**: `handle_primary_lost()` spawns an async task that tries to broadcast `election:start` and `election:candidate` directly. But the task cannot access `&mut self`, so `self.phase` stays `Waiting` and `self.candidates` is never populated. The TS version calls `this.startElection()` directly from the timeout.

### Changes

#### 2a. Add new `ElectionEvent` variants

```rust
#[derive(Debug, Clone)]
pub enum ElectionEvent {
    ElectionStarted,
    PrimaryElected { device_id: String, is_local: bool },
    PrimaryLost { previous_primary_id: String },
    /// Mesh messages to broadcast to all peers.
    Broadcast(MeshMessage),
    /// LOCAL ONLY: Grace period expired, MeshNode should call start_election().
    GracePeriodExpired,
    /// LOCAL ONLY: Election timeout expired, MeshNode should call decide_election().
    DecideNow,
}
```

#### 2b. Rewrite `handle_primary_lost()` in `election.rs`

Replace the spawned task that tries to broadcast directly. Instead, spawn a task that only sends `GracePeriodExpired` after the grace period:

```rust
pub fn handle_primary_lost(&mut self, previous_primary_id: &str) {
    let _config = match self.config.clone() {
        Some(c) => c,
        None => return,
    };

    tracing::info!("Primary lost: {previous_primary_id}");
    self.primary_id = None;
    self.emit(ElectionEvent::PrimaryLost {
        previous_primary_id: previous_primary_id.to_string(),
    });

    self.cancel_grace_period();
    self.phase = ElectionPhase::Waiting;

    let grace_duration = self.timing.primary_loss_grace;
    let event_tx = self.event_tx.clone();

    let handle = tokio::spawn(async move {
        tokio::time::sleep(grace_duration).await;
        tracing::info!("Grace period expired, requesting election start");
        let _ = event_tx.send(ElectionEvent::GracePeriodExpired).await;
    });

    self.grace_period_handle = Some(handle.abort_handle());
}
```

#### 2c. Rewrite timeout in `start_election()` and `handle_election_start()`

Replace the `election:timeout` broadcast with `DecideNow`:

```rust
// In start_election() - replace the timeout task:
let handle = tokio::spawn(async move {
    tokio::time::sleep(timeout_duration).await;
    let _ = event_tx.send(ElectionEvent::DecideNow).await;
});
self.election_timeout_handle = Some(handle.abort_handle());
```

Same change in `handle_election_start()`.

#### 2d. Handle new events in node.rs election event handler

In `start_event_loop()`, the election event handler task currently has:

```rust
ElectionEvent::Broadcast(message) => { /* send to all peers */ }
```

Add handlers for the new variants:

```rust
ElectionEvent::GracePeriodExpired => {
    tracing::info!("Grace period expired, starting election");
    let mut el = election_for_events.write().await;
    el.start_election();
}
ElectionEvent::DecideNow => {
    tracing::info!("Election timeout, deciding");
    let mut el = election_for_events.write().await;
    el.decide_election();
}
```

#### 2e. Remove `election:timeout` from the transport event loop

In the inline `match mesh_msg.msg_type.as_str()` block (node.rs line ~1028-1032), **delete** the `"election:timeout"` arm entirely. This message type should never appear on the wire.

### New Tests

```rust
#[tokio::test]
async fn grace_period_sends_local_event() {
    let (mut election, mut rx) = make_election();
    election.configure(config("dev-1", 1000, false));
    election.set_primary("dev-2");
    election.handle_primary_lost("dev-2");
    assert_eq!(election.phase(), ElectionPhase::Waiting);

    // Fast-forward: the spawned task will send GracePeriodExpired
    // (in tests, use tokio::time::advance)
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::task::yield_now().await;

    let mut found = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, ElectionEvent::GracePeriodExpired) {
            found = true;
        }
    }
    assert!(found, "should receive GracePeriodExpired, not Broadcast");
}

#[tokio::test]
async fn start_election_sends_decide_now_not_broadcast_timeout() {
    let (mut election, mut rx) = make_election();
    election.configure(config("dev-1", 1000, false));
    election.start_election();

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(4)).await;
    tokio::task::yield_now().await;

    let mut found_decide = false;
    let mut found_timeout_broadcast = false;
    while let Ok(event) = rx.try_recv() {
        match &event {
            ElectionEvent::DecideNow => found_decide = true,
            ElectionEvent::Broadcast(msg) if msg.msg_type == "election:timeout" => {
                found_timeout_broadcast = true;
            }
            _ => {}
        }
    }
    assert!(found_decide, "should emit DecideNow");
    assert!(!found_timeout_broadcast, "must NOT broadcast election:timeout");
}
```

### Verification

```bash
cargo test -p truffle-core -- election
```

---

## Changeset 3: Unify Dial Pipeline (Fix Outgoing Connections)

**Issues**: BUG-1, BUG-9
**Risk**: HIGH (most critical fix, changes connection establishment flow)
**Files to modify**:
- `crates/truffle-core/src/bridge/shim.rs`
- `crates/truffle-core/src/bridge/manager.rs`
- `crates/truffle-napi/src/mesh_node.rs`

### Problem

**BUG-1**: `GoShim` has `pending_dials: HashMap<String, oneshot::Sender<TcpStream>>` and `BridgeManager` has `pending_dials: HashMap<String, oneshot::Sender<BridgeConnection>>`. When Go dials and bridges back, `BridgeManager` looks in its own map (which is empty) and drops the connection as "unknown outgoing request_id".

**BUG-9**: `GoShim.dial()` inserts into its own `pending_dials` but the NAPI layer discards the `oneshot::Receiver<TcpStream>`. Entries leak.

### Solution

Eliminate `GoShim::pending_dials` entirely. The caller (NAPI or runtime.rs in CS-7) is responsible for:
1. Inserting into `BridgeManager::pending_dials` before calling `GoShim::dial()`.
2. Awaiting the `oneshot::Receiver<BridgeConnection>`.
3. Passing the `BridgeConnection` to `ConnectionManager::handle_outgoing()`.

### Changes

#### 3a. Simplify `GoShim::dial()` -- remove pending_dials

```rust
impl GoShim {
    /// Request the Go shim to dial a remote peer.
    ///
    /// Returns the request_id. The caller must insert into BridgeManager::pending_dials
    /// BEFORE calling this, using the returned request_id.
    pub async fn dial_raw(
        &self,
        target: String,
        port: u16,
        request_id: String,
    ) -> Result<(), ShimError> {
        let data = DialCommandData {
            request_id,
            target,
            port,
        };
        let cmd = ShimCommand {
            command: command_type::DIAL,
            data: Some(serde_json::to_value(&data)?),
        };
        self.send_command(cmd).await
    }
}
```

Remove `pending_dials` field from `GoShim` struct. Remove the `pending_dials()` accessor. Remove the pending_dials parameter from `spawn_manager_task` and `handle_event`. Remove the drain logic from the crash recovery path (it now only exists in BridgeManager).

The `dial_result` event handler in `handle_event` for failed dials: this now needs access to BridgeManager's pending_dials. We pass it through or use a callback:

```rust
// In handle_event, for DIAL_RESULT failure:
// The shim no longer holds pending_dials, so dial failures
// are reported via a ShimLifecycleEvent.
ShimLifecycleEvent::DialFailed {
    request_id: String,
    error: String,
}
```

#### 3b. Add `DialFailed` to `ShimLifecycleEvent`

```rust
pub enum ShimLifecycleEvent {
    Started,
    Crashed { exit_code: Option<i32>, stderr_tail: String },
    AuthRequired { auth_url: String },
    Status(super::protocol::StatusEventData),
    Peers(super::protocol::PeersEventData),
    Stopped,
    /// A dial request failed. Caller should remove from BridgeManager::pending_dials.
    DialFailed { request_id: String, error: String },
}
```

#### 3c. Create `dial_peer()` helper for the orchestration layer

This will live in `runtime.rs` (CS-7) but for now we implement it inline in `napi/mesh_node.rs`:

```rust
/// Dial a peer through the bridge pipeline.
///
/// 1. Insert pending dial in BridgeManager
/// 2. Tell Go shim to dial
/// 3. Await BridgeConnection
/// 4. Pass to ConnectionManager::handle_outgoing()
async fn dial_peer(
    bridge_pending: &Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>>,
    shim: &GoShim,
    connection_manager: &Arc<ConnectionManager>,
    target_dns: &str,
    port: u16,
    timeout: Duration,
) -> Result<(), String> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();

    // Step 1: Insert BEFORE dial command (prevents race)
    {
        let mut dials = bridge_pending.lock().await;
        dials.insert(request_id.clone(), tx);
    }

    // Step 2: Tell Go to dial
    if let Err(e) = shim.dial_raw(target_dns.to_string(), port, request_id.clone()).await {
        let mut dials = bridge_pending.lock().await;
        dials.remove(&request_id);
        return Err(format!("dial command failed: {e}"));
    }

    // Step 3: Await bridge connection with timeout
    let bridge_conn = match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(_)) => {
            return Err("dial cancelled (sender dropped)".into());
        }
        Err(_) => {
            let mut dials = bridge_pending.lock().await;
            dials.remove(&request_id);
            return Err(format!("dial timed out after {timeout:?}"));
        }
    };

    // Step 4: Upgrade to WebSocket
    connection_manager.handle_outgoing(bridge_conn).await;
    Ok(())
}
```

#### 3d. Update NAPI peer dialing (lines 296-322)

Replace the current code that calls `shim.dial()` and discards the receiver:

```rust
// In the Peers lifecycle handler:
for peer in &peers_data.peers {
    // ... existing filtering ...
    let dns = peer.dns_name.clone();
    let bridge_pending = bridge_pending_dials.clone();
    let shim_ref = shim_handle.clone();
    let conn_mgr = connection_manager_for_dial.clone();

    // Spawn each dial as an independent task (don't hold shim lock)
    tokio::spawn(async move {
        let shim_guard = shim_ref.lock().await;
        if let Some(ref shim) = *shim_guard {
            match dial_peer(
                &bridge_pending,
                shim,
                &conn_mgr,
                &dns,
                443,
                Duration::from_secs(10), // M-2: 10s timeout
            ).await {
                Ok(()) => tracing::info!("Connected to {dns}"),
                Err(e) => tracing::warn!("Failed to dial {dns}: {e}"),
            }
        }
    });
}
```

This also fixes **ARCH-8** (lock held across network I/O) because we clone the shim Arc and release it after getting the reference, and each dial runs in its own task.

#### 3e. Handle `DialFailed` lifecycle events

In the lifecycle event handler, clean up BridgeManager's pending_dials:

```rust
ShimLifecycleEvent::DialFailed { request_id, error } => {
    tracing::warn!("Dial failed: request_id={request_id}: {error}");
    let mut dials = bridge_pending_dials.lock().await;
    dials.remove(&request_id);
}
```

### New Tests

```rust
#[tokio::test]
async fn dial_pipeline_inserts_into_bridge_pending_dials() {
    // Test that calling dial_peer inserts into BridgeManager's pending_dials
    // and that when Go bridges back, the connection arrives via ConnectionManager.
    // (Integration test using BridgeManager::bind + synthetic header)
}

#[tokio::test]
async fn dial_timeout_cleans_up_pending() {
    // Test that a dial timeout removes the entry from pending_dials
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    // ... insert, let timeout fire, verify map is empty
}
```

### Verification

1. Unit tests pass: `cargo test -p truffle-core -- dial`
2. Manual: Start two truffle nodes on the same tailnet. Verify outgoing connections establish successfully (both directions).

---

## Changeset 4: Shutdown Cleanup

**Issues**: BUG-5, BUG-6, BUG-7, M-3
**Risk**: LOW
**Files to modify**:
- `crates/truffle-core/src/mesh/node.rs`

### Changes

#### 4a. BUG-6 + M-3: Call `close_all()` on stop

In `MeshNode::stop()`, add connection cleanup:

```rust
pub async fn stop(&self) {
    let is_running = *self.running.read().await;
    if !is_running {
        return;
    }
    tracing::info!("MeshNode stopping");

    // Stop announce interval
    self.stop_announce_interval().await;

    // Broadcast goodbye and wait for delivery
    self.broadcast_goodbye().await;

    // Give write pumps time to flush goodbye messages (BUG-7)
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Close all connections (BUG-6 / M-3)
    self.connection_manager.close_all().await;

    // Stop event loop AFTER connections are closed
    {
        let mut handle = self.event_loop_handle.write().await;
        if let Some(h) = handle.take() {
            h.abort();
        }
    }

    // ... rest of cleanup unchanged
}
```

#### 4b. BUG-5: Broadcast device list after election win

In the election event handler, replace the TODO comment:

```rust
ElectionEvent::PrimaryElected { device_id, is_local } => {
    // ... existing role update code ...

    if is_local {
        // BUG-5: Broadcast device list as new primary
        let local_device = device_manager_el.read().await.local_device().clone();
        let devices = {
            let dm = device_manager_el.read().await;
            let mut devs = vec![local_device];
            devs.extend(dm.devices());
            devs
        };
        let config_id = config_el.device_id.clone();
        let list_msg = MeshMessage::new(
            "device:list",
            &config_id,
            serde_json::to_value(&DeviceListPayload {
                devices,
                primary_id: config_id,
            }).unwrap_or_default(),
        );
        let envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "message".to_string(),
            payload: serde_json::to_value(&list_msg).unwrap_or_default(),
            timestamp: Some(current_timestamp_ms()),
        };
        if let Ok(value) = serde_json::to_value(&envelope) {
            let conns = connection_manager_el.get_connections().await;
            for conn in &conns {
                if conn.status == ConnectionStatus::Connected {
                    let _ = connection_manager_el.send(&conn.id, &value).await;
                }
            }
        }
    }
}
```

### New Tests

```rust
#[tokio::test]
async fn stop_calls_close_all() {
    // Verify that after stop(), get_connections() returns empty
    let (node, mut rx) = create_test_node();
    node.start().await;
    let _ = rx.recv().await; // Started
    node.stop().await;
    let conns = node.connection_manager.get_connections().await;
    assert!(conns.is_empty());
}
```

### Verification

```bash
cargo test -p truffle-core -- mesh_node
```

---

## Changeset 5: Event Loop Extraction

**Issues**: ARCH-1, ARCH-2, ARCH-14
**Risk**: MEDIUM (structural refactor, no logic change)
**Files to modify**:
- `crates/truffle-core/src/mesh/node.rs`
- `crates/truffle-core/src/mesh/mod.rs` (add `handler` module)
- **NEW**: `crates/truffle-core/src/mesh/handler.rs`

### Problem

The transport event loop is a ~240-line inline closure (lines 900-1136) with 5-6 levels of nesting. The same logic is duplicated as dead-code methods (`handle_mesh_message()`, `handle_incoming_data()`, `handle_route_envelope()`). Envelope creation is copy-pasted ~6 times.

### Changes

#### 5a. Create `mesh/handler.rs` with a `TransportHandler` struct

```rust
//! Handler methods for the MeshNode transport event loop.
//!
//! Extracted from the inline closure in `start_event_loop()` for testability.

use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::protocol::envelope::{MeshEnvelope, MESH_NAMESPACE};
use crate::protocol::message_types::*;
use crate::transport::connection::{ConnectionManager, ConnectionStatus, TransportEvent};
use crate::types::DeviceRole;

use super::device::DeviceManager;
use super::election::{ElectionEvent, PrimaryElection};
use super::message_bus::{BusMessage, MeshMessageBus};
use super::node::{IncomingMeshMessage, MeshNodeConfig, MeshNodeEvent};

/// Holds shared references needed by event handlers.
/// All methods are `&self` -- locks are acquired per-call.
pub(crate) struct TransportHandler {
    pub config: MeshNodeConfig,
    pub connection_manager: Arc<ConnectionManager>,
    pub device_manager: Arc<RwLock<DeviceManager>>,
    pub election: Arc<RwLock<PrimaryElection>>,
    pub event_tx: broadcast::Sender<MeshNodeEvent>,
    pub message_bus: Arc<MeshMessageBus>,
}

impl TransportHandler {
    // ── Envelope helpers ──────────────────────────────────────────────

    /// Create a mesh-namespace envelope wrapping a MeshMessage.
    fn wrap_mesh_message(message: &MeshMessage) -> MeshEnvelope {
        MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "message".to_string(),
            payload: serde_json::to_value(message).unwrap_or_default(),
            timestamp: Some(crate::mesh::node::current_timestamp_ms()),
        }
    }

    /// Serialize and send an envelope to a connection.
    async fn send_envelope_to_conn(&self, conn_id: &str, envelope: &MeshEnvelope) {
        if let Ok(value) = serde_json::to_value(envelope) {
            let _ = self.connection_manager.send(conn_id, &value).await;
        }
    }

    // ── Transport event handlers ──────────────────────────────────────

    pub async fn handle_connected(&self, conn: &crate::transport::connection::WSConnection) {
        tracing::info!("Transport connected: {} ({:?})", conn.id, conn.direction);

        // Send our device announce
        let local_device = self.device_manager.read().await.local_device().clone();
        let announce = MeshMessage::new(
            "device:announce",
            &self.config.device_id,
            serde_json::to_value(&DeviceAnnouncePayload {
                device: local_device.clone(),
                protocol_version: 2,
            }).unwrap_or_default(),
        );
        let envelope = Self::wrap_mesh_message(&announce);
        self.send_envelope_to_conn(&conn.id, &envelope).await;

        // If primary, send device list to new connection
        if self.election.read().await.is_primary() {
            let devices = {
                let dm = self.device_manager.read().await;
                let mut devs = vec![local_device];
                devs.extend(dm.devices());
                devs
            };
            let list = MeshMessage::new(
                "device:list",
                &self.config.device_id,
                serde_json::to_value(&DeviceListPayload {
                    devices,
                    primary_id: self.config.device_id.clone(),
                }).unwrap_or_default(),
            );
            let envelope = Self::wrap_mesh_message(&list);
            self.send_envelope_to_conn(&conn.id, &envelope).await;
        }
    }

    pub async fn handle_disconnected(&self, connection_id: &str, reason: &str) {
        tracing::info!("Transport disconnected: {connection_id} ({reason})");
        let devices = self.device_manager.read().await.devices();
        for device in &devices {
            let conn = self.connection_manager.get_connection_by_device(&device.id).await;
            if conn.is_none() {
                let mut dm = self.device_manager.write().await;
                dm.mark_device_offline(&device.id);
            }
        }
    }

    pub async fn handle_message(&self, connection_id: &str, payload: &serde_json::Value) {
        let envelope: MeshEnvelope = match serde_json::from_value(payload.clone()) {
            Ok(e) => e,
            Err(_) => return,
        };

        let conn = self.connection_manager.get_connection(connection_id).await;
        let from_device_id = conn.and_then(|c| c.device_id);

        if envelope.namespace == MESH_NAMESPACE {
            if envelope.msg_type == "message" {
                self.handle_mesh_envelope(connection_id, &from_device_id, &envelope).await;
            } else {
                self.handle_route_envelope(connection_id, from_device_id.as_deref(), &envelope).await;
            }
        } else {
            self.handle_app_message(connection_id, from_device_id, &envelope).await;
        }
    }

    async fn handle_mesh_envelope(
        &self,
        connection_id: &str,
        from_device_id: &Option<String>,
        envelope: &MeshEnvelope,
    ) {
        let mesh_msg: MeshMessage = match serde_json::from_value(envelope.payload.clone()) {
            Ok(m) => m,
            Err(_) => return,
        };

        // Bind device ID from announce
        if mesh_msg.msg_type == "device:announce" {
            if let Ok(payload) = serde_json::from_value::<DeviceAnnouncePayload>(mesh_msg.payload.clone()) {
                self.connection_manager
                    .set_device_id(connection_id, payload.device.id.clone())
                    .await;
            }
        }

        self.dispatch_mesh_message(&mesh_msg).await;
    }

    async fn dispatch_mesh_message(&self, msg: &MeshMessage) {
        match msg.msg_type.as_str() {
            "device:announce" => {
                if let Ok(payload) = serde_json::from_value::<DeviceAnnouncePayload>(msg.payload.clone()) {
                    self.device_manager.write().await.handle_device_announce(&msg.from, &payload);
                }
            }
            "device:list" => {
                if let Ok(payload) = serde_json::from_value::<DeviceListPayload>(msg.payload.clone()) {
                    self.device_manager.write().await.handle_device_list(&msg.from, &payload);
                    if !payload.primary_id.is_empty() {
                        self.election.write().await.set_primary(&payload.primary_id);
                    }
                }
            }
            "device:goodbye" => {
                self.device_manager.write().await.handle_device_goodbye(&msg.from);
            }
            "election:start" => {
                self.election.write().await.handle_election_start(&msg.from);
            }
            "election:candidate" => {
                if let Ok(payload) = serde_json::from_value::<ElectionCandidatePayload>(msg.payload.clone()) {
                    self.election.write().await.handle_election_candidate(&msg.from, &payload);
                }
            }
            "election:result" => {
                if let Ok(payload) = serde_json::from_value::<ElectionResultPayload>(msg.payload.clone()) {
                    self.election.write().await.handle_election_result(&msg.from, &payload);
                }
            }
            _ => {
                tracing::debug!("Unknown mesh message type: {}", msg.msg_type);
            }
        }
    }

    async fn handle_route_envelope(
        &self,
        connection_id: &str,
        from_device_id: Option<&str>,
        envelope: &MeshEnvelope,
    ) {
        let is_primary = self.election.read().await.is_primary();
        if !is_primary {
            tracing::warn!("Received route envelope but not primary");
            return;
        }

        match envelope.msg_type.as_str() {
            "route:message" => {
                if let (Some(target), Some(inner)) = (
                    envelope.payload.get("targetDeviceId").and_then(|v| v.as_str()),
                    envelope.payload.get("envelope"),
                ) {
                    if let Ok(inner_env) = serde_json::from_value::<MeshEnvelope>(inner.clone()) {
                        if let Some(conn) = self.connection_manager.get_connection_by_device(target).await {
                            let mut env_ts = inner_env;
                            if env_ts.timestamp.is_none() {
                                env_ts.timestamp = Some(crate::mesh::node::current_timestamp_ms());
                            }
                            self.send_envelope_to_conn(&conn.id, &env_ts).await;
                        }
                    }
                }
            }
            "route:broadcast" => {
                if let Some(inner) = envelope.payload.get("envelope") {
                    if let Ok(inner_env) = serde_json::from_value::<MeshEnvelope>(inner.clone()) {
                        let conns = self.connection_manager.get_connections().await;
                        for c in &conns {
                            if let Some(ref did) = c.device_id {
                                if from_device_id != Some(did.as_str())
                                    && c.status == ConnectionStatus::Connected
                                {
                                    let mut env_ts = inner_env.clone();
                                    if env_ts.timestamp.is_none() {
                                        env_ts.timestamp = Some(crate::mesh::node::current_timestamp_ms());
                                    }
                                    self.send_envelope_to_conn(&c.id, &env_ts).await;
                                }
                            }
                        }
                        // Local delivery
                        self.deliver_app_message(
                            connection_id,
                            from_device_id.map(|s| s.to_string()),
                            &inner_env,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    async fn handle_app_message(
        &self,
        connection_id: &str,
        from_device_id: Option<String>,
        envelope: &MeshEnvelope,
    ) {
        self.deliver_app_message(connection_id, from_device_id.clone(), envelope);
        self.message_bus.dispatch(&BusMessage {
            from: from_device_id,
            namespace: envelope.namespace.clone(),
            msg_type: envelope.msg_type.clone(),
            payload: envelope.payload.clone(),
        }).await;
    }

    fn deliver_app_message(
        &self,
        connection_id: &str,
        from: Option<String>,
        envelope: &MeshEnvelope,
    ) {
        let incoming = IncomingMeshMessage {
            from,
            connection_id: connection_id.to_string(),
            namespace: envelope.namespace.clone(),
            msg_type: envelope.msg_type.clone(),
            payload: envelope.payload.clone(),
        };
        let _ = self.event_tx.send(MeshNodeEvent::Message(incoming));
    }
}
```

#### 5b. Rewrite `start_event_loop()` transport loop

```rust
let handler = Arc::new(TransportHandler {
    config: config.clone(),
    connection_manager: connection_manager.clone(),
    device_manager: device_manager.clone(),
    election: election.clone(),
    event_tx: event_tx.clone(),
    message_bus: message_bus.clone(),
});

let handle = tokio::spawn(async move {
    loop {
        match transport_rx.recv().await {
            Ok(TransportEvent::Connected(conn)) => {
                handler.handle_connected(&conn).await;
            }
            Ok(TransportEvent::Disconnected { connection_id, reason }) => {
                handler.handle_disconnected(&connection_id, &reason).await;
            }
            Ok(TransportEvent::DeviceIdentified { connection_id, device_id }) => {
                tracing::info!("Device identified: {connection_id} -> {device_id}");
            }
            Ok(TransportEvent::Message { connection_id, message }) => {
                handler.handle_message(&connection_id, &message.payload).await;
            }
            Ok(TransportEvent::Reconnecting { device_id, attempt, delay }) => {
                tracing::info!("Reconnecting to {device_id}: attempt {attempt}, delay {delay:?}");
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Transport event receiver lagged by {n}");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
});
```

#### 5c. Delete dead code methods

Remove `handle_mesh_message()`, `handle_incoming_data()`, `handle_route_envelope()`, and `broadcast_announce()` from `MeshNode` (all marked `#[allow(dead_code)]`).

Make `current_timestamp_ms()` `pub(crate)` so `handler.rs` can use it.

### New Tests

All `TransportHandler` methods are now independently testable:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_handler() -> (TransportHandler, broadcast::Receiver<MeshNodeEvent>) {
        // Creates handler with mock ConnectionManager, DeviceManager, Election
        // ...
    }

    #[tokio::test]
    async fn dispatch_device_announce() {
        let (handler, _rx) = make_handler();
        let msg = MeshMessage::new("device:announce", "dev-2", /* payload */);
        handler.dispatch_mesh_message(&msg).await;
        let dm = handler.device_manager.read().await;
        assert!(dm.device_by_id("dev-2").is_some());
    }

    #[tokio::test]
    async fn dispatch_device_list_sets_primary() {
        let (handler, _rx) = make_handler();
        let msg = MeshMessage::new("device:list", "dev-2", /* payload with primary_id */);
        handler.dispatch_mesh_message(&msg).await;
        assert_eq!(handler.election.read().await.primary_id(), Some("dev-2"));
    }

    #[tokio::test]
    async fn route_message_forwarded_by_primary() {
        // Test that route:message is forwarded to target device
    }

    #[tokio::test]
    async fn route_broadcast_forwarded_to_all_except_sender() {
        // Test that route:broadcast goes to all except sender
    }
}
```

### Verification

```bash
cargo test -p truffle-core -- handler
cargo test -p truffle-core -- mesh_node
```

---

## Changeset 6: MeshNode State Consolidation

**Issues**: ARCH-3, ARCH-11
**Risk**: MEDIUM (changes internal struct layout)
**Files to modify**:
- `crates/truffle-core/src/mesh/node.rs`

### Problem

**ARCH-3**: `handle_tailnet_peers()` acquires `device_manager.read()` then `election.read()`. Another task could hold `election.write()` and try `device_manager.write()`, causing ABBA deadlock.

**ARCH-11**: MeshNode has 14 fields, 9 wrapped in `Arc<RwLock<>>`.

### Solution

Group mutable lifecycle state into `MeshNodeState`:

```rust
/// Mutable lifecycle state, grouped under a single lock.
struct MeshNodeLifecycle {
    running: bool,
    started_at: u64,
    auth_status: AuthStatus,
    auth_url: Option<String>,
    announce_handle: Option<tokio::task::AbortHandle>,
    event_loop_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Default for MeshNodeLifecycle {
    fn default() -> Self {
        Self {
            running: false,
            started_at: 0,
            auth_status: AuthStatus::Unknown,
            auth_url: None,
            announce_handle: None,
            event_loop_handle: None,
        }
    }
}
```

This reduces 6 `Arc<RwLock<>>` fields to 1:

```rust
pub struct MeshNode {
    config: MeshNodeConfig,
    device_manager: Arc<RwLock<DeviceManager>>,
    election: Arc<RwLock<PrimaryElection>>,
    connection_manager: Arc<ConnectionManager>,
    message_bus: Arc<MeshMessageBus>,
    lifecycle: Arc<RwLock<MeshNodeLifecycle>>,
    event_tx: broadcast::Sender<MeshNodeEvent>,
    device_event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<DeviceEvent>>>,
    election_event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<ElectionEvent>>>,
}
```

For ARCH-3, document and enforce lock ordering:

```rust
/// Lock ordering (always acquire in this order to prevent deadlock):
/// 1. lifecycle
/// 2. device_manager
/// 3. election
///
/// NEVER acquire device_manager while holding election, or vice versa,
/// unless you acquire device_manager FIRST.
```

Fix `handle_tailnet_peers()` to follow the ordering -- it currently does `device_manager.read()` then `election.read()`, which is correct. But we must audit all other call sites. The problematic pattern is in the election event handler (device event -> election lock in DeviceOffline handler). Fix by not holding the device_manager lock when acquiring election:

```rust
DeviceEvent::DeviceOffline(id) => {
    let _ = event_tx_dev.send(MeshNodeEvent::DeviceOffline(id.clone()));
    // Read primary_id without holding device_manager lock
    let primary_id = election_dev.read().await.primary_id().map(|s| s.to_string());
    if primary_id.as_deref() == Some(id.as_str()) {
        let mut el = election_dev.write().await;
        el.handle_primary_lost(id);
    }
}
```

This is already the current code -- the device event handler receives events via channel, not while holding the device_manager lock. So ARCH-3 is actually safe in current code, but we document the ordering to prevent future regressions.

### New Tests

```rust
#[tokio::test]
async fn lifecycle_state_grouped() {
    let (node, _rx) = create_test_node();
    let lc = node.lifecycle.read().await;
    assert!(!lc.running);
    assert_eq!(lc.auth_status, AuthStatus::Unknown);
}
```

### Verification

```bash
cargo test -p truffle-core
```

---

## Changeset 7: Runtime Extraction

**Issues**: ARCH-4, ARCH-8
**Risk**: MEDIUM (new public API surface, NAPI becomes thinner)
**Files to modify**:
- **NEW**: `crates/truffle-core/src/runtime.rs`
- `crates/truffle-core/src/lib.rs`
- `crates/truffle-napi/src/mesh_node.rs`

### Problem

`napi/mesh_node.rs` contains ~200 lines of pure Rust domain logic: sidecar bootstrapping, lifecycle event handling, peer dialing. A Tauri plugin would need to duplicate all of it.

### Changes

#### 7a. Create `runtime.rs`

```rust
//! TruffleRuntime -- owns the full lifecycle of a Truffle mesh node.
//!
//! Wires together BridgeManager, GoShim, ConnectionManager, and MeshNode.
//! NAPI and Tauri consume this as a thin wrapper.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, oneshot, Mutex};

use crate::bridge::header::Direction;
use crate::bridge::manager::{BridgeConnection, BridgeManager, ChannelHandler};
use crate::bridge::shim::{GoShim, ShimConfig, ShimError, ShimLifecycleEvent};
use crate::mesh::node::{MeshNode, MeshNodeConfig, MeshNodeEvent};
use crate::transport::connection::{ConnectionManager, TransportConfig};
use crate::types::TailnetPeer;

/// Configuration for the Truffle runtime.
pub struct RuntimeConfig {
    pub mesh: MeshNodeConfig,
    pub transport: TransportConfig,
    pub sidecar_path: Option<PathBuf>,
    pub state_dir: Option<String>,
    pub auth_key: Option<String>,
}

/// The full Truffle runtime -- owns sidecar, bridge, and mesh node.
pub struct TruffleRuntime {
    mesh_node: Arc<MeshNode>,
    connection_manager: Arc<ConnectionManager>,
    shim: Arc<Mutex<Option<GoShim>>>,
    bridge_pending_dials: Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>>,
}

impl TruffleRuntime {
    /// Create a new runtime (does not start anything).
    pub fn new(config: RuntimeConfig) -> (Self, broadcast::Receiver<MeshNodeEvent>) {
        let (connection_manager, _transport_rx) = ConnectionManager::new(config.transport);
        let connection_manager = Arc::new(connection_manager);
        let (mesh_node, event_rx) = MeshNode::new(config.mesh, connection_manager.clone());

        let runtime = Self {
            mesh_node: Arc::new(mesh_node),
            connection_manager,
            shim: Arc::new(Mutex::new(None)),
            bridge_pending_dials: Arc::new(Mutex::new(HashMap::new())),
        };

        (runtime, event_rx)
    }

    /// Start the runtime: spawn sidecar (if configured), start mesh node.
    pub async fn start(&self, config: &RuntimeConfig) -> Result<(), RuntimeError> {
        if let Some(ref sidecar_path) = config.sidecar_path {
            self.bootstrap_sidecar(sidecar_path, config).await?;
        }
        self.mesh_node.start().await;
        Ok(())
    }

    /// Stop the runtime: stop mesh node and sidecar.
    pub async fn stop(&self) {
        let shim = {
            let mut guard = self.shim.lock().await;
            guard.take()
        };
        if let Some(shim) = shim {
            let _ = shim.stop().await;
        }
        self.mesh_node.stop().await;
    }

    /// Access the mesh node.
    pub fn mesh_node(&self) -> &Arc<MeshNode> {
        &self.mesh_node
    }

    /// Dial a peer through the full bridge pipeline.
    pub async fn dial_peer(&self, target_dns: &str, port: u16) -> Result<(), String> {
        let shim_guard = self.shim.lock().await;
        let shim = shim_guard.as_ref().ok_or("no sidecar running")?;

        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        {
            let mut dials = self.bridge_pending_dials.lock().await;
            dials.insert(request_id.clone(), tx);
        }

        shim.dial_raw(target_dns.to_string(), port, request_id.clone())
            .await
            .map_err(|e| {
                // Clean up on failure
                let pending = self.bridge_pending_dials.clone();
                let rid = request_id.clone();
                tokio::spawn(async move {
                    pending.lock().await.remove(&rid);
                });
                format!("dial command failed: {e}")
            })?;

        drop(shim_guard); // Release shim lock during network wait

        let timeout = Duration::from_secs(10);
        let bridge_conn = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(_)) => return Err("dial cancelled".into()),
            Err(_) => {
                self.bridge_pending_dials.lock().await.remove(&request_id);
                return Err(format!("dial timed out after {timeout:?}"));
            }
        };

        self.connection_manager.handle_outgoing(bridge_conn).await;
        Ok(())
    }

    async fn bootstrap_sidecar(
        &self,
        sidecar_path: &PathBuf,
        config: &RuntimeConfig,
    ) -> Result<(), RuntimeError> {
        // Generate session token
        let mut session_token = [0u8; 32];
        getrandom::getrandom(&mut session_token)
            .map_err(|e| RuntimeError::Bootstrap(format!("token generation failed: {e}")))?;
        let session_token_hex = hex::encode(session_token);

        // Bind bridge
        let mut bridge_manager = BridgeManager::bind(session_token)
            .await
            .map_err(|e| RuntimeError::Bootstrap(format!("bridge bind failed: {e}")))?;
        let bridge_port = bridge_manager.local_port();

        // Register WS handlers
        let (ws_in_tx, mut ws_in_rx) = mpsc::channel(64);
        let (ws_out_tx, mut ws_out_rx) = mpsc::channel(64);
        bridge_manager.add_handler(443, Direction::Incoming, Arc::new(ChannelHandler::new(ws_in_tx)));
        bridge_manager.add_handler(443, Direction::Outgoing, Arc::new(ChannelHandler::new(ws_out_tx)));

        // Store bridge pending_dials reference for dial_peer()
        let bridge_pending = bridge_manager.pending_dials().clone();
        {
            // Replace our own pending_dials with the bridge's
            // (They point to the same Arc)
            // Actually, we just use bridge_manager's pending_dials directly.
        }

        // Spawn bridge (CS-9: CancellationToken for graceful shutdown)
        tokio::spawn(async move { bridge_manager.run(tokio_util::sync::CancellationToken::new()).await });

        // Spawn WS connection handlers
        let conn_mgr = self.connection_manager.clone();
        tokio::spawn(async move {
            while let Some(bc) = ws_in_rx.recv().await {
                conn_mgr.handle_incoming(bc).await;
            }
        });
        let conn_mgr2 = self.connection_manager.clone();
        tokio::spawn(async move {
            while let Some(bc) = ws_out_rx.recv().await {
                conn_mgr2.handle_outgoing(bc).await;
            }
        });

        // Build hostname
        let hostname = format!(
            "{}-{}-{}",
            config.mesh.hostname_prefix,
            config.mesh.device_type,
            config.mesh.device_id
        );

        let state_dir = config.state_dir.clone()
            .unwrap_or_else(|| format!(".truffle-state/{}", config.mesh.device_id));
        std::fs::create_dir_all(&state_dir)
            .map_err(|e| RuntimeError::Bootstrap(format!("state dir: {e}")))?;

        // Spawn sidecar
        let shim_config = ShimConfig {
            binary_path: sidecar_path.clone(),
            hostname,
            state_dir,
            auth_key: config.auth_key.clone(),
            bridge_port,
            session_token: session_token_hex,
            auto_restart: true,
        };

        let (shim, lifecycle_rx) = GoShim::spawn(shim_config)
            .await
            .map_err(|e| RuntimeError::Bootstrap(format!("sidecar spawn: {e}")))?;

        *self.shim.lock().await = Some(shim);

        // Spawn lifecycle handler
        self.spawn_lifecycle_handler(lifecycle_rx, bridge_pending);

        Ok(())
    }

    fn spawn_lifecycle_handler(
        &self,
        mut rx: broadcast::Receiver<ShimLifecycleEvent>,
        bridge_pending: Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>>,
    ) {
        let node = self.mesh_node.clone();
        let shim = self.shim.clone();
        let conn_mgr = self.connection_manager.clone();
        let hostname_prefix = node.config_ref().hostname_prefix.clone();

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ShimLifecycleEvent::Status(status)) => {
                        if !status.tailscale_ip.is_empty() {
                            let dns = if status.dns_name.is_empty() { None } else { Some(status.dns_name.as_str()) };
                            node.set_local_online(&status.tailscale_ip, dns).await;
                            node.set_auth_authenticated().await;
                            // Request peers
                            let sg = shim.lock().await;
                            if let Some(ref s) = *sg {
                                let _ = s.get_peers().await;
                            }
                        }
                    }
                    Ok(ShimLifecycleEvent::AuthRequired { auth_url }) => {
                        node.set_auth_required(&auth_url).await;
                    }
                    Ok(ShimLifecycleEvent::Peers(peers_data)) => {
                        let core_peers: Vec<TailnetPeer> = peers_data.peers.iter()
                            .map(|p| p.to_canonical())
                            .collect();
                        node.handle_tailnet_peers(&core_peers).await;

                        // Dial online truffle peers
                        let local_dns = node.local_device().await
                            .tailscale_dns_name.unwrap_or_default();
                        for peer in &peers_data.peers {
                            if !peer.online || peer.dns_name.is_empty() { continue; }
                            if peer.dns_name == local_dns { continue; }
                            if !peer.hostname.contains(&hostname_prefix) { continue; }

                            let dns = peer.dns_name.clone();
                            let bp = bridge_pending.clone();
                            let sh = shim.clone();
                            let cm = conn_mgr.clone();
                            tokio::spawn(async move {
                                let sg = sh.lock().await;
                                if let Some(ref s) = *sg {
                                    let rid = uuid::Uuid::new_v4().to_string();
                                    let (tx, rx) = oneshot::channel();
                                    bp.lock().await.insert(rid.clone(), tx);
                                    if s.dial_raw(dns.clone(), 443, rid.clone()).await.is_ok() {
                                        drop(sg);
                                        match tokio::time::timeout(Duration::from_secs(10), rx).await {
                                            Ok(Ok(bc)) => { cm.handle_outgoing(bc).await; }
                                            _ => { bp.lock().await.remove(&rid); }
                                        }
                                    } else {
                                        bp.lock().await.remove(&rid);
                                    }
                                }
                            });
                        }
                    }
                    Ok(ShimLifecycleEvent::DialFailed { request_id, error }) => {
                        tracing::warn!("Dial failed: {request_id}: {error}");
                        bridge_pending.lock().await.remove(&request_id);
                    }
                    Ok(ShimLifecycleEvent::Crashed { exit_code, stderr_tail }) => {
                        node.emit_event(MeshNodeEvent::Error(format!(
                            "Sidecar crashed (exit={exit_code:?}): {}",
                            stderr_tail.chars().take(200).collect::<String>()
                        )));
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Lifecycle receiver lagged by {n}");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("bootstrap failed: {0}")]
    Bootstrap(String),
}
```

#### 7b. Add `config_ref()` to MeshNode

```rust
impl MeshNode {
    /// Access the config (needed by runtime.rs for hostname_prefix).
    pub fn config_ref(&self) -> &MeshNodeConfig {
        &self.config
    }
}
```

#### 7c. Thin out NAPI layer

`NapiMeshNode` becomes a thin wrapper around `TruffleRuntime`:

```rust
#[napi]
pub struct NapiMeshNode {
    runtime: Arc<TruffleRuntime>,
    pending_callback: std::sync::Mutex<Option<ThreadsafeFunction<NapiMeshEvent>>>,
    config: RuntimeConfig, // stored for start()
}

#[napi]
impl NapiMeshNode {
    #[napi]
    pub async fn start(&self) -> Result<()> {
        // ... setup callback ...
        self.runtime.start(&self.config).await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(())
    }

    #[napi]
    pub async fn stop(&self) -> Result<()> {
        self.runtime.stop().await;
        Ok(())
    }

    // All other methods delegate to self.runtime.mesh_node()
}
```

### Breaking API Changes

None. The NAPI interface remains identical. `TruffleRuntime` is a new `pub` API in `truffle-core`.

### New Tests

```rust
#[tokio::test]
async fn runtime_creates_and_starts() {
    let config = RuntimeConfig { /* ... no sidecar ... */ };
    let (runtime, mut rx) = TruffleRuntime::new(config);
    runtime.start(&config).await.unwrap();
    let event = rx.recv().await.unwrap();
    assert!(matches!(event, MeshNodeEvent::Started));
    runtime.stop().await;
}
```

### Verification

```bash
cargo test -p truffle-core -- runtime
cargo test -p truffle-napi
```

---

## Changeset 8: Event Drop Logging + Signal Separation

**Issues**: ARCH-6, ARCH-10
**Risk**: LOW
**Files to modify**:
- `crates/truffle-core/src/mesh/device.rs`
- `crates/truffle-core/src/mesh/election.rs`
- `crates/truffle-core/src/bridge/shim.rs`

### Changes

#### 8a. ARCH-6: Log warnings on `try_send()` failure

In `device.rs`:

```rust
fn emit(&self, event: DeviceEvent) {
    if let Err(mpsc::error::TrySendError::Full(event)) = self.event_tx.try_send(event) {
        tracing::warn!(
            "DeviceEvent channel full, dropping {:?}",
            std::mem::discriminant(&event)
        );
    }
}
```

In `election.rs`:

```rust
fn emit(&self, event: ElectionEvent) {
    if let Err(mpsc::error::TrySendError::Full(event)) = self.event_tx.try_send(event) {
        tracing::warn!(
            "ElectionEvent channel full, dropping {:?}",
            std::mem::discriminant(&event)
        );
    }
}
```

For critical events (`PrimaryChanged`, `PrimaryElected`), consider using `send().await` in the spawned tasks where an async context is available. For synchronous `&self` methods, `try_send` with a warning is the best we can do without changing the API to async.

#### 8b. ARCH-10: Separate resume signal from shutdown

In `GoShim`, replace `resume_auto_restart()` which overloads the `shutdown` Notify:

```rust
pub struct GoShim {
    // ... existing fields ...
    shutdown: Arc<Notify>,
    resume_signal: Arc<Notify>,  // NEW: separate signal
    auto_restart_paused: Arc<std::sync::atomic::AtomicBool>,
}
```

In `spawn_manager_task`, when auto-restart is paused:

```rust
if last_event_was_auth {
    auto_restart_paused.store(true, std::sync::atomic::Ordering::Relaxed);
    // Wait for resume OR shutdown
    tokio::select! {
        _ = resume_signal.notified() => {
            auto_restart_paused.store(false, std::sync::atomic::Ordering::Relaxed);
            restart_delay = INITIAL_RESTART_DELAY;
            continue;
        }
        _ = shutdown.notified() => break,
    }
}
```

```rust
pub fn resume_auto_restart(&self) {
    self.auto_restart_paused.store(false, std::sync::atomic::Ordering::Relaxed);
    self.resume_signal.notify_one();
}
```

### New Tests

```rust
#[test]
fn device_event_channel_full_logs_warning() {
    // Create a channel with capacity 1, fill it, verify try_send handles gracefully
    let (tx, _rx) = mpsc::channel(1);
    let _ = tx.try_send(DeviceEvent::DevicesChanged(vec![]));
    // Second send should not panic
    let result = tx.try_send(DeviceEvent::DevicesChanged(vec![]));
    assert!(result.is_err());
}
```

### Verification

```bash
cargo test -p truffle-core -- emit
cargo test -p truffle-core -- shim
```

---

## Changeset 9: Code Quality Sweep

**Issues**: ARCH-5, ARCH-7, ARCH-9, ARCH-12
**Risk**: LOW
**Files to modify**:
- `crates/truffle-core/src/mesh/node.rs` (ARCH-5: remove duplicate `current_timestamp_ms`)
- `crates/truffle-core/src/mesh/election.rs` (ARCH-5)
- `crates/truffle-core/src/protocol/message_types.rs` (ARCH-5)
- `crates/truffle-core/src/mesh/message_bus.rs` (ARCH-7)
- `crates/truffle-core/src/bridge/manager.rs` (ARCH-9)
- **NEW or existing**: `crates/truffle-core/src/util.rs` (ARCH-5)
- `crates/truffle-core/src/store_sync/adapter.rs` (ARCH-12)

### Changes

#### 9a. ARCH-5: Deduplicate `current_timestamp_ms()`

Create `crates/truffle-core/src/util.rs`:

```rust
/// Current time in milliseconds since Unix epoch.
pub fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
```

Add `pub mod util;` to `lib.rs`. Replace all 3 copies with `crate::util::current_timestamp_ms()`.

#### 9b. ARCH-7: Implement `unsubscribe()` or remove `SubscriptionId`

Option A (implement): Use UUID-keyed subscriptions:

```rust
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubscriptionId(Uuid);

pub async fn subscribe(
    &self,
    namespace: &str,
    handler: BusMessageHandler,
) -> SubscriptionId {
    let id = SubscriptionId(Uuid::new_v4());
    let mut handlers = self.handlers.write().await;
    let entry = handlers.entry(namespace.to_string()).or_default();
    entry.push((id.clone(), handler));
    let _ = self.event_tx.try_send(MessageBusEvent::Subscribed(namespace.to_string()));
    id
}

pub async fn unsubscribe(&self, id: &SubscriptionId) {
    let mut handlers = self.handlers.write().await;
    for (_, entries) in handlers.iter_mut() {
        entries.retain(|(sub_id, _)| sub_id != id);
    }
    let _ = self.event_tx.try_send(MessageBusEvent::Unsubscribed(format!("{:?}", id)));
}
```

Update handler storage type from `Vec<BusMessageHandler>` to `Vec<(SubscriptionId, BusMessageHandler)>`.

Option B (simpler, recommended): Remove `SubscriptionId` return type. Change `subscribe` to return `()`. This is the M-7 assessment: "Low priority". We go with Option B for now and add a TODO for Option A.

```rust
pub async fn subscribe(&self, namespace: &str, handler: BusMessageHandler) {
    let mut handlers = self.handlers.write().await;
    let entry = handlers.entry(namespace.to_string()).or_default();
    entry.push(handler);
    let _ = self.event_tx.try_send(MessageBusEvent::Subscribed(namespace.to_string()));
}
```

Delete `SubscriptionId` struct.

#### 9c. ARCH-9: Add shutdown to `BridgeManager::run()`

Change `run(self)` to take a `CancellationToken`:

```rust
use tokio_util::sync::CancellationToken;

pub async fn run(self, cancel: CancellationToken) {
    let session_token = self.session_token;
    // ... existing setup ...

    loop {
        tokio::select! {
            result = self.listener.accept() => {
                match result {
                    Ok((stream, addr)) => { /* existing logic */ }
                    Err(e) => {
                        tracing::error!("bridge accept error: {e}");
                        break;
                    }
                }
            }
            _ = cancel.cancelled() => {
                tracing::info!("BridgeManager shutting down");
                break;
            }
        }
    }
}
```

Add `tokio-util` to `Cargo.toml` if not present. Update callers to pass a `CancellationToken`.

#### 9d. ARCH-12: Use `AtomicBool` in `StoreSyncAdapter`

Find the `tokio::sync::RwLock<bool>` fields in `store_sync/adapter.rs` and replace with `AtomicBool`:

```rust
use std::sync::atomic::{AtomicBool, Ordering};

pub struct StoreSyncAdapter {
    // ...
    syncing: AtomicBool,
    enabled: AtomicBool,
}

impl StoreSyncAdapter {
    pub fn is_syncing(&self) -> bool {
        self.syncing.load(Ordering::Relaxed)
    }

    pub fn set_syncing(&self, val: bool) {
        self.syncing.store(val, Ordering::Relaxed);
    }
}
```

### New Tests

```rust
#[test]
fn util_current_timestamp_ms() {
    let ts = crate::util::current_timestamp_ms();
    assert!(ts > 1_700_000_000_000); // After 2023
}

#[tokio::test]
async fn message_bus_subscribe_returns_unit() {
    let (tx, _rx) = mpsc::channel(64);
    let bus = MeshMessageBus::new(tx);
    bus.subscribe("test", Arc::new(|_| {})).await;
    // Should compile -- no SubscriptionId to handle
}
```

### Verification

```bash
cargo test -p truffle-core
```

---

## Changeset 10: Sidecar Robustness

**Issues**: BUG-8, M-2
**Risk**: LOW (Go code change + Rust timeout addition)
**Files to modify**:
- `packages/sidecar-slim/main.go`
- `crates/truffle-core/src/bridge/manager.rs` (for M-2 context)

### Changes

#### 10a. BUG-8: Report bridge connection failure in Go

In `main.go`, the `bridgeToRust` function silently drops the connection if `net.Dial` to the Rust bridge port fails. Add a `dialResult` failure report:

```go
func (s *shim) bridgeToRust(tsnetConn net.Conn, port uint16, direction byte, requestID, remoteAddr, remoteDNS string) {
    localConn, err := net.Dial("tcp", fmt.Sprintf("127.0.0.1:%d", s.bridgePort))
    if err != nil {
        log.Printf("bridge connect failed: %v", err)
        tsnetConn.Close()
        // NEW: Report failure if this was an outgoing dial
        if direction == dirOutgoing && requestID != "" {
            s.sendEvent(Event{
                Event: "bridge:dialResult",
                Data: map[string]interface{}{
                    "requestId": requestID,
                    "success":   false,
                    "error":     fmt.Sprintf("bridge connect failed: %v", err),
                },
            })
        }
        return
    }
    // ... rest unchanged
}
```

Also report header write failure:

```go
    if err := writeHeader(localConn, s.sessionToken, direction, port, requestID, remoteAddr, remoteDNS); err != nil {
        log.Printf("header write failed: %v", err)
        localConn.Close()
        tsnetConn.Close()
        // NEW: Report failure
        if direction == dirOutgoing && requestID != "" {
            s.sendEvent(Event{
                Event: "bridge:dialResult",
                Data: map[string]interface{}{
                    "requestId": requestID,
                    "success":   false,
                    "error":     fmt.Sprintf("header write failed: %v", err),
                },
            })
        }
        return
    }
```

#### 10b. M-2: Connection timeout already implemented in CS-3

The `dial_peer()` function in CS-3/CS-7 already uses `tokio::time::timeout(Duration::from_secs(10), rx)`. This implements M-2 (10-second connection timeout matching `ws-service.ts:222-227`).

### New Tests

Go tests:

```go
func TestBridgeToRust_DialFailure_ReportsResult(t *testing.T) {
    // Start a shim with a non-existent bridge port
    // Trigger a dial
    // Verify dialResult failure event is emitted on stdout
}
```

### Verification

```bash
cd packages/sidecar-slim && go test ./...
cargo test -p truffle-core
```

---

## Risk Assessment

| Changeset | Risk | Regression Likelihood | Mitigation |
|-----------|------|----------------------|------------|
| CS-1 (serde) | LOW | Very low | Additive aliases only. Existing tests continue to pass with camelCase. |
| CS-2 (election) | MEDIUM | Medium | New event variants change state machine flow. Thoroughly test grace period + timeout paths with `tokio::time::pause()`. |
| CS-3 (dial unification) | HIGH | High | Most critical fix. Changes the connection establishment pipeline. Requires end-to-end testing with real sidecar. |
| CS-4 (shutdown) | LOW | Low | Adds cleanup calls. Small delay for goodbye flush. |
| CS-5 (event loop) | MEDIUM | Medium | Structural refactor. Logic must be identical. Compare test coverage before/after. |
| CS-6 (state consolidation) | MEDIUM | Low | Internal struct change. Public API unchanged. |
| CS-7 (runtime) | MEDIUM | Medium | New abstraction layer. NAPI becomes thinner. Test that all NAPI functions still work. |
| CS-8 (event drops) | LOW | Very low | Logging-only change + separate Notify. |
| CS-9 (quality) | LOW | Very low | Utility extraction, dead API cleanup. |
| CS-10 (sidecar) | LOW | Low | Go-side error reporting. Rust side already handles dial failures. |

### Testing Strategy

1. **Unit tests**: Each changeset adds targeted tests. Run `cargo test -p truffle-core` after each.
2. **Integration tests**: After CS-3 and CS-7, run the integration test suite from `integration.rs`.
3. **End-to-end**: After all changesets, perform manual 2-node testing on a real tailnet:
   - Node A starts, becomes primary.
   - Node B starts, dials A, receives device list.
   - Kill A, verify B detects primary loss, runs election, becomes primary.
   - Restart A, verify A connects to B, receives device list, becomes secondary.
4. **NAPI compatibility**: After CS-7, run `npm test` in the truffle-napi package to verify all bindings work.

### Estimated Effort

| Changeset | LOC Changed | Time Estimate |
|-----------|-------------|---------------|
| CS-1 | ~50 | 30 min |
| CS-2 | ~150 | 2 hours |
| CS-3 | ~300 | 3 hours |
| CS-4 | ~60 | 1 hour |
| CS-5 | ~500 (net zero, extracting) | 3 hours |
| CS-6 | ~150 | 1.5 hours |
| CS-7 | ~400 (new file) | 3 hours |
| CS-8 | ~40 | 30 min |
| CS-9 | ~100 | 1 hour |
| CS-10 | ~30 Go | 30 min |
| **Total** | **~1,780** | **~16 hours** |

---

## Summary of Issue-to-Changeset Mapping

| Issue | Changeset | Description |
|-------|-----------|-------------|
| BUG-1 | CS-3 | Unify dual pending_dials maps |
| BUG-2 | CS-1 | Fix serde aliases for acronym casing |
| BUG-3 | CS-2 | Replace Broadcast("election:timeout") with DecideNow |
| BUG-4 | CS-2 | Replace grace period task with GracePeriodExpired event |
| BUG-5 | CS-4 | Call broadcast_device_list() after election win |
| BUG-6 | CS-4 | Call close_all() on stop |
| BUG-7 | CS-4 | Add flush delay before aborting event loop |
| BUG-8 | CS-10 | Report bridge:dialResult failure from Go |
| BUG-9 | CS-3 | Eliminate GoShim pending_dials (use BridgeManager's) |
| ARCH-1 | CS-5 | Extract event loop into testable handler methods |
| ARCH-2 | CS-5 | Delete dead code methods |
| ARCH-3 | CS-6 | Document lock ordering, verify compliance |
| ARCH-4 | CS-7 | Create runtime.rs with TruffleRuntime |
| ARCH-5 | CS-9 | Deduplicate current_timestamp_ms() into util.rs |
| ARCH-6 | CS-8 | Log warnings on try_send() failure |
| ARCH-7 | CS-9 | Remove unused SubscriptionId (or implement unsubscribe) |
| ARCH-8 | CS-3/CS-7 | Don't hold shim lock across dials; spawn per-dial tasks |
| ARCH-9 | CS-9 | Add CancellationToken to BridgeManager::run() |
| ARCH-10 | CS-8 | Separate resume_signal from shutdown Notify |
| ARCH-11 | CS-6 | Group lifecycle fields into MeshNodeLifecycle |
| ARCH-12 | CS-9 | Replace RwLock<bool> with AtomicBool in StoreSyncAdapter |
| ARCH-13 | CS-1 | Eliminate duplicate TailnetPeer type |
| ARCH-14 | CS-5 | Consolidate envelope creation into helper methods |
| M-2 | CS-3/CS-10 | 10-second dial timeout |
| M-3 | CS-4 | close_all() on stop |

---

## Post-Implementation API Summary

This section documents all public API changes that resulted from implementing CS-1 through CS-10. Added 2026-03-18 after implementation.

### New Modules

| Module | File | Description |
|--------|------|-------------|
| `runtime` | `src/runtime.rs` | `TruffleRuntime` -- owns full lifecycle (sidecar, bridge, mesh node). NAPI/Tauri consume this. |
| `util` | `src/util.rs` | `current_timestamp_ms()` -- single shared utility, replaces 3 duplicates. |
| `mesh::handler` | `src/mesh/handler.rs` | `TransportHandler` (pub(crate)) -- extracted transport event loop handlers for testability. |
| `integration` | `src/integration.rs` | `wire_store_sync()`, `wire_file_transfer()` -- boilerplate-free wiring of adapters to MeshNode. |

### Changed Types

#### `ElectionEvent` (election.rs)

Two new local-only variants added:

```rust
pub enum ElectionEvent {
    ElectionStarted,
    PrimaryElected { device_id: String, is_local: bool },
    PrimaryLost { previous_primary_id: String },
    Broadcast(MeshMessage),
    GracePeriodExpired,  // NEW (CS-2): replaces grace period broadcasting directly
    DecideNow,           // NEW (CS-2): replaces Broadcast("election:timeout")
}
```

#### `ShimLifecycleEvent` (bridge/shim.rs)

New `DialFailed` variant:

```rust
pub enum ShimLifecycleEvent {
    Started,
    Crashed { exit_code: Option<i32>, stderr_tail: String },
    AuthRequired { auth_url: String },
    Status(StatusEventData),
    Peers(PeersEventData),
    Stopped,
    DialFailed { request_id: String, error: String },  // NEW (CS-3)
}
```

#### `PeersEventData` (bridge/protocol.rs)

Now uses `BridgeTailnetPeer` (wire-format) instead of the canonical `TailnetPeer`:

```rust
pub struct PeersEventData {
    pub peers: Vec<BridgeTailnetPeer>,  // CHANGED from Vec<TailnetPeer> (CS-1)
}

// NEW wire-format struct (CS-1):
pub struct BridgeTailnetPeer {
    pub id: String,
    pub hostname: String,
    pub dns_name: String,
    pub tailscale_ips: Vec<String>,
    pub online: bool,
    pub os: String,  // String, not Option<String> -- empty = None
}

impl BridgeTailnetPeer {
    pub fn to_canonical(&self) -> TailnetPeer { ... }
}
```

#### `BaseDevice` (types/mod.rs)

New serde aliases for Go/TS interop:

```rust
pub struct BaseDevice {
    // ...
    #[serde(skip_serializing_if = "Option::is_none", alias = "tailscaleDNSName")]
    pub tailscale_dns_name: Option<String>,  // CHANGED: added alias (CS-1)
    #[serde(skip_serializing_if = "Option::is_none", alias = "tailscaleIP")]
    pub tailscale_ip: Option<String>,         // CHANGED: added alias (CS-1)
    // ...
}
```

#### `TailnetPeer` (types/mod.rs)

New serde rename/alias for `tailscale_ips`:

```rust
pub struct TailnetPeer {
    // ...
    #[serde(rename = "tailscaleIPs", alias = "tailscaleIps")]
    pub tailscale_ips: Vec<String>,  // CHANGED: explicit rename + alias (CS-1)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
}
```

### Changed Method Signatures

#### `GoShim::dial()` renamed to `GoShim::dial_raw()` (bridge/shim.rs)

```rust
// OLD:
pub async fn dial(&self, target: String, port: u16) -> Result<oneshot::Receiver<TcpStream>, ShimError>

// NEW (CS-3): Caller provides request_id, manages pending_dials externally
pub async fn dial_raw(&self, target: String, port: u16, request_id: String) -> Result<(), ShimError>
```

`GoShim::pending_dials` field was removed entirely. Pending dials are now managed by `BridgeManager::pending_dials()`.

#### `BridgeManager::run()` (bridge/manager.rs)

```rust
// OLD:
pub async fn run(self)

// NEW (CS-9): Takes CancellationToken for graceful shutdown
pub async fn run(self, cancel: tokio_util::sync::CancellationToken)
```

#### `MeshMessageBus::subscribe()` (mesh/message_bus.rs)

```rust
// OLD:
pub async fn subscribe(&self, namespace: &str, handler: BusMessageHandler) -> SubscriptionId

// NEW (CS-9): SubscriptionId removed; returns ()
pub async fn subscribe(&self, namespace: &str, handler: BusMessageHandler)
```

`SubscriptionId` type was removed. Unsubscribe support deferred to M-7.

#### `MeshNode::new()` (mesh/node.rs)

```rust
// OLD:
pub fn new(config: MeshNodeConfig, conn_mgr: Arc<ConnectionManager>) -> (Self, mpsc::Receiver<MeshNodeEvent>)

// NEW (CS-5/CS-6): Returns broadcast::Receiver instead of mpsc::Receiver
pub fn new(config: MeshNodeConfig, conn_mgr: Arc<ConnectionManager>) -> (Self, broadcast::Receiver<MeshNodeEvent>)
```

### New Methods

| Method | Module | Description |
|--------|--------|-------------|
| `MeshNode::subscribe_events()` | mesh/node.rs | Returns additional `broadcast::Receiver<MeshNodeEvent>` for multiple consumers |
| `MeshNode::config_ref()` | mesh/node.rs | Returns `&MeshNodeConfig` (needed by `runtime.rs`) |
| `MeshNode::emit_event()` | mesh/node.rs | Public broadcast of `MeshNodeEvent` |
| `PrimaryElection::replace_event_tx()` | mesh/election.rs | Replace channel sender for stop/start cycle |
| `BridgeTailnetPeer::to_canonical()` | bridge/protocol.rs | Convert wire-format peer to canonical `TailnetPeer` |
| `TruffleRuntime::new()` | runtime.rs | Create runtime (does not start) |
| `TruffleRuntime::start()` | runtime.rs | Bootstrap sidecar + start mesh node |
| `TruffleRuntime::stop()` | runtime.rs | Stop mesh node + sidecar |
| `TruffleRuntime::mesh_node()` | runtime.rs | Access the inner `Arc<MeshNode>` |
| `TruffleRuntime::connection_manager()` | runtime.rs | Access the inner `Arc<ConnectionManager>` |
| `TruffleRuntime::dial_peer()` | runtime.rs | Full dial pipeline: insert pending -> dial_raw -> await bridge -> WS upgrade |
| `wire_store_sync()` | integration.rs | Wire StoreSyncAdapter to MeshNode with 2 background tasks |
| `wire_file_transfer()` | integration.rs | Wire FileTransferAdapter to MeshNode with 2 background tasks |

### Internal Structural Changes (not public API)

- `MeshNodeLifecycle` struct consolidates 6 `Arc<RwLock<>>` fields into 1 (CS-6)
- `StoreSyncAdapter` `disposed`/`started` changed from `RwLock<bool>` to `AtomicBool` (CS-9)
- `GoShim` has separate `resume_signal: Arc<Notify>` instead of overloading `shutdown` (CS-8)
- `TransportHandler` (pub(crate)) extracted from inline event loop closure (CS-5)
- Dead code methods removed from `MeshNode`: `handle_mesh_message()`, `handle_incoming_data()`, `handle_route_envelope()` (CS-5)
