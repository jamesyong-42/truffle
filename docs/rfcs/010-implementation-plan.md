# RFC 010 Implementation Plan: P2P Mesh Redesign + CLI Daemon

**Status**: Ready for implementation
**Created**: 2026-03-20
**Implements**: RFC 010 (P2P + CLI Redesign)
**Companion**: cli-design.md

---

## Overview

This plan breaks RFC 010 into 11 independently shippable phases. Each phase
leaves the workspace in a compilable, all-tests-passing state. The first three
phases are pure deletions and simplifications; phases 4-11 are additive.

**Total estimated effort: ~95 hours**

### Dependency graph

```
Phase 1 (delete election)
    |
Phase 2 (delete routing)
    |
Phase 3 (simplify handler + MeshNode)
    |
Phase 4 (update events + types + NAPI) ───────────┐
    |                                               |
Phase 5 (Go sidecar additions)                     |
    |                                               |
Phase 6 (CLI daemon architecture) <─────────────────
    |
Phase 7 (CLI: up, down, status)
    |
Phase 8 (CLI: ls, ping + name resolution)
    |
Phase 9 (CLI: tcp, ws, proxy, expose)
    |
Phase 10 (CLI: chat, send)
    |
Phase 11 (CLI: cp, doctor, completion)
    |
Phase 12 (polish: help text, output design, aliases)
```

Phases 4 and 5 can run in parallel. All other phases are sequential.

---

## Phase 1: Delete Election System

**Depends on:** None
**Effort:** 6 hours
**Breaking changes:** Yes -- removes `DeviceRole`, `is_primary()`, `primary_id()`, `role()`, `RoleChanged`, `PrimaryChanged` from public API
**Can ship independently:** Yes (all remaining code paths are valid without election)

### What changes

#### Files DELETED entirely:

- **`crates/truffle-core/src/mesh/election.rs`** (469 lines + ~400 lines of tests) -- entire module removed

#### Files MODIFIED:

- **`crates/truffle-core/src/mesh/mod.rs`**
  - Remove `pub mod election;` line
  - Remove `pub mod routing;` line (preemptive -- routing depends on election concepts)

- **`crates/truffle-core/src/mesh/node.rs`** (~250 lines removed)
  - Remove `use super::election::*` imports
  - Remove `use super::routing` import
  - Remove `election` field from `MeshNode` struct
  - Remove `election_event_rx` field from `MeshNode` struct
  - Remove lock ordering doc comment mentioning election
  - Remove `PrimaryElection::new()` call from `MeshNode::new()`
  - Remove `(election_event_tx, election_event_rx)` channel creation from `new()`
  - Remove `ElectionConfig` setup from `start()` (lines 219-227)
  - Remove `election.reset()` from `stop()` (lines 284-286)
  - Remove election channel recreation from `stop()` (lines 293-296)
  - Remove `is_primary()`, `primary_id()`, `role()` methods (lines 331-345)
  - Remove `ViaPrimary` fallback from `send_envelope()` (lines 371-379)
  - Remove primary/secondary branching from `broadcast_envelope()` (lines 389-414)
  - Remove entire election event processing task from `start_event_loop()` (lines 699-832)
  - Remove election trigger logic from `handle_tailnet_peers()` (lines 541-561)
  - Remove `election` clone in `start_event_loop()` (line 660)
  - Remove `ElectionTimingConfig` from `MeshTimingConfig` struct (field `election`)
  - Remove `prefer_primary` from `MeshNodeConfig`

- **`crates/truffle-core/src/mesh/handler.rs`**
  - Remove `use super::election::PrimaryElection` import
  - Remove `election` field from `TransportHandler` struct
  - Remove election-dependent code from `handle_connected()` (lines 97-123: device-list sending when primary)
  - Remove `ElectionStart`, `ElectionCandidate`, `ElectionResult` arms from `dispatch_mesh_message()` (lines 254-270)
  - Remove `RouteMessage`/`RouteBroadcast` warning arms from `dispatch_mesh_message()` (lines 272-280)
  - Remove split-brain detection from `DeviceList` handler in `dispatch_mesh_message()` (lines 227-245)
  - Remove `handle_route_envelope()` entirely (lines 292-445)
  - Simplify `handle_message()` -- remove route envelope branch (lines 168-175)

- **`crates/truffle-core/src/runtime.rs`**
  - Remove `use crate::mesh::election::ElectionTimingConfig` import
  - Remove `use crate::types::DeviceRole` import
  - Remove `RoleChanged` variant from `TruffleEvent` enum
  - Remove `PrimaryChanged` variant from `TruffleEvent` enum
  - Remove `prefer_primary` field from `TruffleRuntimeBuilder`
  - Remove `election_timeout` field from `TruffleRuntimeBuilder`
  - Remove `primary_loss_grace` field from `TruffleRuntimeBuilder`
  - Remove `primary_candidate()` builder method
  - Remove `election_timeout()` builder method
  - Remove `primary_loss_grace()` builder method
  - Remove `ElectionTimingConfig` construction from `build()`
  - Remove `election` field from `MeshTimingConfig` construction in `build()`
  - Remove `prefer_primary` from `MeshNodeConfig` construction in `build()`
  - Remove `is_primary()` convenience method from `TruffleRuntime`
  - Remove `RoleChanged` arm from `mesh_event_to_truffle()`
  - Remove `PrimaryChanged` arm from `mesh_event_to_truffle()`

### Key code changes

```rust
// mesh/node.rs -- AFTER
pub struct MeshTimingConfig {
    pub announce_interval: Duration,
    pub discovery_timeout: Duration,
    // No election field
}

pub struct MeshNodeConfig {
    pub device_id: String,
    pub device_name: String,
    pub device_type: String,
    pub hostname_prefix: String,
    // No prefer_primary field
    pub capabilities: Vec<String>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    pub timing: MeshTimingConfig,
}

pub struct MeshNode {
    config: MeshNodeConfig,
    device_manager: Arc<RwLock<DeviceManager>>,
    // No election field
    connection_manager: Arc<ConnectionManager>,
    message_bus: Arc<MeshMessageBus>,
    lifecycle: Arc<RwLock<MeshNodeLifecycle>>,
    event_tx: broadcast::Sender<MeshNodeEvent>,
    device_event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<DeviceEvent>>>,
    // No election_event_rx field
}
```

```rust
// mesh/handler.rs -- AFTER
pub(crate) struct TransportHandler {
    pub config: MeshNodeConfig,
    pub connection_manager: Arc<ConnectionManager>,
    pub device_manager: Arc<RwLock<DeviceManager>>,
    // No election field
    pub event_tx: broadcast::Sender<MeshNodeEvent>,
    pub message_bus: Arc<MeshMessageBus>,
}
```

