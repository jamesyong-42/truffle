# RFC 010: P2P Mesh Redesign + CLI Daemon Architecture

**Status**: Proposed
**Created**: 2026-03-20
**Depends on**: RFC 008 (Vision), RFC 009 (Wire Protocol Redesign)
**Supersedes**: RFC 008 Layer 3 (STAR topology, primary election, routing)

---

## 1. Problem Statement

Truffle's current architecture has a fundamental mismatch: it implements a STAR topology with primary election on top of Tailscale, which already provides a full-mesh encrypted network. This creates unnecessary complexity that is the #1 source of bugs and the #1 barrier to truffle becoming a product-grade CLI tool.

### 1.1 STAR Mesh is Over-Engineering

Tailscale gives us:
- **Full-mesh connectivity** via WireGuard tunnels between all nodes
- **Automatic NAT traversal** via DERP relays
- **Peer discovery** via the coordination server
- **Encrypted transport** via WireGuard

Truffle then builds a STAR topology *on top of this full mesh*, introducing:
- A **primary election** system to pick a hub node
- A **routing layer** that forwards messages through the primary
- **Role management** (Primary/Secondary) that creates artificial hierarchy
- **Split-brain detection** for election conflicts

This is like building a load balancer on top of a load balancer. Tailscale already solved the hard networking problems. Truffle's STAR topology adds complexity without adding value.

### 1.2 Election System is the #1 Bug Source

The `PrimaryElection` system (469 lines in `election.rs`) is responsible for:
- **Split-brain scenarios**: Two nodes both claim primary after network partitions
- **Failover races**: Grace period timers interact badly with reconnection timers
- **Startup ordering bugs**: Discovery timeout vs. election timeout vs. announce interval
- **State synchronization issues**: `device-list` carries `primary_id`, creating circular dependencies between device discovery and role assignment

Every bug report involving "messages not delivered" or "node stuck in wrong state" traces back to the election system.

### 1.3 Current CLI is SDK-Style, Not Product-Grade

The current CLI (`truffle-cli/src/main.rs`, 126 lines) exposes library internals:
- Commands like `send` require `device_id`, `namespace`, `msg_type`, `payload` -- four positional arguments
- `proxy` and `serve` are bolted-on features that require the full runtime
- No daemon mode: every command starts a full `TruffleRuntime`, authenticates with Tailscale, and tears it down
- No concept of a persistent node identity

The CLI should be a product, not a test harness.

### 1.4 The Connection Topology is Already Full-Mesh

Looking at the actual data flow:

```
Current:
  A ──ws──> Primary ──ws──> B    (STAR: A routes through Primary to reach B)

Reality (Tailscale layer):
  A ──wireguard──> B              (direct encrypted tunnel already exists)

After RFC 010:
  A ──ws──> B                     (direct WebSocket over the existing tunnel)
```

Every node can already reach every other node directly. The primary is an unnecessary relay.

---

## 2. P2P Mesh Redesign

### 2.1 What Gets DELETED

#### Files deleted entirely:

| File | Lines | Description |
|------|-------|-------------|
| `mesh/election.rs` | 469 | Primary election state machine, candidate collection, grace periods, timeouts |
| `mesh/routing.rs` | 160 | STAR routing decisions, `RouteDecision` enum, `wrap_route_message`, `wrap_route_broadcast` |

**Total: ~629 lines deleted entirely.**

#### Code removed from surviving files:

**`mesh/node.rs`** (~250 lines removed):
- `election` field and all `Arc<RwLock<PrimaryElection>>` references
- `election_event_rx` field and Mutex wrapper
- `ElectionConfig` setup in `start()` (~10 lines)
- Election reset in `stop()` (~5 lines)
- Election event channel recreation in `stop()` (~5 lines)
- `is_primary()`, `primary_id()`, `role()` methods (~15 lines)
- `ViaPrimary` fallback in `send_envelope()` (~10 lines)
- Primary/secondary branching in `broadcast_envelope()` (~15 lines)
- Entire election event processing task in `start_event_loop()` (~130 lines)
- `handle_tailnet_peers()` election trigger logic (~20 lines)
- `MeshTimingConfig.election` field
- `MeshNodeConfig.prefer_primary` field
- `MeshNodeEvent::RoleChanged` and `MeshNodeEvent::PrimaryChanged` variants

**`mesh/handler.rs`** (~200 lines removed):
- `election` field from `TransportHandler`
- `handle_connected()` primary device-list sending (~25 lines)
- `dispatch_mesh_message()` election arms: `ElectionStart`, `ElectionCandidate`, `ElectionResult` (~20 lines)
- `dispatch_mesh_message()` route arms: `RouteMessage`, `RouteBroadcast` warning (~10 lines)
- `dispatch_mesh_message()` split-brain detection in `DeviceList` handler (~15 lines)
- `handle_route_envelope()` entirely (~130 lines)
- `handle_message()` route envelope branch (~5 lines)

**`protocol/types.rs`** (~80 lines removed):
- `MeshMessageType` variants: `ElectionStart`, `ElectionCandidate`, `ElectionResult`, `RouteMessage`, `RouteBroadcast`
- `MeshPayload` variants: `ElectionStart`, `ElectionCandidate(..)`, `ElectionResult(..)`, `RouteMessage(..)`, `RouteBroadcast(..)`
- Corresponding `from_str` match arms and `parse` match arms

**`protocol/message_types.rs`** (~30 lines removed):
- `ElectionCandidatePayload` struct
- `ElectionResultPayload` struct
- `RouteMessagePayload` struct
- `RouteBroadcastPayload` struct

**`types/mod.rs`** (~5 lines removed):
- `DeviceRole` enum (`Primary`, `Secondary`)

#### Enums/types removed:

```rust
// ALL OF THESE ARE DELETED:
pub enum DeviceRole { Primary, Secondary }
pub struct ElectionConfig { .. }
pub enum ElectionPhase { Idle, Waiting, Collecting, Decided }
pub enum ElectionEvent { .. }
pub struct ElectionTimingConfig { .. }
pub struct PrimaryElection { .. }
pub enum RouteDecision { Direct, ViaPrimary { .. }, ForwardAsHub, Local, Unroutable }
pub struct ElectionCandidatePayload { .. }
pub struct ElectionResultPayload { .. }
pub struct RouteMessagePayload { .. }
pub struct RouteBroadcastPayload { .. }
```

#### Wire protocol message types removed (5 of 8):

```
election-start       DELETED
election-candidate   DELETED
election-result      DELETED
route-message        DELETED
route-broadcast      DELETED
```

**Estimated total: ~1,200 lines removed across all files.**

### 2.2 What STAYS (Unchanged)

| Component | File(s) | Why it stays |
|-----------|---------|-------------|
| `DeviceManager` | `mesh/device.rs` | Tracks discovered peers, handles device-announce/goodbye. No election dependency. |
| `ConnectionManager` | `transport/connection.rs` | WebSocket connection lifecycle. Transport-layer, no mesh knowledge. |
| `MeshMessageBus` | `mesh/message_bus.rs` | Namespace pub/sub. Consumers subscribe to namespaces, bus dispatches. No routing logic. |
| `StoreSyncAdapter` | `store_sync/adapter.rs` | Calls `broadcast_envelope()`. Unchanged because broadcast semantics don't change. |
| `FileTransferAdapter` | `file_transfer/adapter.rs` | Already uses direct P2P connections for data transfer (HTTP PUT). |
| `BridgeManager` | `bridge/manager.rs` | Binary-framed TCP between Go sidecar and Rust. Infrastructure layer. |
| `GoShim` | `bridge/shim.rs` | Spawns and manages the Go sidecar process. |
| HTTP layer | `http/` | Reverse proxy, static hosting, PWA. Built on bridge, not on mesh topology. |
| Wire protocol v3 | `protocol/` | Frame types, binary framing, typed dispatch. Only the mesh message type enum shrinks. |
| `TruffleRuntime` builder | `runtime.rs` | Simplified config (fewer fields), but same builder pattern. |

### 2.3 What CHANGES

#### 2.3.1 Simplified `MeshNodeConfig`

```rust
// BEFORE (RFC 008):
pub struct MeshNodeConfig {
    pub device_id: String,
    pub device_name: String,
    pub device_type: String,
    pub hostname_prefix: String,
    pub prefer_primary: bool,          // REMOVED
    pub capabilities: Vec<String>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    pub timing: MeshTimingConfig,      // simplified
}

pub struct MeshTimingConfig {
    pub announce_interval: Duration,
    pub discovery_timeout: Duration,
    pub election: ElectionTimingConfig, // REMOVED
}

// AFTER (RFC 010):
pub struct MeshNodeConfig {
    pub device_id: String,
    pub device_name: String,
    pub device_type: String,
    pub hostname_prefix: String,
    pub capabilities: Vec<String>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    pub timing: MeshTimingConfig,
}

pub struct MeshTimingConfig {
    pub announce_interval: Duration,
    pub discovery_timeout: Duration,
    // No election timing. Elections don't exist.
}
```

#### 2.3.2 Simplified `MeshNode`

```rust
// BEFORE (RFC 008):
pub struct MeshNode {
    config: MeshNodeConfig,
    device_manager: Arc<RwLock<DeviceManager>>,
    election: Arc<RwLock<PrimaryElection>>,     // REMOVED
    connection_manager: Arc<ConnectionManager>,
    message_bus: Arc<MeshMessageBus>,
    lifecycle: Arc<RwLock<MeshNodeLifecycle>>,
    event_tx: broadcast::Sender<MeshNodeEvent>,
    device_event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<DeviceEvent>>>,
    election_event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<ElectionEvent>>>,  // REMOVED
}

// AFTER (RFC 010):
pub struct MeshNode {
    config: MeshNodeConfig,
    device_manager: Arc<RwLock<DeviceManager>>,
    connection_manager: Arc<ConnectionManager>,
    message_bus: Arc<MeshMessageBus>,
    lifecycle: Arc<RwLock<MeshNodeLifecycle>>,
    event_tx: broadcast::Sender<MeshNodeEvent>,
    device_event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<DeviceEvent>>>,
    // NO election, NO election_event_rx
}
```

#### 2.3.3 Simplified `MeshNode::send_envelope()`

```rust
// BEFORE: STAR routing with ViaPrimary fallback
pub async fn send_envelope(&self, device_id: &str, envelope: &MeshEnvelope) -> bool {
    let local_id = self.config.device_id.clone();
    if device_id == local_id {
        // local delivery...
        return true;
    }
    let conn = self.connection_manager.get_connection_by_device(device_id).await;
    if conn.is_some() {
        return self.send_envelope_direct(device_id, envelope).await;
    }
    // Route via primary if we're a secondary
    if !self.is_primary().await {
        if let Some(primary_id) = self.primary_id().await {
            if let Some(route_envelope) = routing::wrap_route_message(device_id, envelope) {
                return self.send_envelope_direct(&primary_id, &route_envelope).await;
            }
            return false;
        }
    }
    tracing::warn!("No connection to device {device_id}");
    false
}

// AFTER: Direct send only. No routing through primary.
pub async fn send_envelope(&self, device_id: &str, envelope: &MeshEnvelope) -> bool {
    let local_id = self.config.device_id.clone();

    // Local delivery
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

    // Direct send. If no connection, return error.
    self.send_envelope_direct(device_id, envelope).await
}
```

#### 2.3.4 Simplified `MeshNode::broadcast_envelope()`

```rust
// BEFORE: Primary fans out, secondary routes through primary
pub async fn broadcast_envelope(&self, envelope: &MeshEnvelope) {
    let local_id = self.config.device_id.clone();
    if self.is_primary().await {
        // send to all connected devices + local
    } else {
        // route broadcast through primary
        if let Some(primary_id) = self.primary_id().await {
            if let Some(route_envelope) = routing::wrap_route_broadcast(envelope) {
                self.send_envelope_direct(&primary_id, &route_envelope).await;
            }
        }
    }
}

// AFTER: Always fan-out to all connections directly. Every node is equal.
pub async fn broadcast_envelope(&self, envelope: &MeshEnvelope) {
    let local_id = self.config.device_id.clone();

    // Fan-out to all connected peers
    let conns = self.connection_manager.get_connections().await;
    for conn in &conns {
        if let Some(ref did) = conn.device_id {
            if conn.status == ConnectionStatus::Connected {
                self.send_envelope_direct(did, envelope).await;
            }
        }
    }

    // Also deliver locally
    let _ = self.event_tx.send(MeshNodeEvent::Message(IncomingMeshMessage {
        from: Some(local_id),
        connection_id: "local".to_string(),
        namespace: envelope.namespace.clone(),
        msg_type: envelope.msg_type.clone(),
        payload: envelope.payload.clone(),
    }));
}
```

#### 2.3.5 Simplified `TransportHandler`

```rust
// BEFORE:
pub(crate) struct TransportHandler {
    pub config: MeshNodeConfig,
    pub connection_manager: Arc<ConnectionManager>,
    pub device_manager: Arc<RwLock<DeviceManager>>,
    pub election: Arc<RwLock<PrimaryElection>>,  // REMOVED
    pub event_tx: broadcast::Sender<MeshNodeEvent>,
    pub message_bus: Arc<MeshMessageBus>,
}

// AFTER:
pub(crate) struct TransportHandler {
    pub config: MeshNodeConfig,
    pub connection_manager: Arc<ConnectionManager>,
    pub device_manager: Arc<RwLock<DeviceManager>>,
    pub event_tx: broadcast::Sender<MeshNodeEvent>,
    pub message_bus: Arc<MeshMessageBus>,
    // NO election reference
}
```

#### 2.3.6 Simplified `handle_connected()`

```rust
// BEFORE: Send announce + device-list if primary
pub async fn handle_connected(&self, conn: &WSConnection) {
    // Send device announce (unchanged)
    let announce = /* ... */;
    self.send_envelope_to_conn(&conn.id, &announce).await;

    // If primary, send device list (REMOVED)
    if self.election.read().await.is_primary() {
        let list = /* ... DeviceListPayload { primary_id: ... } ... */;
        self.send_envelope_to_conn(&conn.id, &list).await;
    }
}

// AFTER: Send announce only. No device-list, no primary check.
pub async fn handle_connected(&self, conn: &WSConnection) {
    tracing::info!("Transport connected: {} ({:?})", conn.id, conn.direction);

    // Send our device announce to the new peer
    let local_device = self.device_manager.read().await.local_device().clone();
    let announce_payload = match serde_json::to_value(&DeviceAnnouncePayload {
        device: local_device,
        protocol_version: 2,
    }) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Failed to serialize DeviceAnnouncePayload: {e}");
            return;
        }
    };
    let announce = MeshMessage::new(
        "device-announce",
        &self.config.device_id,
        announce_payload,
    );
    if let Some(envelope) = Self::wrap_mesh_message(&announce) {
        self.send_envelope_to_conn(&conn.id, &envelope).await;
    }

    // Emit PeerConnected event
    let _ = self.event_tx.send(MeshNodeEvent::PeerConnected {
        connection_id: conn.id.clone(),
        peer_dns: conn.peer_dns_name.clone(),
    });
}
```

#### 2.3.7 Simplified `dispatch_mesh_message()`

```rust
// BEFORE: 8 match arms (3 device + 3 election + 2 route warning)
pub(crate) async fn dispatch_mesh_message(&self, msg: &MeshMessage) {
    match MeshPayload::parse(&msg.msg_type, msg.payload.clone()) {
        Ok(MeshPayload::DeviceAnnounce(payload)) => { /* ... */ }
        Ok(MeshPayload::DeviceList(payload)) => {
            // handles device list + split-brain detection + election set_primary
        }
        Ok(MeshPayload::DeviceGoodbye(_payload)) => { /* ... */ }
        Ok(MeshPayload::ElectionStart) => { /* election.handle_election_start() */ }
        Ok(MeshPayload::ElectionCandidate(payload)) => { /* election.handle_election_candidate() */ }
        Ok(MeshPayload::ElectionResult(payload)) => { /* election.handle_election_result() */ }
        Ok(MeshPayload::RouteMessage(_)) | Ok(MeshPayload::RouteBroadcast(_)) => {
            tracing::warn!("Route message via dispatch -- should use handle_route_envelope");
        }
        Err(e) => { /* log warning */ }
    }
}

// AFTER: 3 match arms only
pub(crate) async fn dispatch_mesh_message(&self, msg: &MeshMessage) {
    match MeshPayload::parse(&msg.msg_type, msg.payload.clone()) {
        Ok(MeshPayload::DeviceAnnounce(payload)) => {
            self.device_manager
                .write()
                .await
                .handle_device_announce(&msg.from, &payload);
        }
        Ok(MeshPayload::DeviceList(payload)) => {
            // Simplified: just update the device list, no primary tracking
            self.device_manager
                .write()
                .await
                .handle_device_list(&msg.from, &payload);
        }
        Ok(MeshPayload::DeviceGoodbye(_payload)) => {
            self.device_manager
                .write()
                .await
                .handle_device_goodbye(&msg.from);
        }
        Err(e) => {
            tracing::warn!(
                msg_type = %msg.msg_type,
                from = %msg.from,
                error = %e,
                "Failed to parse mesh message"
            );
        }
    }
}
```