```rust
// mesh/node.rs -- simplified send_envelope
pub async fn send_envelope(&self, device_id: &str, envelope: &MeshEnvelope) -> bool {
    let local_id = self.config.device_id.clone();
    if device_id == local_id {
        let _ = self.event_tx.send(MeshNodeEvent::Message(IncomingMeshMessage {
            from: Some(local_id),
            connection_id: "local".to_string(),
            namespace: envelope.namespace.clone(),
            msg_type: envelope.msg_type.clone(),
            payload: envelope.payload.clone(),
        }));
        return true;
    }
    self.send_envelope_direct(device_id, envelope).await
}

// mesh/node.rs -- simplified broadcast_envelope
pub async fn broadcast_envelope(&self, envelope: &MeshEnvelope) {
    let local_id = self.config.device_id.clone();
    let conns = self.connection_manager.get_connections().await;
    for conn in &conns {
        if let Some(ref did) = conn.device_id {
            if conn.status == ConnectionStatus::Connected {
                self.send_envelope_direct(did, envelope).await;
            }
        }
    }
    let _ = self.event_tx.send(MeshNodeEvent::Message(IncomingMeshMessage {
        from: Some(local_id),
        connection_id: "local".to_string(),
        namespace: envelope.namespace.clone(),
        msg_type: envelope.msg_type.clone(),
        payload: envelope.payload.clone(),
    }));
}

// mesh/node.rs -- simplified handle_tailnet_peers
pub async fn handle_tailnet_peers(&self, peers: &[TailnetPeer]) {
    let identity = self.device_manager.read().await.device_identity().clone();
    tracing::info!("Discovered {} tailnet peers", peers.len());
    for peer in peers {
        if peer.hostname == identity.tailscale_hostname { continue; }
        let device = {
            let mut dm = self.device_manager.write().await;
            dm.add_discovered_peer(peer)
        };
        if let Some(device) = device {
            if peer.online {
                let conn = self.connection_manager.get_connection_by_device(&device.id).await;
                if conn.is_none() || conn.map(|c| c.status) != Some(ConnectionStatus::Connected) {
                    tracing::info!("Should connect to peer {} ({})", device.name, device.id);
                }
            }
        }
    }
    // No election trigger
}

// mesh/node.rs -- simplified start_event_loop (device events only, no election events)
async fn start_event_loop(&self) {
    let mut transport_rx = self.connection_manager.subscribe();
    let mut device_event_rx = self.device_event_rx.lock().await.take()
        .expect("device_event_rx already taken");

    let event_tx = self.event_tx.clone();
    let connection_manager = self.connection_manager.clone();
    let device_manager = self.device_manager.clone();
    let config = self.config.clone();
    let message_bus = self.message_bus.clone();

    // Device event task (simplified -- no election interaction)
    let event_tx_dev = event_tx.clone();
    tokio::spawn(async move {
        while let Some(event) = device_event_rx.recv().await {
            match &event {
                DeviceEvent::DeviceDiscovered(d) =>
                    { let _ = event_tx_dev.send(MeshNodeEvent::DeviceDiscovered(d.clone())); }
                DeviceEvent::DeviceUpdated(d) =>
                    { let _ = event_tx_dev.send(MeshNodeEvent::DeviceUpdated(d.clone())); }
                DeviceEvent::DeviceOffline(id) =>
                    { let _ = event_tx_dev.send(MeshNodeEvent::DeviceOffline(id.clone())); }
                DeviceEvent::DevicesChanged(devs) =>
                    { let _ = event_tx_dev.send(MeshNodeEvent::DevicesChanged(devs.clone())); }
                DeviceEvent::PrimaryChanged(_) => { /* no-op, primary concept removed */ }
                DeviceEvent::LocalDeviceChanged(_) => { /* handled by announce interval */ }
            }
        }
    });

    // Transport event task (unchanged)
    // ... handler setup ...
}
```

```rust
// runtime.rs -- simplified TruffleRuntimeBuilder::build()
pub fn build(self) -> Result<(TruffleRuntime, broadcast::Receiver<MeshNodeEvent>), RuntimeError> {
    let hostname_prefix = self.hostname_prefix
        .ok_or_else(|| RuntimeError::Bootstrap("hostname is required".into()))?;
    let device_id = self.device_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let device_name = self.device_name.unwrap_or_else(|| format!("{hostname_prefix}-node"));
    let device_type = self.device_type.unwrap_or_else(|| "desktop".to_string());

    let mesh_timing = MeshTimingConfig {
        announce_interval: self.announce_interval
            .unwrap_or_else(|| MeshTimingConfig::default().announce_interval),
        discovery_timeout: self.discovery_timeout
            .unwrap_or_else(|| MeshTimingConfig::default().discovery_timeout),
        // No election field
    };

    let mesh_config = MeshNodeConfig {
        device_id,
        device_name,
        device_type,
        hostname_prefix,
        // No prefer_primary field
        capabilities: self.capabilities,
        metadata: self.metadata,
        timing: mesh_timing,
    };
    // ... rest unchanged ...
}
```

### Tests

**Delete:**
- All tests in `crates/truffle-core/src/mesh/election.rs` `#[cfg(test)]` module (~400 lines)
- Election-related tests in `crates/truffle-core/src/mesh/handler.rs` tests:
  - `test_dispatch_election_start` (if exists)
  - `test_dispatch_election_candidate` (if exists)
  - `test_dispatch_election_result` (if exists)
  - `test_route_message_handling` (if exists)
  - `test_route_broadcast_handling` (if exists)
  - `test_split_brain_detection` (if exists)
- Any test that constructs `PrimaryElection` in handler test helpers

**Update:**
- `crates/truffle-core/src/mesh/handler.rs` -- `make_handler()` test helper: remove `election` field from `TransportHandler` construction, remove election channel creation
- `crates/truffle-core/tests/mesh_integration.rs` -- ALL tests reference election and role concepts:
  - `test_single_node_self_elects` -- rewrite as `test_single_node_starts_and_announces` (just verify Started event, no RoleChanged)
  - `test_two_node_election` -- rewrite as `test_two_node_discovery` (verify DeviceDiscovered events, no election)
  - `test_primary_failover` -- delete entirely (no failover in P2P)
  - `test_message_delivery_two_nodes` -- update to remove election wait, just wait for DeviceDiscovered
  - `test_broadcast_two_nodes` -- update to remove election wait
- `crates/truffle-core/src/runtime.rs` `#[cfg(test)]` module:
  - Remove tests asserting `TruffleEvent::RoleChanged`
  - Remove tests asserting `TruffleEvent::PrimaryChanged`
  - Remove `builder_primary_candidate_maps_to_prefer_primary` test
  - Update `mesh_event_to_truffle` mapping tests to remove RoleChanged/PrimaryChanged cases
  - Update builder default test to not check `prefer_primary`

**Add:**
- `test_p2p_broadcast_fanout` -- verify broadcast goes to all connected peers directly
- `test_p2p_send_direct` -- verify send to a peer works without routing
- `test_p2p_send_no_connection_fails` -- verify send to unconnected peer returns false (no ViaPrimary fallback)

### Verification

1. `cargo check --workspace` -- must pass with no errors
2. `cargo test --workspace` -- all surviving tests pass
3. `cargo clippy --workspace` -- no new warnings
4. Verify `election.rs` and `routing.rs` are no longer in the module tree
5. Verify no remaining `use` of `PrimaryElection`, `ElectionConfig`, `ElectionEvent`, `ElectionTimingConfig`, `DeviceRole` in `truffle-core` (grep for these symbols)

---

## Phase 2: Delete Routing Module

**Depends on:** Phase 1
**Effort:** 3 hours
**Breaking changes:** No additional (Phase 1 already removed routing usage)
**Can ship independently:** Yes

### What changes

#### Files DELETED entirely:

- **`crates/truffle-core/src/mesh/routing.rs`** (160 lines, including ~65 lines of tests)

#### Files MODIFIED:

- **`crates/truffle-core/src/mesh/mod.rs`** -- already removed `pub mod routing;` in Phase 1 (verify)
- **`crates/truffle-core/src/mesh/node.rs`** -- remove any remaining `use super::routing` import (should already be gone from Phase 1)

### Key code changes

This is a pure deletion phase. The `routing.rs` file provides:
- `RouteDecision` enum -- no longer used
- `route_to_device()` function -- no longer called
- `wrap_route_message()` function -- no longer called
- `wrap_route_broadcast()` function -- no longer called

All callers were removed in Phase 1 when `send_envelope()` and `broadcast_envelope()` were simplified.

### Tests

**Delete:**
- All tests in `crates/truffle-core/src/mesh/routing.rs` `#[cfg(test)]` module:
  - `route_to_self_is_local`
  - `route_with_direct_connection`
  - `secondary_routes_via_primary`
  - `secondary_no_primary_is_unroutable`
  - `primary_no_direct_connection_is_unroutable`
  - `wrap_route_message_creates_envelope`
  - `wrap_route_broadcast_creates_envelope`

### Verification

1. `cargo check --workspace`
2. `cargo test --workspace`
3. `grep -r "routing" crates/truffle-core/src/mesh/` -- should find nothing

---

## Phase 3: Simplify Wire Protocol Types

**Depends on:** Phase 1, Phase 2
**Effort:** 5 hours
**Breaking changes:** Yes -- wire protocol break (5 message types removed, `device-list` format changed)
**Can ship independently:** Yes