#### 2.3.8 Simplified `handle_tailnet_peers()`

```rust
// BEFORE: Connect to peers + trigger election if no primary
pub async fn handle_tailnet_peers(&self, peers: &[TailnetPeer]) {
    // ... discover and connect to peers ...

    let has_primary = self.election.read().await.primary_id().is_some();
    if !has_primary && online_devices.is_empty() {
        // self-elect as primary
        election.set_primary(&identity.id);
    } else if !has_primary && !online_devices.is_empty() {
        // wait for discovery timeout, then start election
        tokio::spawn(async move { /* ... election ... */ });
    }
}

// AFTER: Just connect to all peers. No election.
pub async fn handle_tailnet_peers(&self, peers: &[TailnetPeer]) {
    let identity = self.device_manager.read().await.device_identity().clone();
    tracing::info!("Discovered {} tailnet peers", peers.len());

    for peer in peers {
        if peer.hostname == identity.tailscale_hostname {
            continue; // skip self
        }

        let device = {
            let mut dm = self.device_manager.write().await;
            dm.add_discovered_peer(peer)
        };

        if let Some(device) = device {
            if peer.online {
                let conn = self.connection_manager
                    .get_connection_by_device(&device.id)
                    .await;
                if conn.is_none()
                    || conn.map(|c| c.status) != Some(ConnectionStatus::Connected)
                {
                    tracing::info!(
                        "Should connect to peer {} ({})",
                        device.name,
                        device.id
                    );
                }
            }
        }
    }
    // No election trigger. No self-elect. Just connect to everyone.
}
```

#### 2.3.9 Simplified `handle_message()`

```rust
// BEFORE: Routes mesh messages and route envelopes separately
pub async fn handle_message(&self, connection_id: &str, payload: &serde_json::Value) {
    let envelope: MeshEnvelope = /* parse */;
    if envelope.namespace == MESH_NAMESPACE {
        if envelope.msg_type == "message" {
            self.handle_mesh_envelope(connection_id, &from_device_id, &envelope).await;
        } else {
            // route-message, route-broadcast
            self.handle_route_envelope(connection_id, from_device_id.as_deref(), &envelope).await;
        }
    } else {
        self.handle_app_message(connection_id, from_device_id, &envelope).await;
    }
}

// AFTER: No route envelopes. All mesh messages go through dispatch.
pub async fn handle_message(&self, connection_id: &str, payload: &serde_json::Value) {
    let envelope: MeshEnvelope = match serde_json::from_value(payload.clone()) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                connection_id,
                error = %e,
                "Failed to parse MeshEnvelope from incoming message"
            );
            return;
        }
    };

    let conn = self.connection_manager.get_connection(connection_id).await;
    let from_device_id = conn.and_then(|c| c.device_id);

    if envelope.namespace == MESH_NAMESPACE {
        self.handle_mesh_envelope(connection_id, &from_device_id, &envelope).await;
    } else {
        self.handle_app_message(connection_id, from_device_id, &envelope).await;
    }
}
```

#### 2.3.10 PWA Gateway Behavior

In the current STAR topology, the primary node serves as the gateway for PWA clients (browser connections). After the P2P redesign:

- **Any node can serve as a PWA gateway.** The node that the PWA connects to forwards mesh messages locally.
- The connected node receives all broadcast messages (because every node is in a full mesh) and relays them to the PWA client over the WebSocket.
- Targeted messages to the PWA client are sent to the gateway node, which forwards them to the browser.
- This is actually simpler than before: the PWA just connects to any truffle node and gets the full mesh view through that node's perspective.

### 2.4 `DeviceListPayload` Simplification

```rust
// BEFORE:
pub struct DeviceListPayload {
    pub devices: Vec<BaseDevice>,
    pub primary_id: String,       // REMOVED
}

// AFTER:
pub struct DeviceListPayload {
    pub devices: Vec<BaseDevice>,
    // No primary_id. There is no primary.
}
```

The `device-list` message type is retained for bootstrapping: when a node connects to a peer, it may receive a `device-list` telling it about other nodes the peer knows about. This is useful for mesh discovery but carries no role information.

### 2.5 New Sidecar Commands (Dynamic Listeners + Ping)

The Go sidecar's bridge protocol gains three new command pairs to support CLI features that require dynamic Tailscale networking:

| Request | Response | Description |
|---------|----------|-------------|
| `tsnet:listen {"port": 3000, "tls": false}` | `tsnet:listening {"port": 3000}` | Start listening on a port via tsnet. Used by `truffle expose`. |
| `tsnet:unlisten {"port": 3000}` | `tsnet:unlistened {"port": 3000}` | Stop listening on a port. Used by `truffle expose` teardown. |
| `tsnet:ping {"target": "100.64.0.3"}` | `tsnet:pingResult {"latency_ms": 2.1, "direct": true, "relay": ""}` | Ping a Tailscale peer via tsnet. Returns latency, whether the path is direct, and relay name if relayed. Used by `truffle ping` and `truffle ls -l`. |

These extend the existing bridge protocol (binary-framed, newline-delimited JSON commands) without changing its framing. The `tsnet:listen` command creates a listener in the Go sidecar that accepts connections and bridges them back to Rust via the existing bridge transport. The `tsnet:ping` command wraps Tailscale's `Ping()` API to provide accurate latency measurements.

### 2.6 Taildrop for File Transfer (`truffle cp`)

For `truffle cp`, truffle uses Tailscale's built-in Taildrop API for file transfer rather than a custom HTTP-based protocol. This leverages Tailscale's optimized peer-to-peer data path and avoids reimplementing file transfer semantics.

New sidecar commands for Taildrop:

| Request | Response | Description |
|---------|----------|-------------|
| `tsnet:pushFile {"target": "100.64.0.3", "name": "report.pdf", "size": 4200000}` | `tsnet:pushFileProgress {"bytes_sent": ..., "done": false}` | Push a file to a peer via Taildrop. The file data follows as raw bytes on the bridge after the command. |
| `tsnet:waitingFiles {}` | `tsnet:waitingFilesList {"files": [...]}` | List files waiting to be received via Taildrop. |
| `tsnet:getWaitingFile {"name": "report.pdf"}` | Raw file bytes on bridge | Retrieve a waiting file's contents. |
| `tsnet:deleteWaitingFile {"name": "report.pdf"}` | `tsnet:deletedWaitingFile {"name": "report.pdf"}` | Delete a received file from the waiting queue. |

The custom HTTP PUT mechanism (used by `FileTransferAdapter`) is retained for internal store sync and as a fallback for non-Taildrop scenarios. Truffle wraps Taildrop with its own progress reporting and SHA-256 post-transfer verification.

---

## 3. CLI Daemon Architecture

The current CLI creates a full `TruffleRuntime` for every command invocation. This means every `truffle status` or `truffle ls` requires:

1. Spawn Go sidecar process
2. Authenticate with Tailscale
3. Establish bridge connection
4. Start mesh node
5. Wait for peer discovery
6. Execute the actual command
7. Tear everything down

This takes 3-10 seconds per command. A daemon architecture makes the runtime persistent and commands instant.

### 3.1 Design Philosophy

From the text.txt vision document:

> Do not expose Tailscale concepts -- expose communication intent.

Users think in terms of:
- Start a node
- Connect to something
- Send data
- Expose a service
- See what's online

They do not think about `tsnet.Server`, `ListenTLS`, `MeshEnvelope`, or `DeviceRole`.

### 3.2 Daemon Architecture

```
┌───────────────────────────────────────────┐
│  truffle daemon                            │
│                                            │
│  ┌──────────────────────────────────────┐  │
│  │  TruffleRuntime                       │  │
│  │  (MeshNode + ConnectionManager +      │  │
│  │   Bridge + GoShim + MessageBus)       │  │
│  └──────────────────────────────────────┘  │
│                                            │
│  ┌──────────────────────────────────────┐  │
│  │  Unix Socket Server                   │  │
│  │  ~/.config/truffle/truffle.sock       │  │
│  │  Accepts CLI commands via JSON-RPC    │  │
│  └──────────────────────────────────────┘  │
│                                            │
│  ┌──────────────────────────────────────┐  │
│  │  PID File                             │  │
│  │  ~/.config/truffle/truffle.pid        │  │
│  └──────────────────────────────────────┘  │
└───────────────────────────────────────────┘

┌───────────────────────────────────────────┐
│  truffle <command>                         │
│                                            │
│  1. Check if daemon is running (PID file)  │
│  2. Connect to Unix socket                 │
│  3. Send JSON-RPC request                  │
│  4. Print response                         │
│  5. Exit                                   │
└───────────────────────────────────────────┘
```

#### Daemon lifecycle:

- `truffle up` starts the daemon in the background (or foreground with `--foreground`)
- Daemon holds a `TruffleRuntime` instance for the lifetime of the process
- Daemon listens on `~/.config/truffle/truffle.sock` for CLI commands
- Daemon writes PID to `~/.config/truffle/truffle.pid` for process management
- `truffle down` sends a graceful shutdown command via the socket, then verifies the process exited

#### Auto-start behavior:

If `auto_up = true` in config and no daemon is running, any command (e.g., `truffle ls`) will automatically start the daemon before executing. This makes the daemon transparent to the user.

### 3.3 Unix Socket Protocol

Commands are sent as JSON-RPC 2.0 over the Unix socket:

```rust
/// Request from CLI to daemon.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonRequest {
    pub jsonrpc: String,  // "2.0"
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

/// Response from daemon to CLI.
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
```

Methods:

| Method | Params | Description |
|--------|--------|-------------|
| `status` | `{}` | Node status (device ID, Tailscale IP, uptime) |
| `peers` | `{}` | List of discovered peers |
| `shutdown` | `{}` | Graceful daemon shutdown |
| `tcp_connect` | `{ "target": "node:port" }` | Open TCP connection (returns stream handle) |
| `ws_connect` | `{ "target": "node:port/path" }` | Open WebSocket connection |
| `proxy_start` | `{ "local_port": 8080, "target": "node:port" }` | Start port forwarding |
| `proxy_stop` | `{ "local_port": 8080 }` | Stop port forwarding |
| `expose_start` | `{ "port": 3000 }` | Expose local port to tailnet |
| `expose_stop` | `{ "port": 3000 }` | Stop exposing port |
| `ping` | `{ "node": "name" }` | Connectivity check |
| `send_message` | `{ "device_id": "..", "namespace": "..", "type": "..", "payload": {} }` | Send mesh message |

### 3.4 Streaming Protocol for Interactive Commands

Some commands (`tcp_connect`, `ws_connect`, `chat_start`) require long-lived bidirectional data streams, not simple request-response. The Unix socket supports this via a protocol upgrade pattern, similar to HTTP upgrading to WebSocket:

1. **Handshake phase.** The CLI sends a normal JSON-RPC request (e.g., `tcp_connect` with `{ "target": "server:5432" }`).
2. **Upgrade response.** The daemon responds with `{ "result": { "stream": true, "handle": "tcp-a1b2c3" } }`. This is the last JSON-RPC message on this socket.
3. **Raw byte stream.** After the upgrade response, the Unix socket becomes a raw bidirectional pipe. Bytes written by the CLI go to the remote target; bytes from the remote target are relayed back to the CLI. No framing, no JSON -- just raw bytes (for TCP) or framed messages (for WebSocket/chat).

For chat sessions, the stream phase uses newline-delimited JSON events instead of raw bytes:

```
← {"type":"message","from":"laptop","text":"hello","ts":"14:23:01"}
← {"type":"message","from":"server","text":"deploy done","ts":"14:23:05"}
→ {"type":"message","text":"got it"}
← {"type":"presence","node":"laptop","status":"typing"}
```

The daemon manages the lifecycle of the underlying connection. If the CLI disconnects from the Unix socket, the daemon tears down the corresponding TCP/WebSocket/chat connection. If the remote side disconnects, the daemon closes its end of the Unix socket, which the CLI detects as EOF.

This pattern keeps the JSON-RPC protocol simple (all methods are still request-response) while enabling interactive commands to work naturally with stdin/stdout piping.

### 3.5 The "Golden 6" Commands

These are the core commands that make truffle useful as a standalone CLI tool. They are verb-first, intent-driven, and cover 90% of use cases.

```
1. truffle up          Start/resume the daemon
2. truffle ls          List tailnet nodes
3. truffle tcp         Raw TCP connect (stdin/stdout, like netcat)
4. truffle ws          WebSocket REPL
5. truffle proxy       Pull remote to local (port forwarding)
6. truffle expose      Push local to tailnet
```

> **Note:** `truffle udp` was originally in this list but has been moved to Future Commands (Section 12.4). The Go sidecar's bridge is TCP-only and has no UDP listener support. Adding UDP requires bridge-level changes and is deferred.

#### `truffle up`

```
truffle up                     # start daemon with defaults
truffle up --name devbox       # custom node name
truffle up --ephemeral         # temporary node (removed on disconnect)
truffle up --foreground        # run in foreground (for debugging)
```

Output:
```
node is up
  name:       james-mbp
  tailnet ip: 100.64.0.5
  dns:        james-mbp.tail1234.ts.net
```

#### `truffle ls`

```
truffle ls                     # list online peers
truffle ls --all               # include offline peers
truffle ls --json              # JSON output for scripting
```

Output:
```
NAME         STATUS   IP             DNS
macbook      online   100.64.0.5     macbook.tail1234.ts.net
nas          online   100.64.0.12    nas.tail1234.ts.net
gpu-box      offline  100.64.0.23    gpu-box.tail1234.ts.net
```

#### `truffle tcp <target>`

Like netcat over Tailscale. Opens a raw TCP connection, pipes stdin/stdout.

```
truffle tcp db:5432                    # interactive TCP
echo "hello" | truffle tcp echo:9000   # pipe data
truffle tcp db:5432 --check            # test connectivity only
truffle tcp file-sink:7000 < dump.bin  # send file
```

#### `truffle ws <target>`

WebSocket REPL. Each line of input is sent as a text frame.

```
truffle ws chat:3000/ws                # interactive REPL
truffle ws chat:3000/ws --json         # pretty-print JSON frames
truffle ws chat:3000/ws --binary < data.msgpack  # send binary
```

Output:
```
connected to ws://chat:3000/ws
> {"type": "hello", "name": "james"}
< {"type": "welcome", "users": 3}
>
```

#### `truffle proxy <local-port> <target>`

Port forwarding: listen locally, forward to a tailnet peer.

```
truffle proxy 8080 app:80              # HTTP proxy
truffle proxy 15432 postgres:5432      # database proxy
truffle proxy 16379 redis:6379         # redis proxy
```

Output:
```
proxying localhost:8080 -> app:80
  press Ctrl+C to stop
```

#### `truffle expose <port>`

Expose a local service to the entire tailnet.

```
truffle expose 3000                    # expose port 3000
truffle expose 3000 --as web:80        # expose as specific name:port
truffle expose 8080 --https            # with Tailscale TLS cert
```

Output:
```
exposing localhost:3000 on tailnet
  accessible at: james-mbp.tail1234.ts.net:3000
```

### 3.6 Unified Target Addressing

All commands that take a `<target>` argument use the same addressing format:

```
node:port              laptop:8080
node/service           chat-server/ws
scheme://node:port     ws://chat:3000/ws
```

Target resolution order:

1. **Config alias** -- Check `[aliases]` in `config.toml` (see 3.7)
2. **Mesh device_name** -- Match against `device_name` from mesh `device-announce` messages (truffle peers only)
3. **Tailscale HostName** -- Match against Tailscale's `HostName` field from the coordination server (works for all Tailscale peers, including non-truffle devices)
4. **Tailscale IP** -- Match by Tailscale IP address (100.64.x.x)
5. **Full DNS** -- Resolve as a full DNS name (e.g., `truffle-desktop-a1b2.tailnet.ts.net`)

The daemon maintains a **correlation table** mapping mesh `device_name` (from `device-announce`) to Tailscale `HostName` (from `monitorState` peer list). This allows users to refer to truffle peers by their friendly mesh name while the daemon resolves the underlying Tailscale address. For non-truffle Tailscale peers (regular devices on the same tailnet), only Tailscale HostName, IP, and DNS resolution work -- mesh names are not available since those devices do not participate in the truffle mesh.

```rust
/// Parsed target address.
pub struct Target {
    pub node: String,           // MagicDNS name or alias
    pub port: Option<u16>,
    pub path: Option<String>,
    pub scheme: Option<String>, // "tcp", "udp", "ws", "wss", "http", "https"
}

impl Target {
    /// Parse a target string like "node:8080" or "ws://node:3000/path".
    pub fn parse(s: &str) -> Result<Self, TargetParseError> {
        // If it contains "://", parse as URL
        if s.contains("://") {
            let url = url::Url::parse(s)?;
            return Ok(Self {
                node: url.host_str().unwrap_or("").to_string(),
                port: url.port(),
                path: Some(url.path().to_string()).filter(|p| p != "/"),
                scheme: Some(url.scheme().to_string()),
            });
        }

        // If it contains "/", split into node and path
        if let Some((node_port, path)) = s.split_once('/') {
            let (node, port) = Self::split_node_port(node_port);
            return Ok(Self {
                node,
                port,
                path: Some(format!("/{path}")),
                scheme: None,
            });
        }

        // Otherwise, it's just node:port
        let (node, port) = Self::split_node_port(s);
        Ok(Self {
            node,
            port,
            path: None,
            scheme: None,
        })
    }

    fn split_node_port(s: &str) -> (String, Option<u16>) {
        match s.rsplit_once(':') {
            Some((node, port_str)) => match port_str.parse::<u16>() {
                Ok(port) => (node.to_string(), Some(port)),
                Err(_) => (s.to_string(), None),
            },
            None => (s.to_string(), None),
        }
    }
}
```

### 3.7 Config File

`~/.config/truffle/config.toml`:

```toml
[node]
name = "james-mbp"          # default node name (defaults to hostname)
auto_up = true               # auto-start daemon when running commands
sidecar_path = ""            # path to sidecar binary (auto-detected if empty)
state_dir = ""               # Tailscale state dir (defaults to ~/.config/truffle/state)

[aliases]
db = "postgres:5432"         # truffle tcp db  ->  truffle tcp postgres:5432
api = "backend:3000"         # truffle proxy 8080 api  ->  truffle proxy 8080 backend:3000
redis = "cache:6379"

[output]
format = "pretty"            # "pretty" or "json"
color = "auto"               # "auto", "always", "never"

[daemon]
log_level = "info"           # daemon log level
log_file = ""                # log to file (empty = stderr)
```

```rust
/// Deserialized config.toml.
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

#[derive(Debug, Deserialize)]
pub struct NodeConfig {
    #[serde(default = "default_node_name")]
    pub name: String,
    #[serde(default = "default_true")]
    pub auto_up: bool,
    #[serde(default)]
    pub sidecar_path: String,
    #[serde(default)]
    pub state_dir: String,
}

#[derive(Debug, Deserialize)]
pub struct OutputConfig {
    #[serde(default = "default_pretty")]
    pub format: String,
    #[serde(default = "default_auto")]
    pub color: String,
}

#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_info")]
    pub log_level: String,
    #[serde(default)]
    pub log_file: String,
}
```

### 3.8 Full Command Tree

```
truffle up                              # start daemon
truffle down                            # stop daemon
truffle ls                              # list peers
truffle status                          # show this node's status
truffle tcp <target>                    # raw TCP (netcat-style)
truffle ws <target>                     # WebSocket REPL
truffle proxy <local-port> <target>     # port forward (pull remote to local)
truffle expose <port>                   # expose local to tailnet
truffle ping <node>                     # connectivity check
truffle doctor                          # diagnostics (auth, netcheck, DERP, health)
truffle completion bash|zsh|fish        # shell completions

# HTTP subgroup:
truffle http proxy <prefix> <target>    # HTTP path-prefix reverse proxy
truffle http serve <dir>                # static file serving

# Dev/debug subgroup:
truffle dev send <id> <ns> <type> <payload>   # send raw mesh message
truffle dev events                             # stream mesh events (SSE-style)
truffle dev connections                        # dump WebSocket connections
```

### 3.9 Clap Command Structure

```rust
#[derive(Parser)]
#[command(
    name = "truffle",
    about = "Mesh networking over Tailscale",
    version,
    propagate_version = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Config file path
    #[arg(long, global = true, default_value = "~/.config/truffle/config.toml")]
    config: String,

    /// Output format override
    #[arg(long, global = true)]
    format: Option<OutputFormat>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the truffle daemon
    Up {
        /// Custom node name
        #[arg(long)]
        name: Option<String>,
        /// Ephemeral node (removed on disconnect)
        #[arg(long)]
        ephemeral: bool,
        /// Run in foreground
        #[arg(long)]
        foreground: bool,
    },

    /// Stop the truffle daemon
    Down,

    /// List peers on the tailnet
    Ls {
        /// Show offline peers too
        #[arg(long)]
        all: bool,
    },

    /// Show this node's status
    Status,

    /// Raw TCP connection (like netcat)
    Tcp {
        /// Target (node:port)
        target: String,
        /// Only test connectivity
        #[arg(long)]
        check: bool,
    },

    // Note: Udp variant deferred to future (Section 12.4)

    /// WebSocket REPL
    Ws {
        /// Target (node:port/path)
        target: String,
        /// Pretty-print JSON frames
        #[arg(long)]
        json: bool,
        /// Send binary from stdin
        #[arg(long)]
        binary: bool,
    },

    /// Port-forward a remote service to localhost
    Proxy {
        /// Local port to listen on
        local_port: u16,
        /// Remote target (node:port)
        target: String,
    },

    /// Expose a local port to the tailnet
    Expose {
        /// Local port to expose
        port: u16,
        /// Expose with Tailscale HTTPS cert
        #[arg(long)]
        https: bool,
        /// Custom name:port on the tailnet
        #[arg(long, rename_all = "kebab-case")]
        r#as: Option<String>,
    },

    /// Test connectivity to a node
    Ping {
        /// Target node name
        node: String,
        /// Number of pings
        #[arg(short = 'c', default_value = "4")]
        count: u32,
    },

    /// Run diagnostics
    Doctor,

    /// Generate shell completions
    Completion {
        /// Shell type
        shell: clap_complete::Shell,
    },

    /// HTTP subcommands
    Http {
        #[command(subcommand)]
        command: HttpCommands,
    },

    /// Developer/debug subcommands
    Dev {
        #[command(subcommand)]
        command: DevCommands,
    },
}

#[derive(Subcommand)]
enum HttpCommands {
    /// Reverse proxy with path prefix
    Proxy {
        prefix: String,
        target: String,
    },
    /// Serve static files
    Serve {
        dir: String,
        #[arg(long, default_value = "/")]
        prefix: String,
    },
}

#[derive(Subcommand)]
enum DevCommands {
    /// Send a raw mesh message
    Send {
        device_id: String,
        namespace: String,
        msg_type: String,
        payload: String,
    },
    /// Stream mesh events
    Events,
    /// Dump WebSocket connections
    Connections,
}

#[derive(Clone, Debug, clap::ValueEnum)]
enum OutputFormat {
    Pretty,
    Json,
}
```

---

## 4. Wire Protocol Changes

### 4.1 `MeshMessageType` Enum

```rust
// BEFORE (RFC 009): 8 variants
pub enum MeshMessageType {
    DeviceAnnounce,
    DeviceList,
    DeviceGoodbye,
    ElectionStart,       // REMOVED
    ElectionCandidate,   // REMOVED
    ElectionResult,      // REMOVED
    RouteMessage,        // REMOVED
    RouteBroadcast,      // REMOVED
}

// AFTER (RFC 010): 3 variants
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
}
```