### What changes

- **`crates/truffle-core/src/protocol/types.rs`**
  - Remove `ElectionStart`, `ElectionCandidate`, `ElectionResult`, `RouteMessage`, `RouteBroadcast` from `MeshMessageType` enum (keep 3: `DeviceAnnounce`, `DeviceList`, `DeviceGoodbye`)
  - Remove corresponding arms from `MeshMessageType::from_str()`
  - Remove corresponding arms from `MeshMessageType::as_str()`
  - Remove `ElectionStart`, `ElectionCandidate(..)`, `ElectionResult(..)`, `RouteMessage(..)`, `RouteBroadcast(..)` from `MeshPayload` enum (keep 3: `DeviceAnnounce`, `DeviceList`, `DeviceGoodbye`)
  - Remove corresponding arms from `MeshPayload::parse()`
  - Remove corresponding arms from `MeshPayload::message_type()`
  - Remove imports of deleted payload types (`ElectionCandidatePayload`, `ElectionResultPayload`, `RouteBroadcastPayload`, `RouteMessagePayload`)

- **`crates/truffle-core/src/protocol/message_types.rs`**
  - Delete `ElectionCandidatePayload` struct
  - Delete `ElectionResultPayload` struct
  - Delete `RouteMessagePayload` struct
  - Delete `RouteBroadcastPayload` struct
  - Remove `primary_id` field from `DeviceListPayload`

- **`crates/truffle-core/src/types/mod.rs`**
  - Delete `DeviceRole` enum entirely
  - Remove `role: Option<DeviceRole>` field from `BaseDevice`

- **`crates/truffle-core/src/mesh/device.rs`**
  - Remove `use crate::types::DeviceRole` import
  - Remove `set_local_role()` method
  - Remove `set_device_role()` method
  - Remove `primary_device()` method
  - Remove `primary_id()` method
  - Remove `primary_id` field from `DeviceManager`
  - Remove `PrimaryChanged` variant from `DeviceEvent` enum
  - Simplify `handle_device_list()` -- remove all `primary_id` logic (lines 282-299)
  - Simplify `mark_device_offline()` -- remove primary tracking (lines 315-318)
  - Remove `role` field from test `BaseDevice` construction

- **`crates/truffle-core/src/mesh/handler.rs`**
  - Simplify `dispatch_mesh_message()` DeviceList handler: remove `primary_id` checks

### Key code changes

```rust
// protocol/types.rs -- AFTER
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeshMessageType {
    DeviceAnnounce,
    DeviceList,
    DeviceGoodbye,
}

impl MeshMessageType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "device-announce" => Some(Self::DeviceAnnounce),
            "device-list"     => Some(Self::DeviceList),
            "device-goodbye"  => Some(Self::DeviceGoodbye),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DeviceAnnounce => "device-announce",
            Self::DeviceList     => "device-list",
            Self::DeviceGoodbye  => "device-goodbye",
        }
    }
}

#[derive(Debug, Clone)]
pub enum MeshPayload {
    DeviceAnnounce(DeviceAnnouncePayload),
    DeviceList(DeviceListPayload),
    DeviceGoodbye(DeviceGoodbyePayload),
}
```

```rust
// protocol/message_types.rs -- AFTER (DeviceListPayload)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceListPayload {
    pub devices: Vec<BaseDevice>,
    // No primary_id field
}
```

```rust
// types/mod.rs -- AFTER (BaseDevice)
pub struct BaseDevice {
    pub id: String,
    #[serde(rename = "type")]
    pub device_type: String,
    pub name: String,
    pub tailscale_hostname: String,
    pub tailscale_dns_name: Option<String>,
    pub tailscale_ip: Option<String>,
    // No role field
    pub status: DeviceStatus,
    pub capabilities: Vec<String>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    pub last_seen: Option<u64>,
    pub started_at: Option<u64>,
    pub os: Option<String>,
    pub latency_ms: Option<f64>,
}
```

```rust
// mesh/device.rs -- simplified DeviceManager
pub struct DeviceManager {
    identity: DeviceIdentity,
    hostname_prefix: String,
    local_device: BaseDevice,
    devices: HashMap<String, BaseDevice>,
    // No primary_id field
    event_tx: mpsc::Sender<DeviceEvent>,
}

pub enum DeviceEvent {
    DeviceDiscovered(BaseDevice),
    DeviceUpdated(BaseDevice),
    DeviceOffline(String),
    DevicesChanged(Vec<BaseDevice>),
    // No PrimaryChanged variant
    LocalDeviceChanged(BaseDevice),
}

// Simplified handle_device_list -- just update device entries
pub fn handle_device_list(&mut self, _from: &str, payload: &DeviceListPayload) {
    for device in &payload.devices {
        if device.id == self.identity.id { continue; }
        let mut new_device = device.clone();
        if new_device.tailscale_dns_name.is_none() {
            if let Some(existing) = self.devices.get(&device.id) {
                new_device.tailscale_dns_name.clone_from(&existing.tailscale_dns_name);
            }
        }
        self.devices.insert(device.id.clone(), new_device);
    }
    self.emit(DeviceEvent::DevicesChanged(self.devices()));
}

// Simplified mark_device_offline -- no primary tracking
pub fn mark_device_offline(&mut self, device_id: &str) {
    if let Some(device) = self.devices.get_mut(device_id) {
        device.status = DeviceStatus::Offline;
        self.emit(DeviceEvent::DeviceOffline(device_id.to_string()));
        self.emit(DeviceEvent::DevicesChanged(self.devices()));
    }
}
```

### Tests

**Delete:**
- `crates/truffle-core/src/protocol/types.rs` tests:
  - `mesh_payload_parse_election_start`
  - `mesh_payload_parse_election_start_legacy_rejected`
  - `mesh_payload_parse_election_candidate`
  - `mesh_payload_parse_election_result`
  - `mesh_payload_parse_route_message`
  - `mesh_payload_parse_route_broadcast`
  - `mesh_payload_parse_route_message_legacy_rejected`
  - All serde roundtrip tests referencing removed variants
- `crates/truffle-core/src/protocol/message_types.rs` tests:
  - `election_candidate_payload_serde`
  - Any test referencing deleted payload types
- `crates/truffle-core/src/types/mod.rs` tests:
  - `DeviceRole` serde roundtrip test
  - Any test constructing `BaseDevice` with `role` field
- `crates/truffle-core/src/mesh/device.rs` tests:
  - Any test calling `set_local_role()`, `set_device_role()`, `primary_id()`

**Update:**
- `mesh_message_type_from_str_kebab_case` -- remove 5 assertion lines
- `mesh_message_type_as_str` -- remove 5 assertion lines
- `mesh_message_type_serde_roundtrip_all_variants` -- reduce to 3 variants
- `mesh_message_type_serde_wire_strings` -- remove deleted variant assertions
- `mesh_message_type_from_str_roundtrip_via_as_str` -- reduce to 3 variants
- `mesh_message_type_msgpack_roundtrip` -- reduce to 3 variants
- `mesh_payload_parse_device_list` -- remove `primaryId` from test JSON payload
- `mesh_payload_message_type_all_variants` -- remove election_start assertion
- All `BaseDevice` construction in tests -- remove `role` field
- All `DeviceListPayload` construction in tests -- remove `primary_id` field
- `crates/truffle-core/src/mesh/handler.rs` `make_handler()` -- remove `role` from test device construction

### Verification

1. `cargo check --workspace`
2. `cargo test --workspace`
3. `grep -rn "DeviceRole\|ElectionCandidate\|ElectionResult\|RouteMessage\|RouteBroadcast\|primary_id" crates/truffle-core/src/protocol/` -- should find nothing
4. `grep -rn "DeviceRole" crates/truffle-core/src/types/` -- should find nothing
5. Verify `DeviceListPayload` serialization no longer includes `primaryId`

---

## Phase 4: Update Events and NAPI Bindings

**Depends on:** Phase 3
**Effort:** 8 hours
**Breaking changes:** Yes -- event names change (`Device*` -> `Peer*`), role events removed, new connection events added
**Can ship independently:** Yes