### 4.2 `MeshPayload` Enum

```rust
// BEFORE: 8 variants
pub enum MeshPayload {
    DeviceAnnounce(DeviceAnnouncePayload),
    DeviceList(DeviceListPayload),
    DeviceGoodbye(DeviceGoodbyePayload),
    ElectionStart,
    ElectionCandidate(ElectionCandidatePayload),
    ElectionResult(ElectionResultPayload),
    RouteMessage(RouteMessagePayload),
    RouteBroadcast(RouteBroadcastPayload),
}

// AFTER: 3 variants
#[derive(Debug, Clone)]
pub enum MeshPayload {
    DeviceAnnounce(DeviceAnnouncePayload),
    DeviceList(DeviceListPayload),
    DeviceGoodbye(DeviceGoodbyePayload),
}

impl MeshPayload {
    pub fn parse(msg_type: &str, payload: serde_json::Value) -> Result<Self, DispatchError> {
        let typ = MeshMessageType::from_str(msg_type).ok_or_else(|| {
            DispatchError::UnknownMessageType {
                namespace: "mesh".into(),
                msg_type: msg_type.into(),
            }
        })?;

        match typ {
            MeshMessageType::DeviceAnnounce => {
                let p: DeviceAnnouncePayload = serde_json::from_value(payload)
                    .map_err(|e| DispatchError::PayloadParse {
                        msg_type: msg_type.into(),
                        source: e.to_string(),
                    })?;
                Ok(Self::DeviceAnnounce(p))
            }
            MeshMessageType::DeviceList => {
                let p: DeviceListPayload = serde_json::from_value(payload)
                    .map_err(|e| DispatchError::PayloadParse {
                        msg_type: msg_type.into(),
                        source: e.to_string(),
                    })?;
                Ok(Self::DeviceList(p))
            }
            MeshMessageType::DeviceGoodbye => {
                let p: DeviceGoodbyePayload = serde_json::from_value(payload)
                    .map_err(|e| DispatchError::PayloadParse {
                        msg_type: msg_type.into(),
                        source: e.to_string(),
                    })?;
                Ok(Self::DeviceGoodbye(p))
            }
        }
    }
}
```

### 4.3 Deleted Payload Types

```rust
// ALL OF THESE ARE DELETED from protocol/message_types.rs:

// pub struct ElectionCandidatePayload { .. }
// pub struct ElectionResultPayload { .. }
// pub struct RouteMessagePayload { .. }
// pub struct RouteBroadcastPayload { .. }
```

### 4.4 Wire Compatibility

This is a **breaking protocol change**. Nodes running RFC 010 cannot interoperate with nodes running RFC 008/009 because:

1. Old nodes send `election-start` messages that new nodes reject as unknown
2. Old secondary nodes route through primary; new nodes expect direct connections
3. `device-list` format changes (no `primary_id`)

Since truffle is pre-1.0, this is acceptable. All nodes must be upgraded together.

---

## 5. `TruffleEvent` Simplification

```rust
// BEFORE (RFC 008):
pub enum MeshNodeEvent {
    Started,
    Stopped,
    AuthRequired(String),
    AuthComplete,
    DeviceDiscovered(BaseDevice),
    DeviceUpdated(BaseDevice),
    DeviceOffline(String),
    DevicesChanged(Vec<BaseDevice>),
    RoleChanged { role: DeviceRole, is_primary: bool },  // REMOVED
    PrimaryChanged(Option<String>),                       // REMOVED
    Message(IncomingMeshMessage),
    Error(String),
}

// AFTER (RFC 010):
pub enum MeshNodeEvent {
    // Lifecycle
    Started,
    Stopped,

    // Tailscale auth
    AuthRequired(String),
    AuthComplete,

    // Peers (simplified -- no roles, no primary)
    PeerDiscovered(BaseDevice),
    PeerUpdated(BaseDevice),
    PeerOffline(String),
    PeersChanged(Vec<BaseDevice>),

    // Connections (new -- replaces role events for tracking mesh state)
    PeerConnected {
        connection_id: String,
        peer_dns: Option<String>,
    },
    PeerDisconnected {
        connection_id: String,
        reason: String,
    },

    // Messages
    Message(IncomingMeshMessage),

    // Errors
    Error(String),
}
```

**Key changes:**
- `DeviceDiscovered` renamed to `PeerDiscovered` (consistent naming)
- `DeviceUpdated` renamed to `PeerUpdated`
- `DeviceOffline` renamed to `PeerOffline`
- `DevicesChanged` renamed to `PeersChanged`
- `RoleChanged` **removed** (no roles exist)
- `PrimaryChanged` **removed** (no primary exists)
- `PeerConnected` **added** (fired when WebSocket connection established with peer)
- `PeerDisconnected` **added** (fired when WebSocket connection lost)

The rename from `Device*` to `Peer*` is a cosmetic improvement that aligns with Tailscale terminology and removes the implication of roles.

---

## 6. `TruffleEvent` for the Runtime Layer

The `TruffleRuntime` wraps `MeshNodeEvent` into a higher-level `TruffleEvent` for NAPI consumers. The simplified version:

```rust
pub enum TruffleEvent {
    // Lifecycle
    Started,
    Stopped,

    // Tailscale
    AuthRequired { auth_url: String },
    AuthComplete,
    Online { ip: String, dns_name: String },
    NeedsApproval,
    StateChanged { state: String, auth_url: String },
    KeyExpiring { expires_at: String },
    HealthWarning { warnings: Vec<String> },
    SidecarCrashed { exit_code: Option<i32>, stderr_tail: String },

    // Peers (no roles)
    PeerDiscovered(BaseDevice),
    PeerUpdated(BaseDevice),
    PeerOffline(String),
    PeersChanged(Vec<BaseDevice>),
    PeerConnected { connection_id: String, peer_dns: Option<String> },
    PeerDisconnected { connection_id: String, reason: String },

    // Messages
    Message(IncomingMeshMessage),

    // Errors
    Error(String),

    // NO RoleChanged
    // NO PrimaryChanged
}
```

---

## 7. Migration Plan

The implementation is phased to keep each step reviewable and testable independently.

### Phase 1: Delete the Election System (Biggest Code Deletion)

**Files affected:** `mesh/election.rs` (delete), `mesh/node.rs`, `mesh/handler.rs`

1. Delete `election.rs` entirely
2. Remove `election` field from `MeshNode`
3. Remove `election_event_rx` field from `MeshNode`
4. Remove election event processing task from `start_event_loop()`
5. Remove `ElectionConfig` setup from `start()`
6. Remove election reset from `stop()`
7. Remove election channel recreation from `stop()`
8. Remove `is_primary()`, `primary_id()`, `role()` methods from `MeshNode`
9. Remove `election` field from `TransportHandler`
10. Remove `ElectionTimingConfig` from `MeshTimingConfig`
11. Remove `prefer_primary` from `MeshNodeConfig`

**Tests:** Delete all election-related tests in `election.rs` (the file has its own `#[cfg(test)]` module). Integration tests that assert `RoleChanged` events will be updated in Phase 4.

**Estimated: -600 lines.**

### Phase 2: Delete Routing + Simplify Handler

**Files affected:** `mesh/routing.rs` (delete), `mesh/handler.rs`, `mesh/node.rs`

1. Delete `routing.rs` entirely
2. Remove `handle_route_envelope()` from `handler.rs` (~130 lines)
3. Simplify `handle_message()` to remove route envelope branch
4. Remove primary check from `handle_connected()` (no device-list on connect)
5. Simplify `send_envelope()`: remove `ViaPrimary` fallback
6. Simplify `broadcast_envelope()`: always fan-out to all connections
7. Simplify `handle_tailnet_peers()`: remove election trigger logic

**Tests:** Delete routing tests in `routing.rs`. Update `handler.rs` tests that test route envelope handling.

**Estimated: -400 lines.**

### Phase 3: Simplify Wire Protocol

**Files affected:** `protocol/types.rs`, `protocol/message_types.rs`

1. Remove 5 variants from `MeshMessageType` enum
2. Remove 5 variants from `MeshPayload` enum
3. Remove 5 match arms from `MeshPayload::parse()`
4. Remove 5 match arms from `MeshMessageType::from_str()`
5. Delete `ElectionCandidatePayload`, `ElectionResultPayload`, `RouteMessagePayload`, `RouteBroadcastPayload`
6. Remove `primary_id` field from `DeviceListPayload`
7. Delete `DeviceRole` enum from `types/mod.rs`

**Estimated: -120 lines.**

### Phase 4: Update Events

**Files affected:** `mesh/node.rs`, `runtime.rs` (if exists), NAPI bindings

1. Rename `DeviceDiscovered` -> `PeerDiscovered` etc.
2. Remove `RoleChanged`, `PrimaryChanged` from `MeshNodeEvent`
3. Add `PeerConnected`, `PeerDisconnected` to `MeshNodeEvent`
4. Emit `PeerConnected` in `handle_connected()`
5. Emit `PeerDisconnected` in `handle_disconnected()`
6. Update NAPI bindings to remove `RoleChanged`/`PrimaryChanged` and add new events
7. Update all tests that assert on removed event variants

**Estimated: -30 lines (net, after adding new events).**

### Phase 5: Update NAPI + Examples

**Files affected:** NAPI bindings, example apps

1. Remove `is_primary()`, `primary_id()`, `role()` from NAPI exports
2. Remove `DeviceRole` from NAPI type definitions
3. Remove `primary_candidate()` builder method
4. Update all examples to remove primary/secondary logic
5. Update example event handlers to use new event names

**Estimated: -50 lines.**

### Phase 6: Implement CLI Daemon Architecture

**New files:** `truffle-cli/src/daemon.rs`, `truffle-cli/src/socket.rs`, `truffle-cli/src/config.rs`

1. Create `TruffleConfig` type + TOML loading
2. Create `DaemonServer` with Unix socket listener
3. Create `DaemonClient` for CLI commands
4. Implement PID file management
5. Implement auto-up behavior
6. Wire up `truffle up` and `truffle down` commands

**Estimated: +400 lines.**

### Phase 7: Implement Golden 6 Commands

**New files:** `truffle-cli/src/commands/{up,down,ls,status,tcp,ws,proxy,expose,ping,doctor}.rs`

1. Implement each command as a module
2. Each command connects to daemon socket, sends JSON-RPC request, formats output
3. `tcp` and `ws` commands handle interactive I/O (stdin/stdout piping)
4. `proxy` command manages background port forwarding
5. `expose` command manages background service exposure
6. Implement `truffle doctor` (auth check, netcheck, DERP, peer connectivity)
7. Implement `truffle completion` for shell completions

**Estimated: +400 lines.**

---

## 8. Breaking Changes

### Removed from Public API

| Item | Type | Used by |
|------|------|---------|
| `DeviceRole` enum | Type | Consumers checking node roles |
| `MeshNodeEvent::RoleChanged` | Event | Event handlers waiting for role assignment |
| `MeshNodeEvent::PrimaryChanged` | Event | Event handlers tracking primary |
| `MeshNode::is_primary()` | Method | Consumers checking if current node is primary |
| `MeshNode::primary_id()` | Method | Consumers looking up the primary |
| `MeshNode::role()` | Method | Consumers reading role |
| `MeshNodeConfig::prefer_primary` | Config | Builder pattern: `.primary_candidate(true)` |
| `MeshTimingConfig::election` | Config | Election timing customization |
| `ElectionConfig` | Type | Internal, but exposed |
| `ElectionPhase` | Type | Internal, but exposed |
| `ElectionEvent` | Type | Internal, but exposed |
| `ElectionTimingConfig` | Type | Builder pattern: `.election_timeout(..)` |

### Removed from Wire Protocol

| Message Type | Direction | Purpose |
|-------------|-----------|---------|
| `election-start` | broadcast | Trigger new election |
| `election-candidate` | broadcast | Announce candidacy |
| `election-result` | broadcast | Announce winner |
| `route-message` | to primary | Route targeted message via hub |
| `route-broadcast` | to primary | Route broadcast via hub |

### Changed on Wire

| Item | Before | After |
|------|--------|-------|
| `device-list` payload | `{ devices: [...], primary_id: "..." }` | `{ devices: [...] }` |

### CLI Changes

| Before | After |
|--------|-------|
| `truffle status` (starts full runtime) | `truffle status` (queries daemon) |
| `truffle peers` | `truffle ls` |
| `truffle send <device_id> <ns> <type> <payload>` | `truffle dev send <device_id> <ns> <type> <payload>` |
| `truffle proxy <prefix> <target>` | `truffle http proxy <prefix> <target>` |
| `truffle serve <dir>` | `truffle http serve <dir>` |
| (none) | `truffle up`, `truffle down`, `truffle tcp`, `truffle ws`, `truffle proxy`, `truffle expose`, `truffle ping`, `truffle doctor` |

---

## 9. What We Keep from Previous RFCs

### From RFC 008 (Vision)

- **Layer 0** (Tailscale/tsnet): unchanged
- **Layer 1** (Bridge): unchanged
- **Layer 2** (Transport): unchanged
- **Layer 3** (Mesh): simplified (P2P instead of STAR)
- **Layer 4** (Application Services): unchanged (message bus, store sync, file transfer)
- **Layer 5** (HTTP): unchanged (reverse proxy, static hosting, PWA, Web Push)
- **Builder pattern**: simplified (fewer config options)
- **TruffleRuntime**: same API surface minus role-related methods

### From RFC 009 (Wire Protocol Redesign)

- **Binary framing** (2-byte header): unchanged
- **Frame types** (Control, Data, Error): unchanged
- **FrameType enum**: unchanged
- **Flags byte**: unchanged
- **ControlMessage enum** (Ping, Pong, Handshake, HandshakeAck): unchanged
- **MeshEnvelope format**: unchanged
- **Namespace enum** (Mesh, Sync, FileTransfer, Custom): unchanged
- **Typed dispatch pattern**: unchanged (just fewer types to dispatch)
- **ErrorCode enum**: unchanged
- **Protocol version handshake**: unchanged
- **16 MB max message size**: unchanged

---

## 10. Estimated Impact

### Code Changes

| Category | Lines |
|----------|-------|
| Deleted (election.rs) | -469 |
| Deleted (routing.rs) | -160 |
| Removed from node.rs | -250 |
| Removed from handler.rs | -200 |
| Removed from protocol/ | -120 |
| Removed from types/ | -5 |
| Removed from NAPI/examples | -50 |
| **Total removed** | **-1,254** |
| | |
| Simplified code (node.rs) | +50 |
| Simplified code (handler.rs) | +30 |
| New events (PeerConnected etc.) | +20 |
| **Total simplified** | **+100** |
| | |
| New: CLI daemon (daemon.rs, socket.rs, config.rs) | +400 |
| New: CLI commands (golden 7 + extras) | +400 |
| **Total new** | **+800** |
| | |
| **Net change** | **-354** |

### Test Impact

| Category | Change |
|----------|--------|
| Election tests (election.rs `#[cfg(test)]`) | Deleted (~400 lines of tests) |
| Routing tests (routing.rs `#[cfg(test)]`) | Deleted (~60 lines of tests) |
| Handler tests (election/route arms) | Deleted (~100 lines) |
| Integration tests (RoleChanged assertions) | Updated |
| New: P2P broadcast/send tests | Added |
| New: CLI daemon tests | Added |
| New: Target parsing tests | Added |
| New: Config loading tests | Added |

### Complexity Reduction

| Metric | Before | After |
|--------|--------|-------|
| Mesh message types | 8 | 3 |
| Event loop tasks | 3 (transport + device + election) | 2 (transport + device) |
| `MeshNode` fields | 8 | 6 |
| `TransportHandler` fields | 6 | 5 |
| Lock ordering rules | 3 (lifecycle > device > election) | 2 (lifecycle > device) |
| `dispatch_mesh_message` arms | 8 | 3 |
| `MeshNodeConfig` fields | 8 | 7 |
| Ways a message can be delivered | 3 (direct, via-primary, route-broadcast) | 1 (direct) |
| CLI command startup time | 3-10s | <100ms (after first `up`) |

---

## 11. Rationale: Why Not Keep Election as Optional?

One alternative considered was making the election system opt-in rather than removing it:

```rust
// Hypothetical: optional STAR mode
TruffleRuntime::builder()
    .topology(Topology::P2P)       // default
    .topology(Topology::Star)      // opt-in
    .build()
```