### What changes

- **`crates/truffle-core/src/mesh/node.rs`** -- `MeshNodeEvent` enum:
  - Rename `DeviceDiscovered` -> `PeerDiscovered`
  - Rename `DeviceUpdated` -> `PeerUpdated`
  - Rename `DeviceOffline` -> `PeerOffline`
  - Rename `DevicesChanged` -> `PeersChanged`
  - Remove `RoleChanged` (already removed in Phase 1)
  - Remove `PrimaryChanged` (already removed in Phase 1)
  - Add `PeerConnected { connection_id: String, peer_dns: Option<String> }`
  - Add `PeerDisconnected { connection_id: String, reason: String }`

- **`crates/truffle-core/src/mesh/handler.rs`**
  - Emit `MeshNodeEvent::PeerConnected` at end of `handle_connected()`
  - Emit `MeshNodeEvent::PeerDisconnected` at end of `handle_disconnected()`

- **`crates/truffle-core/src/mesh/node.rs`** -- event loop device event mapping:
  - `DeviceEvent::DeviceDiscovered` -> `MeshNodeEvent::PeerDiscovered`
  - `DeviceEvent::DeviceUpdated` -> `MeshNodeEvent::PeerUpdated`
  - `DeviceEvent::DeviceOffline` -> `MeshNodeEvent::PeerOffline`
  - `DeviceEvent::DevicesChanged` -> `MeshNodeEvent::PeersChanged`

- **`crates/truffle-core/src/runtime.rs`**
  - Rename `TruffleEvent::DeviceDiscovered` -> `TruffleEvent::PeerDiscovered`
  - Rename `TruffleEvent::DeviceUpdated` -> `TruffleEvent::PeerUpdated`
  - Rename `TruffleEvent::DeviceOffline` -> `TruffleEvent::PeerOffline`
  - Rename `TruffleEvent::DevicesChanged` -> `TruffleEvent::PeersChanged`
  - Add `TruffleEvent::PeerConnected { connection_id: String, peer_dns: Option<String> }`
  - Add `TruffleEvent::PeerDisconnected { connection_id: String, reason: String }`
  - Remove `TruffleEvent::RoleChanged` (if not already done in Phase 1)
  - Remove `TruffleEvent::PrimaryChanged` (if not already done in Phase 1)
  - Update `mesh_event_to_truffle()` mapping function

- **`crates/truffle-napi/src/mesh_node.rs`**
  - Remove `is_primary()` method
  - Remove `primary_id()` method
  - Remove `role()` method
  - Remove `ElectionTimingConfig` import
  - Remove `election_timeout_ms` and `primary_loss_grace_ms` from NAPI timing config mapping
  - Remove `prefer_primary` from NAPI config mapping
  - Remove `election` field from `MeshTimingConfig` construction

- **`crates/truffle-napi/src/types.rs`**
  - Remove `prefer_primary` from `NapiMeshNodeConfig`
  - Remove `election_timeout_ms` and `primary_loss_grace_ms` from `NapiMeshTimingConfig`
  - Remove `role` field from `NapiBaseDevice`
  - Remove `DeviceRole` import and role-to-string conversion in `From<BaseDevice>` for `NapiBaseDevice`
  - Update `mesh_event_to_napi()`:
    - `DeviceDiscovered` -> use `"peerDiscovered"` event type
    - `DeviceUpdated` -> use `"peerUpdated"` event type
    - `DeviceOffline` -> use `"peerOffline"` event type
    - `DevicesChanged` -> use `"peersChanged"` event type
    - Remove `RoleChanged` arm
    - Remove `PrimaryChanged` arm
    - Add `PeerConnected` arm
    - Add `PeerDisconnected` arm
  - Update `truffle_event_to_napi()` with same changes

### Key code changes

```rust
// mesh/node.rs -- AFTER
pub enum MeshNodeEvent {
    Started,
    Stopped,
    AuthRequired(String),
    AuthComplete,
    PeerDiscovered(BaseDevice),
    PeerUpdated(BaseDevice),
    PeerOffline(String),
    PeersChanged(Vec<BaseDevice>),
    PeerConnected { connection_id: String, peer_dns: Option<String> },
    PeerDisconnected { connection_id: String, reason: String },
    Message(IncomingMeshMessage),
    Error(String),
}
```

```rust
// runtime.rs -- AFTER
pub enum TruffleEvent {
    Started,
    Stopped,
    AuthRequired { auth_url: String },
    AuthComplete,
    Online { ip: String, dns_name: String },
    SidecarStarted,
    SidecarStopped,
    SidecarCrashed { exit_code: Option<i32>, stderr_tail: String },
    SidecarStateChanged { state: String },
    SidecarNeedsApproval,
    SidecarKeyExpiring { expires_at: String },
    SidecarHealthWarning { warnings: Vec<String> },
    PeerDiscovered(BaseDevice),
    PeerUpdated(BaseDevice),
    PeerOffline(String),
    PeersChanged(Vec<BaseDevice>),
    PeerConnected { connection_id: String, peer_dns: Option<String> },
    PeerDisconnected { connection_id: String, reason: String },
    Message(IncomingMeshMessage),
    Error(String),
    // NO RoleChanged, NO PrimaryChanged
}
```

```rust
// handler.rs -- emit new events
pub async fn handle_connected(&self, conn: &WSConnection) {
    tracing::info!("Transport connected: {} ({:?})", conn.id, conn.direction);
    // Send device announce (unchanged)
    // ...
    // Emit PeerConnected event
    let _ = self.event_tx.send(MeshNodeEvent::PeerConnected {
        connection_id: conn.id.clone(),
        peer_dns: Some(conn.remote_dns_name.clone()).filter(|s| !s.is_empty()),
    });
}

pub async fn handle_disconnected(&self, connection_id: &str, reason: &str) {
    tracing::info!("Transport disconnected: {connection_id} ({reason})");
    // Mark devices offline (unchanged)
    // ...
    // Emit PeerDisconnected event
    let _ = self.event_tx.send(MeshNodeEvent::PeerDisconnected {
        connection_id: connection_id.to_string(),
        reason: reason.to_string(),
    });
}
```

### Tests

**Delete:**
- Any test asserting `MeshNodeEvent::RoleChanged` (should be gone from Phase 1)
- Any test asserting `MeshNodeEvent::PrimaryChanged`
- Any test asserting `TruffleEvent::RoleChanged`
- Any test asserting `TruffleEvent::PrimaryChanged`

**Update:**
- All tests asserting `MeshNodeEvent::DeviceDiscovered` -> `MeshNodeEvent::PeerDiscovered`
- All tests asserting `MeshNodeEvent::DeviceUpdated` -> `MeshNodeEvent::PeerUpdated`
- All tests asserting `MeshNodeEvent::DeviceOffline` -> `MeshNodeEvent::PeerOffline`
- All tests asserting `MeshNodeEvent::DevicesChanged` -> `MeshNodeEvent::PeersChanged`
- All tests asserting `TruffleEvent::DeviceDiscovered` -> `TruffleEvent::PeerDiscovered`
- Integration tests in `tests/mesh_integration.rs` -- update event matching
- NAPI type conversion tests

**Add:**
- `test_peer_connected_event_on_ws_connect` -- verify `PeerConnected` fires when transport connects
- `test_peer_disconnected_event_on_ws_close` -- verify `PeerDisconnected` fires when transport disconnects
- `test_napi_peer_connected_conversion` -- verify NAPI event mapping
- `test_napi_peer_disconnected_conversion`

### Verification

1. `cargo check --workspace` (including `truffle-napi` crate)
2. `cargo test --workspace`
3. `grep -rn "DeviceDiscovered\|DeviceUpdated\|DeviceOffline\|DevicesChanged\|RoleChanged\|PrimaryChanged" crates/truffle-core/src/ crates/truffle-napi/src/` -- should find nothing (only `PeerDiscovered` etc.)
4. `grep -rn "DeviceRole" crates/truffle-napi/` -- should find nothing

---

## Phase 5: Go Sidecar Additions