This was rejected because:

1. **Maintenance cost.** Two topology modes means two code paths through every handler, two sets of tests, and two mental models. The election code is not "free to keep."

2. **No use case.** Tailscale already provides full-mesh connectivity. STAR topology would only make sense if some nodes couldn't reach each other -- but that's Tailscale's job (via DERP relays), not truffle's.

3. **Complexity budget.** Every line of code is a liability. The election system's 469 lines plus routing's 160 lines plus handler's 130 lines are 759 lines of code that exist only to solve a problem that Tailscale already solved.

4. **Bug surface.** The election system is the #1 source of truffle bugs. Keeping it "optional" doesn't reduce the bug surface -- the code still exists and must be maintained.

The right answer is deletion.

---

## 12. Future Considerations

### 12.1 Large Mesh Optimization

In a P2P full mesh, broadcast messages are O(n) per sender (fan-out to all peers). For small meshes (2-10 nodes, truffle's target), this is fine. If truffle ever needs to support larger meshes (50+ nodes), consider:

- **Gossip protocol**: Each node forwards to a random subset, with probabilistic delivery guarantees
- **Pub/sub filtering**: Only send messages to nodes subscribed to the relevant namespace
- **Broadcast trees**: Dynamically construct minimum spanning trees for broadcast efficiency

These optimizations are not needed now and should not be pre-built.

### 12.2 CLI Plugin System

The `truffle http` and `truffle dev` subgroups hint at a future plugin architecture:

```
truffle <plugin> <command> [args...]
```

Built-in plugins: `http`, `dev`. User plugins could extend the CLI with custom commands. This is out of scope for RFC 010 but the subgroup structure is forward-compatible.

### 12.3 `truffle tunnel`

A potential future command that creates an SSH-like encrypted tunnel between two truffle nodes:

```
truffle tunnel remote-box -L 5432:localhost:5432
```

This would combine `proxy` with more powerful forwarding semantics. Deferred to a future RFC.

### 12.4 `truffle udp` (Raw UDP)

Originally included in the "Golden 7" command list, `truffle udp` has been deferred because:

- The Go sidecar's bridge protocol is TCP-only (binary-framed TCP between Go and Rust)
- The sidecar has no `tsnet:listenUDP` or UDP relay support
- Adding UDP requires bridge-level changes to support datagram semantics (no framing, no guaranteed ordering)

When implemented, it would look like:

```
truffle udp dns:53                     # interactive UDP
echo "ping" | truffle udp echo:9000 --once  # send one datagram
```

This requires a new bridge transport mode or a separate UDP sidecar channel.

---

## 13. Sleep/Wake Handling

Laptop sleep/wake is a first-class concern for truffle. When a device sleeps, all network connections drop. The recovery path:

1. **Tailscale layer.** On wake, Tailscale automatically re-establishes WireGuard tunnels and re-registers with the coordination server. This is handled entirely by Tailscale -- truffle does not need to intervene.

2. **Sidecar layer.** The Go sidecar receives a `monitorState` notification when Tailscale comes back online. This triggers peer re-discovery.

3. **Daemon layer.** The daemon detects peer re-appearance via the sidecar's `monitorState` events and re-dials WebSocket connections to all known peers. The existing reconnection logic (exponential backoff in `reconnect.rs`) handles the re-dial.

4. **Connection layer.** WebSocket connections that were open before sleep will have timed out. The daemon establishes fresh connections and re-sends `device-announce` to each peer.

5. **Application layer.** Active chat sessions display a `reconnecting...` status message when the connection drops and automatically resume when reconnected. File transfers that were in progress must be restarted (or resumed if `--resume` was used). Port forwarding (`truffle proxy`, `truffle expose`) re-establishes automatically.

The user experience: close your laptop lid, open it later, and truffle is back online within a few seconds of Tailscale reconnecting. No manual intervention required.

---

## 14. Platform Support

### 14.1 macOS and Linux

Fully supported. The daemon uses a Unix domain socket (`~/.config/truffle/truffle.sock`) for CLI-to-daemon IPC.

### 14.2 Windows (Deferred)

Windows support is deferred. The daemon IPC uses Unix sockets, which are not available on Windows. A Windows port would require:

- Named pipes for CLI-to-daemon IPC (replacing the Unix socket)
- Windows service integration (replacing daemonization with PID files)
- Path changes for config/state directories (`%APPDATA%` instead of `~/.config`)

This is a non-trivial porting effort and is out of scope for the initial release.

---

## 15. Appendix A: File-Level Change Map

```
crates/truffle-core/src/mesh/
  election.rs       DELETE ENTIRE FILE
  routing.rs         DELETE ENTIRE FILE
  node.rs            SIMPLIFY (~250 lines removed, ~50 lines added)
  handler.rs         SIMPLIFY (~200 lines removed, ~30 lines added)
  device.rs          UNCHANGED
  message_bus.rs     UNCHANGED
  mod.rs             UPDATE (remove election, routing re-exports)

crates/truffle-core/src/protocol/
  types.rs           SIMPLIFY (remove 5 MeshMessageType variants, 5 MeshPayload variants)
  message_types.rs   SIMPLIFY (remove 4 payload structs, update DeviceListPayload)
  envelope.rs        UNCHANGED
  hostname.rs        UNCHANGED

crates/truffle-core/src/types/
  mod.rs             SIMPLIFY (remove DeviceRole enum)

crates/truffle-core/src/transport/
  connection.rs      UNCHANGED
  heartbeat.rs       UNCHANGED
  reconnect.rs       UNCHANGED
  websocket.rs       UNCHANGED

crates/truffle-core/src/bridge/
  manager.rs         UNCHANGED
  header.rs          UNCHANGED
  protocol.rs        UNCHANGED
  shim.rs            UNCHANGED

crates/truffle-core/src/
  runtime.rs         SIMPLIFY (remove prefer_primary builder, election timing config)
  integration.rs     SIMPLIFY (remove election wiring)

crates/truffle-cli/src/
  main.rs            REWRITE (new command structure)
  daemon.rs          NEW (daemon server + PID file)
  socket.rs          NEW (Unix socket JSON-RPC)
  config.rs          NEW (config.toml parsing)
  target.rs          NEW (target address parsing)
  commands/
    up.rs            NEW
    down.rs          NEW
    ls.rs            NEW (replaces peers.rs)
    status.rs        REWRITE
    tcp.rs           NEW
    ws.rs            NEW
    proxy.rs         REWRITE (port forwarding, not HTTP prefix proxy)
    expose.rs        NEW
    ping.rs          NEW
    doctor.rs        NEW
    http/
      proxy.rs       NEW (HTTP path-prefix proxy, from old proxy.rs)
      serve.rs       REWRITE (from old serve.rs)
    dev/
      send.rs        REWRITE (from old send.rs)
      events.rs      NEW
      connections.rs NEW
```

---

## 16. Appendix B: Decision Log

| Decision | Alternatives Considered | Rationale |
|----------|------------------------|-----------|
| Delete STAR topology entirely | Keep as opt-in, deprecate gradually | Tailscale already provides full mesh. Maintaining two topologies doubles complexity for zero user value. |
| Delete election system entirely | Simplify to "first node wins" | Even a trivial election adds state, events, and edge cases. P2P needs no leader. |
| Unix socket for daemon IPC | TCP socket, named pipe, gRPC | Unix socket is the standard for local daemons on macOS/Linux. No network exposure. Natural permission model via filesystem. |
| JSON-RPC for daemon protocol | Custom binary protocol, gRPC | JSON-RPC is trivial to implement, debug, and extend. Performance is irrelevant for CLI commands. |
| TOML for config | YAML, JSON, env vars only | TOML is idiomatic for Rust CLI tools (Cargo.toml precedent). Readable, well-supported by `toml` crate. |
| Daemon mode for CLI | Start/stop runtime per command | 3-10s startup per command is unacceptable for a CLI tool. Daemon makes commands instant. |
| Keep `device-list` message type | Remove entirely | Useful for mesh bootstrapping (a new node can learn about peers it hasn't connected to yet via a peer that knows more). Just remove the `primary_id` field. |
| Rename `Device*` events to `Peer*` | Keep `Device*` naming | "Peer" is more natural for P2P and aligns with Tailscale terminology. |