**Depends on:** Phase 3 (needs types to be stabilized)
**Effort:** 10 hours
**Breaking changes:** No (additive only)
**Can ship independently:** Yes (can run in parallel with Phase 4)

### What changes

- **`packages/sidecar-slim/main.go`**
  - Add `tsnet:listen` command handler: creates a `tsnet.Server.Listen()` on specified port, starts accept loop, bridges connections back to Rust
  - Add `tsnet:unlisten` command handler: closes the listener for a port
  - Add `tsnet:ping` command handler: calls `tsnet.Server.LocalClient().Ping()` and returns latency
  - Add `tsnet:pushFile` command handler: sends file via Taildrop
  - Add `tsnet:waitingFiles` command handler: lists waiting Taildrop files
  - Add `tsnet:getWaitingFile` command handler: retrieves a Taildrop file
  - Add `tsnet:deleteWaitingFile` command handler: deletes a Taildrop file
  - Add listener tracking map: `map[uint16]net.Listener`
  - Add file transfer state (temporary directory for incoming files)

- **`crates/truffle-core/src/bridge/protocol.rs`** -- add new command/event types:
  - Add command type constants: `LISTEN`, `UNLISTEN`, `PING`, `PUSH_FILE`, `WAITING_FILES`, `GET_WAITING_FILE`, `DELETE_WAITING_FILE`
  - Add event type constants: `LISTENING`, `UNLISTENED`, `PING_RESULT`, `PUSH_FILE_PROGRESS`, `WAITING_FILES_LIST`, `DELETED_WAITING_FILE`
  - Add data structs: `ListenCommandData`, `UnlistenCommandData`, `PingCommandData`, `PushFileCommandData`, `WaitingFilesCommandData`, `GetWaitingFileCommandData`, `DeleteWaitingFileCommandData`
  - Add event data structs: `ListeningEventData`, `UnlistenedEventData`, `PingResultEventData`, `PushFileProgressEventData`, `WaitingFilesListEventData`, `DeletedWaitingFileEventData`

- **`crates/truffle-core/src/bridge/shim.rs`** -- add new command methods:
  - `pub async fn listen(&self, port: u16, tls: bool) -> Result<(), ShimError>`
  - `pub async fn unlisten(&self, port: u16) -> Result<(), ShimError>`
  - `pub async fn ping(&self, target: &str) -> Result<PingResult, ShimError>`
  - `pub async fn push_file(&self, target: &str, name: &str, size: u64) -> Result<(), ShimError>`
  - `pub async fn waiting_files(&self) -> Result<Vec<WaitingFile>, ShimError>`
  - `pub async fn get_waiting_file(&self, name: &str) -> Result<Vec<u8>, ShimError>`
  - `pub async fn delete_waiting_file(&self, name: &str) -> Result<(), ShimError>`

### Key code changes

```go
// main.go -- new command handlers (signatures)
func (s *shim) handleListen(data json.RawMessage)
func (s *shim) handleUnlisten(data json.RawMessage)
func (s *shim) handlePing(data json.RawMessage)
func (s *shim) handlePushFile(data json.RawMessage)
func (s *shim) handleWaitingFiles(data json.RawMessage)
func (s *shim) handleGetWaitingFile(data json.RawMessage)
func (s *shim) handleDeleteWaitingFile(data json.RawMessage)

// main.go -- new types
type listenData struct {
    Port uint16 `json:"port"`
    TLS  bool   `json:"tls,omitempty"`
}

type pingData struct {
    Target string `json:"target"`
}

type pingResultData struct {
    LatencyMs float64 `json:"latencyMs"`
    Direct    bool    `json:"direct"`
    Relay     string  `json:"relay,omitempty"`
    Error     string  `json:"error,omitempty"`
}

type pushFileData struct {
    Target string `json:"target"`
    Name   string `json:"name"`
    Size   int64  `json:"size"`
}

type waitingFileInfo struct {
    Name string `json:"name"`
    Size int64  `json:"size"`
}
```

```rust
// bridge/protocol.rs -- new constants
pub mod command_type {
    // existing...
    pub const LISTEN: &str = "tsnet:listen";
    pub const UNLISTEN: &str = "tsnet:unlisten";
    pub const PING: &str = "tsnet:ping";
    pub const PUSH_FILE: &str = "tsnet:pushFile";
    pub const WAITING_FILES: &str = "tsnet:waitingFiles";
    pub const GET_WAITING_FILE: &str = "tsnet:getWaitingFile";
    pub const DELETE_WAITING_FILE: &str = "tsnet:deleteWaitingFile";
}

pub mod event_type {
    // existing...
    pub const LISTENING: &str = "tsnet:listening";
    pub const UNLISTENED: &str = "tsnet:unlistened";
    pub const PING_RESULT: &str = "tsnet:pingResult";
    pub const PUSH_FILE_PROGRESS: &str = "tsnet:pushFileProgress";
    pub const WAITING_FILES_LIST: &str = "tsnet:waitingFilesList";
    pub const DELETED_WAITING_FILE: &str = "tsnet:deletedWaitingFile";
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PingCommandData {
    pub target: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResultEventData {
    pub latency_ms: f64,
    pub direct: bool,
    #[serde(default)]
    pub relay: String,
    #[serde(default)]
    pub error: String,
}
```

### Tests

**Add:**
- `test_listen_command_serialization` -- verify JSON format
- `test_unlisten_command_serialization`
- `test_ping_command_serialization`
- `test_ping_result_deserialization`
- `test_push_file_command_serialization`
- `test_waiting_files_list_deserialization`
- Go sidecar unit tests for new handlers (in `main_test.go` or similar)

### Verification

1. `cargo check --workspace`
2. `cargo test --workspace`
3. `cd packages/sidecar-slim && go build` -- must compile
4. `cd packages/sidecar-slim && go test ./...` -- Go tests pass
5. Manual test: start sidecar, send `tsnet:ping` command, verify `tsnet:pingResult` response

---

## Phase 6: CLI Daemon Architecture

**Depends on:** Phase 4 (needs simplified events and types)
**Effort:** 14 hours
**Breaking changes:** No (additive -- new daemon module)
**Can ship independently:** Yes (daemon starts, accepts socket connections, responds to `status` and `shutdown`)

### What changes

#### New files:

- **`crates/truffle-cli/src/daemon/mod.rs`** -- module declaration
- **`crates/truffle-cli/src/daemon/server.rs`** -- `DaemonServer` struct: Unix socket listener, JSON-RPC dispatch, `TruffleRuntime` ownership
- **`crates/truffle-cli/src/daemon/client.rs`** -- `DaemonClient` struct: connect to socket, send request, read response
- **`crates/truffle-cli/src/daemon/protocol.rs`** -- `DaemonRequest`, `DaemonResponse`, `DaemonError`, method constants, streaming upgrade protocol
- **`crates/truffle-cli/src/daemon/pid.rs`** -- PID file management (write, read, check, remove, stale detection)
- **`crates/truffle-cli/src/config.rs`** -- `TruffleConfig`, `NodeConfig`, `OutputConfig`, `DaemonConfig`, TOML parsing, default config creation
- **`crates/truffle-cli/src/target.rs`** -- `Target` struct, `Target::parse()`, unified target addressing

#### Modified files:

- **`crates/truffle-cli/src/main.rs`** -- rewrite with new `clap` structure per RFC 010 Section 3.9
- **`crates/truffle-cli/Cargo.toml`** -- add dependencies: `serde`, `toml`, `dirs`, `nix` (for Unix socket), `serde_json`

### Key code changes

```rust
// daemon/protocol.rs
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonRequest {
    pub jsonrpc: String,  // "2.0"
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub jsonrpc: String,  // "2.0"
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DaemonError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonError {
    pub code: i32,
    pub message: String,
}

pub mod method {
    pub const STATUS: &str = "status";
    pub const PEERS: &str = "peers";
    pub const SHUTDOWN: &str = "shutdown";
    pub const TCP_CONNECT: &str = "tcp_connect";
    pub const WS_CONNECT: &str = "ws_connect";
    pub const PROXY_START: &str = "proxy_start";
    pub const PROXY_STOP: &str = "proxy_stop";
    pub const EXPOSE_START: &str = "expose_start";
    pub const EXPOSE_STOP: &str = "expose_stop";
    pub const PING: &str = "ping";
    pub const SEND_MESSAGE: &str = "send_message";
}
```

```rust
// daemon/server.rs
pub struct DaemonServer {
    runtime: Arc<TruffleRuntime>,
    socket_path: PathBuf,
    pid_path: PathBuf,
    shutdown: Arc<Notify>,
}

impl DaemonServer {
    pub async fn start(config: &TruffleConfig) -> Result<Self, DaemonError>;
    pub async fn run(&self) -> Result<(), DaemonError>; // accept loop
    async fn handle_connection(&self, stream: UnixStream);
    async fn dispatch(&self, request: DaemonRequest) -> DaemonResponse;
    pub async fn shutdown(&self);
}
```

```rust
// daemon/client.rs
pub struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    pub fn new(socket_path: PathBuf) -> Self;
    pub async fn connect(&self) -> Result<UnixStream, ClientError>;
    pub async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, ClientError>;
    pub fn is_daemon_running(&self) -> bool;
}
```

```rust
// daemon/pid.rs
pub fn write_pid(path: &Path) -> io::Result<()>;
pub fn read_pid(path: &Path) -> io::Result<Option<u32>>;
pub fn is_process_running(pid: u32) -> bool;
pub fn remove_pid(path: &Path) -> io::Result<()>;
pub fn check_stale_pid(path: &Path) -> io::Result<bool>;
```

```rust
// config.rs
#[derive(Debug, Deserialize, Default)]
pub struct TruffleConfig {
    #[serde(default)]
    pub node: NodeConfig,
    #[serde(default)]
    pub aliases: HashMap<String, String>,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
}

impl TruffleConfig {
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError>;
    pub fn default_path() -> PathBuf;  // ~/.config/truffle/config.toml
    pub fn socket_path() -> PathBuf;   // ~/.config/truffle/truffle.sock
    pub fn pid_path() -> PathBuf;      // ~/.config/truffle/truffle.pid
}
```

```rust
// target.rs
pub struct Target {
    pub node: String,
    pub port: Option<u16>,
    pub path: Option<String>,
    pub scheme: Option<String>,
}

impl Target {
    pub fn parse(s: &str) -> Result<Self, TargetParseError>;
    fn split_node_port(s: &str) -> (String, Option<u16>);
}
```

### Tests

**Add:**
- `test_daemon_request_serialization`
- `test_daemon_response_serialization`
- `test_daemon_error_response`
- `test_pid_file_write_read_remove`
- `test_stale_pid_detection`
- `test_config_load_defaults`
- `test_config_load_from_toml`
- `test_config_missing_file_uses_defaults`
- `test_target_parse_node_port` -- `"laptop:8080"` -> `Target { node: "laptop", port: Some(8080), .. }`
- `test_target_parse_node_only` -- `"laptop"` -> `Target { node: "laptop", port: None, .. }`
- `test_target_parse_with_path` -- `"server:3000/ws"` -> `Target { node: "server", port: Some(3000), path: Some("/ws"), .. }`
- `test_target_parse_with_scheme` -- `"ws://server:3000/path"` -> full parse
- `test_daemon_client_connect_no_daemon` -- verify clean error when no daemon running

### Verification

1. `cargo check --workspace`
2. `cargo test --workspace`
3. Manual: run `truffle up --foreground`, verify PID file created, socket listening, Ctrl+C cleans up

---

## Phase 7: CLI Commands -- Lifecycle (`up`, `down`, `status`)

**Depends on:** Phase 6
**Effort:** 10 hours
**Breaking changes:** Yes -- CLI interface completely changes
**Can ship independently:** Yes

### What changes

#### New files:

- **`crates/truffle-cli/src/commands/up.rs`** -- `truffle up`: start daemon, show live dashboard or background
- **`crates/truffle-cli/src/commands/down.rs`** -- `truffle down`: send shutdown to daemon
- **`crates/truffle-cli/src/commands/status.rs`** -- rewrite: query daemon via socket, display formatted output

#### Modified files:

- **`crates/truffle-cli/src/main.rs`** -- wire up new command enum
- **`crates/truffle-cli/src/commands/mod.rs`** -- rewrite module exports

#### Deleted files:

- **`crates/truffle-cli/src/commands/peers.rs`** (replaced by `ls` in Phase 8)
- **`crates/truffle-cli/src/commands/send.rs`** (moved to `dev send` in Phase 10)
- **`crates/truffle-cli/src/commands/serve.rs`** (moved to `http serve` later)
- **`crates/truffle-cli/src/commands/proxy.rs`** (rewritten as `proxy` in Phase 9, `http proxy` later)

### Key code changes

```rust
// commands/up.rs
pub async fn run(
    config: &TruffleConfig,
    name: Option<String>,
    ephemeral: bool,
    foreground: bool,
) -> Result<(), CliError> {
    // 1. Check if daemon already running (PID check)
    // 2. If not foreground: fork daemon process, wait for socket
    // 3. If foreground: start DaemonServer inline
    // 4. Display status dashboard (node name, IP, DNS, mesh count)
    // 5. Handle Ctrl+C for clean shutdown
}
```

```rust
// commands/down.rs
pub async fn run(config: &TruffleConfig, force: bool) -> Result<(), CliError> {
    // 1. Connect to daemon socket
    // 2. Send "shutdown" JSON-RPC request
    // 3. Wait for process to exit (check PID)
    // 4. Print confirmation
    // Handle: not running, force stop, active transfers
}
```

```rust
// commands/status.rs (rewritten)
pub async fn run(config: &TruffleConfig, watch: bool) -> Result<(), CliError> {
    // 1. Connect to daemon socket
    // 2. Send "status" JSON-RPC request
    // 3. Format and display: node name, status, IP, DNS, uptime, mesh stats
    // 4. If --watch: poll periodically and update in place
    // Handle: daemon not running -> "Node is not running. Run 'truffle up' to start."
}
```

### Tests

**Add:**
- `test_up_already_running` -- verify message when daemon already running
- `test_down_not_running` -- verify message when daemon not running
- `test_status_daemon_running` -- verify output format
- `test_status_daemon_not_running` -- verify hint message

### Verification

1. `cargo check --workspace`
2. `cargo test --workspace`
3. Manual: `truffle up` -> verify node starts; `truffle status` -> verify output; `truffle down` -> verify clean stop

---

## Phase 8: CLI Commands -- Discovery (`ls`, `ping`)

**Depends on:** Phase 7
**Effort:** 8 hours
**Breaking changes:** No (additive)
**Can ship independently:** Yes

### What changes

#### New files:

- **`crates/truffle-cli/src/commands/ls.rs`** -- `truffle ls`: list peers with formatted table output
- **`crates/truffle-cli/src/commands/ping.rs`** -- `truffle ping`: connectivity check via sidecar `tsnet:ping`
- **`crates/truffle-cli/src/resolve.rs`** -- name resolution layer (alias -> device_name -> HostName -> IP -> DNS)
- **`crates/truffle-cli/src/output.rs`** -- output formatting helpers (table, colors, indicators)

#### Modified files:

- **`crates/truffle-cli/src/main.rs`** -- add `Ls` and `Ping` to command enum
- **`crates/truffle-cli/src/daemon/server.rs`** -- implement `peers` and `ping` method handlers

### Key code changes

```rust
// commands/ls.rs
pub async fn run(
    config: &TruffleConfig,
    all: bool,
    long: bool,
    json: bool,
) -> Result<(), CliError> {
    // 1. Connect to daemon
    // 2. Send "peers" request
    // 3. If --long: also send "ping" for each peer (latency + direct/relayed)
    // 4. Format as table (or JSON with --json)
    // 5. Display summary line ("3 nodes online")
}
```

```rust
// commands/ping.rs
pub async fn run(
    config: &TruffleConfig,
    node: &str,
    count: u32,
    interval_ms: u64,
) -> Result<(), CliError> {
    // 1. Resolve node name to target
    // 2. Connect to daemon
    // 3. Send "ping" request N times
    // 4. Display each reply (time, direct/relayed)
    // 5. Display summary statistics (min/avg/max, loss%)
}
```

```rust
// resolve.rs
pub struct NameResolver {
    aliases: HashMap<String, String>,
    peers: Vec<PeerInfo>,
}

impl NameResolver {
    /// Resolve a user-provided name to a Tailscale address.
    /// Order: config alias -> mesh device_name -> Tailscale HostName -> IP -> DNS
    pub fn resolve(&self, name: &str) -> Result<ResolvedTarget, ResolveError>;

    /// Suggest corrections for a misspelled name (fuzzy matching).
    pub fn suggest(&self, name: &str) -> Option<String>;
}
```

```rust
// output.rs
pub fn print_table(headers: &[&str], rows: &[Vec<String>]);
pub fn status_indicator(online: bool) -> &'static str;  // "●" / "○"
pub fn format_latency(ms: Option<f64>) -> String;       // "2ms" / "—"
pub fn format_uptime(seconds: u64) -> String;            // "2h 14m"
pub fn dim(text: &str) -> String;                        // ANSI dim
pub fn bold(text: &str) -> String;                       // ANSI bold
pub fn green(text: &str) -> String;                      // ANSI green
pub fn red(text: &str) -> String;                        // ANSI red
```

### Tests

**Add:**
- `test_ls_formats_table`
- `test_ls_json_output`
- `test_ls_includes_offline_with_all`
- `test_ping_formats_output`
- `test_name_resolver_alias`
- `test_name_resolver_device_name`
- `test_name_resolver_hostname`
- `test_name_resolver_ip`
- `test_name_resolver_fuzzy_suggest`
- `test_format_latency`
- `test_format_uptime`
- `test_status_indicator`

### Verification

1. `cargo check --workspace`
2. `cargo test --workspace`
3. Manual: `truffle ls` -> verify table output; `truffle ping <node>` -> verify ping output

---

## Phase 9: CLI Commands -- Connectivity (`tcp`, `ws`, `proxy`, `expose`)

**Depends on:** Phase 8 (needs name resolution)
**Effort:** 14 hours
**Breaking changes:** No (additive)
**Can ship independently:** Yes

### What changes

#### New files:

- **`crates/truffle-cli/src/commands/tcp.rs`** -- `truffle tcp`: raw TCP via streaming upgrade over Unix socket
- **`crates/truffle-cli/src/commands/ws.rs`** -- `truffle ws`: WebSocket REPL via streaming upgrade
- **`crates/truffle-cli/src/commands/proxy.rs`** -- rewritten `truffle proxy`: port forwarding with live dashboard
- **`crates/truffle-cli/src/commands/expose.rs`** -- `truffle expose`: expose local port via `tsnet:listen`
- **`crates/truffle-cli/src/stream.rs`** -- stdin/stdout <-> Unix socket bidirectional pipe

#### Modified files:

- **`crates/truffle-cli/src/main.rs`** -- add `Tcp`, `Ws`, `Proxy`, `Expose` to command enum
- **`crates/truffle-cli/src/daemon/server.rs`** -- implement `tcp_connect`, `ws_connect`, `proxy_start`, `proxy_stop`, `expose_start`, `expose_stop` handlers including streaming upgrade

### Key code changes

```rust
// commands/tcp.rs
pub async fn run(
    config: &TruffleConfig,
    target: &str,
    listen: bool,
    port: Option<u16>,
) -> Result<(), CliError> {
    // 1. Parse target
    // 2. Connect to daemon socket
    // 3. Send "tcp_connect" request
    // 4. Receive streaming upgrade response
    // 5. Bidirectional pipe: stdin -> socket, socket -> stdout
    // 6. Handle EOF/disconnect
}
```

```rust
// commands/proxy.rs (rewritten)
pub async fn run(
    config: &TruffleConfig,
    local_port: u16,
    target: &str,
    bind_addr: &str,
) -> Result<(), CliError> {
    // 1. Parse target
    // 2. Connect to daemon
    // 3. Send "proxy_start" request
    // 4. Display live dashboard (local, remote, status, connections, bandwidth)
    // 5. Handle Ctrl+C -> send "proxy_stop"
}
```

```rust
// commands/expose.rs
pub async fn run(
    config: &TruffleConfig,
    port: u16,
    https: bool,
    as_name: Option<String>,
) -> Result<(), CliError> {
    // 1. Connect to daemon
    // 2. Send "expose_start" request (triggers tsnet:listen in sidecar)
    // 3. Display live dashboard
    // 4. Handle Ctrl+C -> send "expose_stop" (triggers tsnet:unlisten)
}
```

```rust
// stream.rs
/// Bidirectional pipe between stdin/stdout and a Unix socket.
/// Used by `tcp` and `ws` commands after streaming upgrade.
pub async fn pipe_stdio(stream: UnixStream) -> io::Result<()> {
    let (read_half, write_half) = stream.into_split();
    let stdin_to_socket = tokio::io::copy(&mut tokio::io::stdin(), &mut write_half);
    let socket_to_stdout = tokio::io::copy(&mut read_half, &mut tokio::io::stdout());
    tokio::select! {
        r = stdin_to_socket => r.map(|_| ()),
        r = socket_to_stdout => r.map(|_| ()),
    }
}
```

### Tests

**Add:**
- `test_tcp_target_parsing`
- `test_ws_target_with_path`
- `test_proxy_port_validation`
- `test_expose_port_validation`
- `test_streaming_upgrade_protocol`

### Verification

1. `cargo check --workspace`
2. `cargo test --workspace`
3. Manual: `truffle tcp <node>:8080` -> verify bidirectional data; `truffle proxy 8080 <node>:80` -> verify forwarding; `truffle expose 3000` -> verify tailnet listener

---

## Phase 10: CLI Commands -- Communication (`chat`, `send`)

**Depends on:** Phase 9 (needs streaming protocol)
**Effort:** 10 hours
**Breaking changes:** No (additive)
**Can ship independently:** Yes

### What changes

#### New files:

- **`crates/truffle-cli/src/commands/chat.rs`** -- `truffle chat`: interactive chat with line-by-line and split-screen modes
- **`crates/truffle-cli/src/commands/send.rs`** -- rewritten `truffle send`: one-shot message to a named node
- **`crates/truffle-cli/src/commands/dev.rs`** -- `truffle dev send|events|connections` subgroup
- **`crates/truffle-cli/src/tui/chat_view.rs`** -- chat line-by-line rendering
- **`crates/truffle-cli/src/tui/split_view.rs`** -- split-screen chat rendering (optional, if time permits)

#### Modified files:

- **`crates/truffle-cli/src/main.rs`** -- add `Chat`, `Send`, `Dev` to command enum
- **`crates/truffle-cli/src/daemon/server.rs`** -- implement `chat_start`, `send_message` handlers
- **`crates/truffle-cli/Cargo.toml`** -- add `crossterm` dependency for terminal handling

### Key code changes

```rust
// commands/chat.rs
pub async fn run(
    config: &TruffleConfig,
    node: Option<String>,
    split: bool,
) -> Result<(), CliError> {
    // 1. Resolve node (or use broadcast if none)
    // 2. Connect to daemon, send "chat_start" request
    // 3. Receive streaming upgrade
    // 4. Enter chat loop:
    //    - Read lines from stdin
    //    - Send as JSON events to socket
    //    - Receive messages from socket, display with timestamps
    // 5. Handle Ctrl+C -> close
}
```

```rust
// commands/send.rs (rewritten)
pub async fn run(
    config: &TruffleConfig,
    node: &str,
    message: &str,
    all: bool,
    wait: bool,
) -> Result<(), CliError> {
    // 1. Resolve node
    // 2. Connect to daemon
    // 3. Send "send_message" request
    // 4. Print confirmation ("Sent to laptop")
    // 5. If --wait: wait for reply, print it
}
```

### Tests

**Add:**
- `test_chat_message_format` -- verify JSON event format
- `test_send_one_shot`
- `test_send_broadcast`
- `test_send_wait_for_reply`

### Verification

1. `cargo check --workspace`
2. `cargo test --workspace`
3. Manual: `truffle send <node> "hello"` -> verify delivery; `truffle chat <node>` -> verify interactive chat

---

## Phase 11: CLI Commands -- Files + Diagnostics (`cp`, `doctor`, `completion`)

**Depends on:** Phase 5 (needs sidecar Taildrop commands), Phase 8 (needs name resolution)
**Effort:** 8 hours
**Breaking changes:** No (additive)
**Can ship independently:** Yes

### What changes

#### New files:

- **`crates/truffle-cli/src/commands/cp.rs`** -- `truffle cp`: file transfer via Taildrop
- **`crates/truffle-cli/src/commands/doctor.rs`** -- `truffle doctor`: connectivity diagnostics
- **`crates/truffle-cli/src/commands/completion.rs`** -- `truffle completion`: shell completion generation

#### Modified files:

- **`crates/truffle-cli/src/main.rs`** -- add `Cp`, `Doctor`, `Completion` to command enum
- **`crates/truffle-cli/src/daemon/server.rs`** -- implement file transfer and diagnostics handlers
- **`crates/truffle-cli/Cargo.toml`** -- add `clap_complete`, `indicatif` (progress bars)

### Key code changes

```rust
// commands/cp.rs
pub async fn run(
    config: &TruffleConfig,
    source: &str,
    dest: &str,
    recursive: bool,
    verify: bool,
) -> Result<(), CliError> {
    // 1. Parse source/dest (local or node:path)
    // 2. Connect to daemon
    // 3. For upload: send "push_file" command, stream file data, show progress bar
    // 4. For download: send "get_waiting_file", receive data, show progress bar
    // 5. If --verify: compute SHA-256, compare
}
```

```rust
// commands/doctor.rs
pub async fn run(config: &TruffleConfig, fix: bool) -> Result<(), CliError> {
    // Check list:
    // 1. Tailscale installed (check binary exists)
    // 2. Tailscale connected (check daemon socket or tailscale status)
    // 3. Sidecar binary found (check config path or default locations)
    // 4. Config file valid
    // 5. Mesh reachable (ping peers if daemon running)
    // 6. Key expiry (check sidecar status)
    // Print results with checkmarks/crosses
}
```

```rust
// commands/completion.rs
pub fn run(shell: clap_complete::Shell) {
    clap_complete::generate(
        shell,
        &mut Cli::command(),
        "truffle",
        &mut std::io::stdout(),
    );
}
```

### Tests

**Add:**
- `test_cp_parse_remote_path` -- verify `"node:path"` parsing
- `test_cp_parse_local_path` -- verify local path handling
- `test_doctor_checks_run` -- verify check structure
- `test_completion_generation` -- verify output is valid shell script

### Verification

1. `cargo check --workspace`
2. `cargo test --workspace`
3. Manual: `truffle doctor` -> verify diagnostic output; `truffle completion zsh` -> verify valid output

---

## Phase 12: Polish -- Help Text, Output Design, Aliases

**Depends on:** All previous phases
**Effort:** 6 hours
**Breaking changes:** No
**Can ship independently:** Yes

### What changes

#### Modified files:

- **`crates/truffle-cli/src/main.rs`** -- rewrite all `about` and `long_about` text to product language per cli-design.md Part 2 principles
- **`crates/truffle-cli/src/output.rs`** -- implement full color palette (dark/light terminal), Unicode box-drawing, column auto-sizing
- **`crates/truffle-cli/src/resolve.rs`** -- integrate config aliases into name resolution
- **`crates/truffle-cli/src/commands/completion.rs`** -- add dynamic node name completion (queries daemon for peer list)
- All command files -- implement the "fail with a fix" error pattern:
  - Every error includes (a) what went wrong, (b) why it likely happened, (c) what to do

### Key changes

**Help text rewrite:**
```rust
#[command(
    name = "truffle",
    about = "Mesh networking for your devices, built on Tailscale.",
    long_about = "Truffle connects your devices into a secure mesh network using Tailscale.\n\
                  Start with 'truffle up' to join the mesh, then 'truffle ls' to see your nodes.",
)]
```

**Error message design:**
```rust
pub fn format_error(what: &str, why: &str, fix: &str) -> String {
    format!(
        "{} {}\n  {} {}\n  {} {}",
        red("Error:"), what,
        dim("Why:"), why,
        dim("Fix:"), fix,
    )
}

// Example usage:
// Error: Can't reach "laptop". It appears to be offline.
//   Why: The node hasn't sent a heartbeat in over 5 minutes.
//   Fix: Run 'truffle ls' to see which nodes are available.
```

**Config alias resolution:**
```rust
impl NameResolver {
    pub fn resolve(&self, name: &str) -> Result<ResolvedTarget, ResolveError> {
        // 1. Check aliases first
        if let Some(alias_target) = self.aliases.get(name) {
            return Target::parse(alias_target).map(ResolvedTarget::from);
        }
        // 2. Check mesh device names
        // 3. Check Tailscale hostnames
        // 4. Check Tailscale IPs
        // 5. Try as DNS
    }
}
```

**Dynamic shell completion:**
```rust
// When generating completions, add a hook that queries the daemon for peer names
// This enables: truffle ping [TAB] -> laptop server work-desktop
```

### Tests

**Update:**
- All error message tests to verify three-part format
- Alias resolution tests

**Add:**
- `test_error_format` -- verify structure
- `test_alias_override` -- verify alias takes precedence over device name
- `test_color_output_disabled` -- verify `--color never` works

### Verification

1. `cargo check --workspace`
2. `cargo test --workspace`
3. Manual walkthrough of every command's `--help` output
4. Manual: verify error messages include fix suggestions
5. Manual: test `truffle completion zsh` with dynamic peer names

---

## Summary

| Phase | Description | Effort | Net LOC | Cumulative |
|-------|-------------|--------|---------|------------|
| 1 | Delete election system | 6h | -1,050 | -1,050 |
| 2 | Delete routing module | 3h | -160 | -1,210 |
| 3 | Simplify wire protocol types | 5h | -200 | -1,410 |
| 4 | Update events + NAPI | 8h | -30 | -1,440 |
| 5 | Go sidecar additions | 10h | +300 | -1,140 |
| 6 | CLI daemon architecture | 14h | +500 | -640 |
| 7 | CLI: up, down, status | 10h | +250 | -390 |
| 8 | CLI: ls, ping + resolution | 8h | +300 | -90 |
| 9 | CLI: tcp, ws, proxy, expose | 14h | +400 | +310 |
| 10 | CLI: chat, send | 10h | +250 | +560 |
| 11 | CLI: cp, doctor, completion | 8h | +200 | +760 |
| 12 | Polish | 6h | +100 | +860 |
| **Total** | | **~95h** | | **-1,410 deleted, +2,270 added** |

### Risk notes

1. **Phase 1 is the riskiest** -- touching every core module. Extensive test updates required. Run integration tests after every sub-step.
2. **Phase 4 is the most diffuse** -- renames touch many files. Use find-and-replace carefully. Run `cargo check` after each rename.
3. **Phase 5 requires Go expertise** -- the sidecar additions need understanding of tsnet APIs (Listen, Ping, Taildrop).
4. **Phase 6 is the largest** -- daemon architecture is greenfield. Consider splitting into 6a (protocol + PID + config) and 6b (server + client) if needed.
5. **Streaming upgrade protocol (Phase 9)** is the most novel pattern -- bidirectional raw bytes over Unix socket after JSON-RPC handshake. Prototype this early.

### Definition of done

Each phase is done when:
1. `cargo check --workspace` passes
2. `cargo test --workspace` passes with zero failures
3. `cargo clippy --workspace` passes with no new warnings
4. No dead code warnings (all removed code was properly cleaned up)
5. No remaining references to deleted types/functions (verified by grep)
6. For CLI phases: manual verification of the command works end-to-end
