use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, RwLock};

use crate::protocol::envelope::{MeshEnvelope, MESH_NAMESPACE};
use crate::protocol::message_types::{
    DeviceAnnouncePayload, DeviceGoodbyePayload, DeviceListPayload,
    ElectionCandidatePayload, ElectionResultPayload, MeshMessage,
};
use crate::transport::connection::{ConnectionManager, ConnectionStatus, TransportEvent};
use crate::types::{BaseDevice, DeviceRole, TailnetPeer};

use super::device::{DeviceEvent, DeviceIdentity, DeviceManager};
use super::election::{ElectionConfig, ElectionEvent, ElectionTimingConfig, PrimaryElection};
use super::message_bus::MeshMessageBus;
use super::routing;

/// Default announce interval (30 seconds).
const DEFAULT_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);

/// Default discovery timeout (5 seconds).
const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Timing configuration for the MeshNode.
#[derive(Debug, Clone)]
pub struct MeshTimingConfig {
    pub announce_interval: Duration,
    pub discovery_timeout: Duration,
    pub election: ElectionTimingConfig,
}

impl Default for MeshTimingConfig {
    fn default() -> Self {
        Self {
            announce_interval: DEFAULT_ANNOUNCE_INTERVAL,
            discovery_timeout: DEFAULT_DISCOVERY_TIMEOUT,
            election: ElectionTimingConfig::default(),
        }
    }
}

/// Configuration for the MeshNode.
#[derive(Debug, Clone)]
pub struct MeshNodeConfig {
    pub device_id: String,
    pub device_name: String,
    pub device_type: String,
    pub hostname_prefix: String,
    pub prefer_primary: bool,
    pub capabilities: Vec<String>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    pub timing: MeshTimingConfig,
}

/// Events emitted by the MeshNode to the application layer.
#[derive(Debug, Clone)]
pub enum MeshNodeEvent {
    Started,
    Stopped,
    AuthRequired(String),
    DeviceDiscovered(BaseDevice),
    DeviceUpdated(BaseDevice),
    DeviceOffline(String),
    DevicesChanged(Vec<BaseDevice>),
    RoleChanged { role: DeviceRole, is_primary: bool },
    PrimaryChanged(Option<String>),
    /// Application-level message received (non-mesh namespace).
    Message(IncomingMeshMessage),
    Error(String),
}

/// An incoming mesh message from a peer.
#[derive(Debug, Clone)]
pub struct IncomingMeshMessage {
    pub from: Option<String>,
    pub connection_id: String,
    pub namespace: String,
    pub msg_type: String,
    pub payload: serde_json::Value,
}

/// MeshNode - The main entry point for Truffle mesh networking.
///
/// Orchestrates device discovery, STAR topology routing, primary election,
/// and message serialization.
pub struct MeshNode {
    config: MeshNodeConfig,
    device_manager: Arc<RwLock<DeviceManager>>,
    election: Arc<RwLock<PrimaryElection>>,
    connection_manager: Arc<ConnectionManager>,
    message_bus: Arc<MeshMessageBus>,

    running: Arc<RwLock<bool>>,
    started_at: Arc<RwLock<u64>>,

    /// Channel for MeshNode events to the application (broadcast supports multiple consumers).
    event_tx: broadcast::Sender<MeshNodeEvent>,

    /// Event receivers from DeviceManager and PrimaryElection, stored here so
    /// `start_event_loop()` can take them. Wrapped in Mutex<Option<>> because
    /// they are moved into the event loop once and cannot be reused.
    device_event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<DeviceEvent>>>,
    election_event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<ElectionEvent>>>,

    /// Handle for the announce interval task.
    announce_handle: Arc<RwLock<Option<tokio::task::AbortHandle>>>,
    /// Handle for the event processing task.
    event_loop_handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
}

impl MeshNode {
    /// Create a new MeshNode.
    ///
    /// Returns the MeshNode and a broadcast event receiver. Additional receivers
    /// can be obtained via `subscribe_events()`.
    pub fn new(
        config: MeshNodeConfig,
        connection_manager: Arc<ConnectionManager>,
    ) -> (Self, broadcast::Receiver<MeshNodeEvent>) {
        let (event_tx, event_rx) = broadcast::channel(256);
        let (device_event_tx, device_event_rx) = mpsc::channel(256);
        let (election_event_tx, election_event_rx) = mpsc::channel(256);
        let (bus_event_tx, _bus_event_rx) = mpsc::channel(64);

        let identity = DeviceIdentity {
            id: config.device_id.clone(),
            device_type: config.device_type.clone(),
            name: config.device_name.clone(),
            tailscale_hostname: crate::protocol::hostname::generate_hostname(
                &config.hostname_prefix,
                &config.device_type,
                &config.device_id,
            ),
        };

        let device_manager = DeviceManager::new(
            identity,
            config.hostname_prefix.clone(),
            config.capabilities.clone(),
            config.metadata.clone(),
            device_event_tx.clone(),
        );

        let election = PrimaryElection::new(
            config.timing.election.clone(),
            election_event_tx.clone(),
        );

        let message_bus = MeshMessageBus::new(bus_event_tx);

        let node = Self {
            config,
            device_manager: Arc::new(RwLock::new(device_manager)),
            election: Arc::new(RwLock::new(election)),
            connection_manager,
            message_bus: Arc::new(message_bus),
            running: Arc::new(RwLock::new(false)),
            started_at: Arc::new(RwLock::new(0)),
            event_tx,
            device_event_rx: tokio::sync::Mutex::new(Some(device_event_rx)),
            election_event_rx: tokio::sync::Mutex::new(Some(election_event_rx)),
            announce_handle: Arc::new(RwLock::new(None)),
            event_loop_handle: Arc::new(RwLock::new(None)),
        };

        (node, event_rx)
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Start the mesh node.
    /// Sets up event processing and periodic announcements.
    pub async fn start(&self) {
        {
            let is_running = *self.running.read().await;
            if is_running {
                tracing::warn!("MeshNode already running");
                return;
            }
        }

        let now_ms = current_timestamp_ms();
        *self.started_at.write().await = now_ms;
        *self.running.write().await = true;

        // Configure election
        {
            let mut election = self.election.write().await;
            election.configure(ElectionConfig {
                device_id: self.config.device_id.clone(),
                started_at: now_ms,
                prefer_primary: self.config.prefer_primary,
            });
        }

        tracing::info!(
            "MeshNode starting with identity: {} ({})",
            self.config.device_id,
            self.config.device_name
        );

        // Start the event processing loop
        self.start_event_loop().await;

        // Start periodic announcements
        self.start_announce_interval().await;

        let _ = self.event_tx.send(MeshNodeEvent::Started);
    }

    /// Stop the mesh node.
    pub async fn stop(&self) {
        let is_running = *self.running.read().await;
        if !is_running {
            return;
        }

        tracing::info!("MeshNode stopping");

        // Stop announce interval
        self.stop_announce_interval().await;

        // Broadcast goodbye
        self.broadcast_goodbye().await;

        // Stop event loop
        {
            let mut handle = self.event_loop_handle.write().await;
            if let Some(h) = handle.take() {
                h.abort();
            }
        }

        *self.running.write().await = false;

        // Clean up state
        {
            let mut dm = self.device_manager.write().await;
            dm.set_local_offline();
            dm.clear();
        }
        {
            let mut election = self.election.write().await;
            election.reset();
        }

        let _ = self.event_tx.send(MeshNodeEvent::Stopped);
    }

    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }

    // ── Device info ───────────────────────────────────────────────────────

    pub async fn local_device(&self) -> BaseDevice {
        self.device_manager.read().await.local_device().clone()
    }

    pub async fn device_id(&self) -> String {
        self.device_manager.read().await.device_id().to_string()
    }

    pub async fn devices(&self) -> Vec<BaseDevice> {
        self.device_manager.read().await.devices()
    }

    pub async fn device_by_id(&self, id: &str) -> Option<BaseDevice> {
        self.device_manager.read().await.device_by_id(id).cloned()
    }

    // ── Role management ───────────────────────────────────────────────────

    pub async fn is_primary(&self) -> bool {
        self.election.read().await.is_primary()
    }

    pub async fn primary_id(&self) -> Option<String> {
        self.election.read().await.primary_id().map(|s| s.to_string())
    }

    pub async fn role(&self) -> DeviceRole {
        if self.is_primary().await {
            DeviceRole::Primary
        } else {
            DeviceRole::Secondary
        }
    }

    // ── Messaging ─────────────────────────────────────────────────────────

    /// Send an envelope to a specific device, using STAR routing if needed.
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

        // Check for direct connection
        let conn = self.connection_manager.get_connection_by_device(device_id).await;
        if conn.is_some() {
            return self.send_envelope_direct(device_id, envelope).await;
        }

        // Route via primary if we're a secondary
        if !self.is_primary().await {
            if let Some(primary_id) = self.primary_id().await {
                let route_envelope = routing::wrap_route_message(device_id, envelope);
                return self.send_envelope_direct(&primary_id, &route_envelope).await;
            }
        }

        tracing::warn!("No connection to device {device_id}");
        false
    }

    /// Broadcast an envelope to all connected devices.
    pub async fn broadcast_envelope(&self, envelope: &MeshEnvelope) {
        let local_id = self.config.device_id.clone();

        if self.is_primary().await {
            // Primary: send directly to all connected devices
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
        } else {
            // Secondary: route broadcast through primary
            if let Some(primary_id) = self.primary_id().await {
                let route_envelope = routing::wrap_route_broadcast(envelope);
                self.send_envelope_direct(&primary_id, &route_envelope).await;
            }
        }
    }

    /// Send an envelope directly to a device via its connection.
    async fn send_envelope_direct(&self, device_id: &str, envelope: &MeshEnvelope) -> bool {
        let conn = match self.connection_manager.get_connection_by_device(device_id).await {
            Some(c) => c,
            None => return false,
        };

        let mut env_with_ts = envelope.clone();
        if env_with_ts.timestamp.is_none() {
            env_with_ts.timestamp = Some(current_timestamp_ms());
        }

        let value = match serde_json::to_value(&env_with_ts) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize envelope: {e}");
                return false;
            }
        };

        match self.connection_manager.send(&conn.id, &value).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Failed to send to {device_id}: {e}");
                false
            }
        }
    }

    /// Get the message bus for application-layer pub/sub.
    pub fn message_bus(&self) -> Arc<MeshMessageBus> {
        self.message_bus.clone()
    }

    /// Subscribe to MeshNode events. Returns a new broadcast receiver.
    ///
    /// Multiple consumers can subscribe independently. Each receiver gets
    /// all events. If a receiver falls behind by the channel capacity (256),
    /// it will receive a `Lagged` error indicating how many events were missed.
    pub fn subscribe_events(&self) -> broadcast::Receiver<MeshNodeEvent> {
        self.event_tx.subscribe()
    }

    /// Emit an event to all subscribers.
    ///
    /// Used by the NAPI layer to forward sidecar lifecycle events (e.g.,
    /// AuthRequired, Error) into the MeshNode event stream.
    pub fn emit_event(&self, event: MeshNodeEvent) {
        let _ = self.event_tx.send(event);
    }

    // ── Discovery ─────────────────────────────────────────────────────────

    /// Handle discovered tailnet peers. Creates connections to matching peers.
    pub async fn handle_tailnet_peers(&self, peers: &[TailnetPeer]) {
        let identity = self.device_manager.read().await.device_identity().clone();

        tracing::info!("Discovered {} tailnet peers", peers.len());

        for peer in peers {
            if peer.hostname == identity.tailscale_hostname {
                continue;
            }

            let device = {
                let mut dm = self.device_manager.write().await;
                dm.add_discovered_peer(peer)
            };

            if let Some(device) = device {
                if peer.online {
                    let conn = self.connection_manager.get_connection_by_device(&device.id).await;
                    if conn.is_none() || conn.map(|c| c.status) != Some(ConnectionStatus::Connected) {
                        // Connection initiation would go through the bridge/shim layer.
                        // The MeshNode doesn't directly dial; it signals the need.
                        tracing::info!("Should connect to peer {} ({})", device.name, device.id);
                    }
                }
            }
        }

        let online_devices = self.device_manager.read().await.online_devices();
        let has_primary = self.election.read().await.primary_id().is_some();

        if !has_primary && online_devices.is_empty() {
            tracing::info!("No other devices found, becoming primary");
            let mut dm = self.device_manager.write().await;
            dm.set_local_role(DeviceRole::Primary);
            let mut election = self.election.write().await;
            election.set_primary(&identity.id);
            let _ = self.event_tx.send(MeshNodeEvent::RoleChanged {
                role: DeviceRole::Primary,
                is_primary: true,
            });
        } else if !has_primary && !online_devices.is_empty() {
            // Wait for discovery timeout then start election if still no primary
            let discovery_timeout = self.config.timing.discovery_timeout;
            let election = self.election.clone();
            tokio::spawn(async move {
                tokio::time::sleep(discovery_timeout).await;
                let mut election = election.write().await;
                if election.primary_id().is_none() {
                    election.handle_no_primary_on_startup();
                }
            });
        }
    }

    /// Set the local device as online.
    pub async fn set_local_online(&self, tailscale_ip: &str, dns_name: Option<&str>) {
        let started_at = *self.started_at.read().await;
        let mut dm = self.device_manager.write().await;
        dm.set_local_online(tailscale_ip, started_at, dns_name);
    }

    // ── Internal: incoming data handling ──────────────────────────────────

    /// Handle an incoming transport message (called from the event loop).
    #[allow(dead_code)]
    async fn handle_incoming_data(&self, connection_id: &str, data: &serde_json::Value) {
        let envelope: MeshEnvelope = match serde_json::from_value(data.clone()) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Invalid mesh envelope: {e}");
                return;
            }
        };

        let conn = self.connection_manager.get_connection(connection_id).await;
        let from_device_id = conn.and_then(|c| c.device_id);

        if envelope.namespace == MESH_NAMESPACE {
            if envelope.msg_type == "message" {
                // Internal mesh message
                let message: MeshMessage = match serde_json::from_value(envelope.payload.clone()) {
                    Ok(m) => m,
                    Err(_) => return,
                };

                // Handle device:announce specially to bind deviceId to connection
                if message.msg_type == "device:announce" {
                    if let Ok(payload) = serde_json::from_value::<DeviceAnnouncePayload>(message.payload.clone()) {
                        self.connection_manager.set_device_id(connection_id, payload.device.id.clone()).await;
                    }
                }

                self.handle_mesh_message(&message).await;
            } else {
                // Route envelope (route:message, route:broadcast)
                self.handle_route_envelope(connection_id, from_device_id.as_deref(), &envelope).await;
            }
            return;
        }

        // Application-level message
        let _ = self.event_tx.send(MeshNodeEvent::Message(IncomingMeshMessage {
            from: from_device_id,
            connection_id: connection_id.to_string(),
            namespace: envelope.namespace,
            msg_type: envelope.msg_type,
            payload: envelope.payload,
        }));
    }

    /// Handle STAR routing envelopes (route:message, route:broadcast).
    #[allow(dead_code)]
    async fn handle_route_envelope(
        &self,
        connection_id: &str,
        from_device_id: Option<&str>,
        envelope: &MeshEnvelope,
    ) {
        match envelope.msg_type.as_str() {
            "route:message" => {
                if !self.is_primary().await {
                    tracing::warn!("Received route:message but not primary");
                    return;
                }
                if let (Some(target), Some(inner)) = (
                    envelope.payload.get("targetDeviceId").and_then(|v| v.as_str()),
                    envelope.payload.get("envelope"),
                ) {
                    if let Ok(inner_envelope) = serde_json::from_value::<MeshEnvelope>(inner.clone()) {
                        self.send_envelope_direct(target, &inner_envelope).await;
                    }
                }
            }
            "route:broadcast" => {
                if !self.is_primary().await {
                    tracing::warn!("Received route:broadcast but not primary");
                    return;
                }
                if let Some(inner) = envelope.payload.get("envelope") {
                    if let Ok(inner_envelope) = serde_json::from_value::<MeshEnvelope>(inner.clone()) {
                        // Forward to all connected devices except the sender
                        let conns = self.connection_manager.get_connections().await;
                        for conn in &conns {
                            if let Some(ref did) = conn.device_id {
                                if from_device_id != Some(did.as_str())
                                    && conn.status == ConnectionStatus::Connected
                                {
                                    self.send_envelope_direct(did, &inner_envelope).await;
                                }
                            }
                        }

                        // Deliver locally too
                        let _ = self.event_tx.send(MeshNodeEvent::Message(IncomingMeshMessage {
                            from: from_device_id.map(|s| s.to_string()),
                            connection_id: connection_id.to_string(),
                            namespace: inner_envelope.namespace,
                            msg_type: inner_envelope.msg_type,
                            payload: inner_envelope.payload,
                        }));
                    }
                }
            }
            _ => {
                tracing::debug!("Unknown mesh envelope type: {}", envelope.msg_type);
            }
        }
    }

    /// Handle internal mesh protocol messages.
    #[allow(dead_code)]
    async fn handle_mesh_message(&self, message: &MeshMessage) {
        match message.msg_type.as_str() {
            "device:announce" => {
                if let Ok(payload) = serde_json::from_value::<DeviceAnnouncePayload>(message.payload.clone()) {
                    let mut dm = self.device_manager.write().await;
                    dm.handle_device_announce(&message.from, &payload);
                }
            }
            "device:list" => {
                if let Ok(payload) = serde_json::from_value::<DeviceListPayload>(message.payload.clone()) {
                    {
                        let mut dm = self.device_manager.write().await;
                        dm.handle_device_list(&message.from, &payload);
                    }
                    if !payload.primary_id.is_empty() {
                        let mut election = self.election.write().await;
                        election.set_primary(&payload.primary_id);
                    }
                }
            }
            "device:goodbye" => {
                let mut dm = self.device_manager.write().await;
                dm.handle_device_goodbye(&message.from);
            }
            "election:start" => {
                let mut election = self.election.write().await;
                election.handle_election_start(&message.from);
            }
            "election:candidate" => {
                if let Ok(payload) = serde_json::from_value::<ElectionCandidatePayload>(message.payload.clone()) {
                    let mut election = self.election.write().await;
                    election.handle_election_candidate(&message.from, &payload);
                }
            }
            "election:result" => {
                if let Ok(payload) = serde_json::from_value::<ElectionResultPayload>(message.payload.clone()) {
                    let mut election = self.election.write().await;
                    election.handle_election_result(&message.from, &payload);
                }
            }
            _ => {
                tracing::debug!("Unknown mesh message type: {}", message.msg_type);
            }
        }
    }

    // ── Internal: messaging helpers ───────────────────────────────────────

    /// Send a mesh message (wrapped in mesh namespace envelope) to a connection.
    async fn send_mesh_message_raw(&self, connection_id: &str, message: &MeshMessage) -> bool {
        let envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "message".to_string(),
            payload: serde_json::to_value(message).unwrap_or_default(),
            timestamp: Some(current_timestamp_ms()),
        };
        let value = match serde_json::to_value(&envelope) {
            Ok(v) => v,
            Err(_) => return false,
        };
        self.connection_manager.send(connection_id, &value).await.is_ok()
    }

    /// Broadcast a mesh message to all connected peers.
    async fn broadcast_mesh_message(&self, message: &MeshMessage) {
        let conns = self.connection_manager.get_connections().await;
        for conn in &conns {
            if conn.status == ConnectionStatus::Connected {
                self.send_mesh_message_raw(&conn.id, message).await;
            }
        }
    }

    /// Broadcast device announce to all peers.
    #[allow(dead_code)]
    async fn broadcast_announce(&self) {
        let local_device = self.local_device().await;
        let announce = MeshMessage::new(
            "device:announce",
            &self.config.device_id,
            serde_json::to_value(&DeviceAnnouncePayload {
                device: local_device,
                protocol_version: 2,
            })
            .unwrap_or_default(),
        );
        self.broadcast_mesh_message(&announce).await;
    }

    /// Broadcast goodbye to all peers.
    async fn broadcast_goodbye(&self) {
        let goodbye = MeshMessage::new(
            "device:goodbye",
            &self.config.device_id,
            serde_json::to_value(&DeviceGoodbyePayload {
                device_id: self.config.device_id.clone(),
                reason: "shutdown".to_string(),
            })
            .unwrap_or_default(),
        );
        self.broadcast_mesh_message(&goodbye).await;
    }

    /// Broadcast the full device list (primary only).
    #[allow(dead_code)]
    async fn broadcast_device_list(&self) {
        let local_device = self.local_device().await;
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
            })
            .unwrap_or_default(),
        );
        self.broadcast_mesh_message(&list).await;
    }

    // ── Internal: event loop ──────────────────────────────────────────────

    /// Start the event processing loop that handles transport events,
    /// device events, and election events.
    async fn start_event_loop(&self) {
        let mut transport_rx = self.connection_manager.subscribe();

        // Take the event receivers that were stored at construction.
        // These are connected to the senders inside DeviceManager and PrimaryElection.
        let mut device_event_rx = self
            .device_event_rx
            .lock()
            .await
            .take()
            .expect("device_event_rx already taken (start_event_loop called twice?)");
        let mut election_event_rx = self
            .election_event_rx
            .lock()
            .await
            .take()
            .expect("election_event_rx already taken (start_event_loop called twice?)");

        let event_tx = self.event_tx.clone();
        let connection_manager = self.connection_manager.clone();
        let device_manager = self.device_manager.clone();
        let election = self.election.clone();
        let config = self.config.clone();
        let message_bus = self.message_bus.clone();

        // Spawn a task to process device events
        let event_tx_dev = event_tx.clone();
        let election_dev = election.clone();
        tokio::spawn(async move {
            while let Some(event) = device_event_rx.recv().await {
                match &event {
                    DeviceEvent::DeviceDiscovered(d) => {
                        let _ = event_tx_dev.send(MeshNodeEvent::DeviceDiscovered(d.clone()));
                    }
                    DeviceEvent::DeviceUpdated(d) => {
                        let _ = event_tx_dev.send(MeshNodeEvent::DeviceUpdated(d.clone()));
                    }
                    DeviceEvent::DeviceOffline(id) => {
                        let _ = event_tx_dev.send(MeshNodeEvent::DeviceOffline(id.clone()));
                        // Check if the primary went offline
                        let primary_id = election_dev.read().await.primary_id().map(|s| s.to_string());
                        if primary_id.as_deref() == Some(id.as_str()) {
                            let mut el = election_dev.write().await;
                            el.handle_primary_lost(id);
                        }
                    }
                    DeviceEvent::DevicesChanged(devs) => {
                        let _ = event_tx_dev.send(MeshNodeEvent::DevicesChanged(devs.clone()));
                    }
                    DeviceEvent::PrimaryChanged(pid) => {
                        let _ = event_tx_dev.send(MeshNodeEvent::PrimaryChanged(pid.clone()));
                    }
                    DeviceEvent::LocalDeviceChanged(_) => {
                        // Could trigger a broadcast announce, but we handle that
                        // via the periodic announce interval.
                    }
                }
            }
        });

        // Spawn a task to process election events
        let event_tx_el = event_tx.clone();
        let connection_manager_el = connection_manager.clone();
        let device_manager_el = device_manager.clone();
        let _config_el = config.clone();
        tokio::spawn(async move {
            while let Some(event) = election_event_rx.recv().await {
                match event {
                    ElectionEvent::PrimaryElected { device_id, is_local } => {
                        tracing::info!("Primary elected: {device_id} (is_local={is_local})");

                        let role = if is_local { DeviceRole::Primary } else { DeviceRole::Secondary };
                        {
                            let mut dm = device_manager_el.write().await;
                            dm.set_local_role(role);
                            // Update all device roles
                            for device in dm.devices() {
                                let dev_role = if device.id == device_id {
                                    DeviceRole::Primary
                                } else {
                                    DeviceRole::Secondary
                                };
                                dm.set_device_role(&device.id, dev_role);
                            }
                        }

                        let _ = event_tx_el.send(MeshNodeEvent::RoleChanged {
                            role,
                            is_primary: is_local,
                        });

                        if is_local {
                            // Broadcast device list as new primary
                            // (simplified - in full implementation we'd call broadcast_device_list)
                        }
                    }
                    ElectionEvent::PrimaryLost { previous_primary_id } => {
                        tracing::info!("Primary lost: {previous_primary_id}");
                    }
                    ElectionEvent::Broadcast(message) => {
                        // Send mesh message to all peers
                        let conns = connection_manager_el.get_connections().await;
                        for conn in &conns {
                            if conn.status == ConnectionStatus::Connected {
                                let envelope = MeshEnvelope {
                                    namespace: MESH_NAMESPACE.to_string(),
                                    msg_type: "message".to_string(),
                                    payload: serde_json::to_value(&message).unwrap_or_default(),
                                    timestamp: Some(current_timestamp_ms()),
                                };
                                if let Ok(value) = serde_json::to_value(&envelope) {
                                    let _ = connection_manager_el.send(&conn.id, &value).await;
                                }
                            }
                        }
                    }
                    ElectionEvent::ElectionStarted => {
                        tracing::info!("Election started");
                    }
                }
            }
        });

        // Main transport event processing loop
        let this_event_tx = event_tx.clone();
        let this_conn_mgr = connection_manager.clone();
        let this_dm = device_manager.clone();
        let this_election = election.clone();
        let this_config = config.clone();
        let this_message_bus = message_bus.clone();

        let handle = tokio::spawn(async move {
            loop {
                match transport_rx.recv().await {
                    Ok(TransportEvent::Connected(conn)) => {
                        // Handle new connection: send announce
                        tracing::info!("Transport connected: {} ({:?})", conn.id, conn.direction);

                        // Send our device announce
                        let local_device = this_dm.read().await.local_device().clone();
                        let announce = MeshMessage::new(
                            "device:announce",
                            &this_config.device_id,
                            serde_json::to_value(&DeviceAnnouncePayload {
                                device: local_device.clone(),
                                protocol_version: 2,
                            }).unwrap_or_default(),
                        );

                        let envelope = MeshEnvelope {
                            namespace: MESH_NAMESPACE.to_string(),
                            msg_type: "message".to_string(),
                            payload: serde_json::to_value(&announce).unwrap_or_default(),
                            timestamp: Some(current_timestamp_ms()),
                        };
                        if let Ok(value) = serde_json::to_value(&envelope) {
                            let _ = this_conn_mgr.send(&conn.id, &value).await;
                        }

                        // If primary, send device list
                        if this_election.read().await.is_primary() {
                            let devices = {
                                let dm = this_dm.read().await;
                                let mut devs = vec![local_device];
                                devs.extend(dm.devices());
                                devs
                            };
                            let list = MeshMessage::new(
                                "device:list",
                                &this_config.device_id,
                                serde_json::to_value(&DeviceListPayload {
                                    devices,
                                    primary_id: this_config.device_id.clone(),
                                }).unwrap_or_default(),
                            );
                            let envelope = MeshEnvelope {
                                namespace: MESH_NAMESPACE.to_string(),
                                msg_type: "message".to_string(),
                                payload: serde_json::to_value(&list).unwrap_or_default(),
                                timestamp: Some(current_timestamp_ms()),
                            };
                            if let Ok(value) = serde_json::to_value(&envelope) {
                                let _ = this_conn_mgr.send(&conn.id, &value).await;
                            }
                        }
                    }
                    Ok(TransportEvent::Disconnected { connection_id, reason }) => {
                        tracing::info!("Transport disconnected: {connection_id} ({reason})");

                        // Mark disconnected devices offline
                        let devices = this_dm.read().await.devices();
                        for device in &devices {
                            let conn = this_conn_mgr.get_connection_by_device(&device.id).await;
                            if conn.is_none() {
                                let mut dm = this_dm.write().await;
                                dm.mark_device_offline(&device.id);
                            }
                        }
                    }
                    Ok(TransportEvent::DeviceIdentified { connection_id, device_id }) => {
                        tracing::info!("Device identified: {connection_id} -> {device_id}");
                    }
                    Ok(TransportEvent::Message { connection_id, message }) => {
                        // Parse as envelope and handle
                        let envelope: Result<MeshEnvelope, _> = serde_json::from_value(message.payload.clone());
                        if let Ok(envelope) = envelope {
                            let conn = this_conn_mgr.get_connection(&connection_id).await;
                            let from_device_id = conn.and_then(|c| c.device_id);

                            if envelope.namespace == MESH_NAMESPACE {
                                if envelope.msg_type == "message" {
                                    if let Ok(mesh_msg) = serde_json::from_value::<MeshMessage>(envelope.payload.clone()) {
                                        // Bind device ID from announce
                                        if mesh_msg.msg_type == "device:announce" {
                                            if let Ok(payload) = serde_json::from_value::<DeviceAnnouncePayload>(mesh_msg.payload.clone()) {
                                                this_conn_mgr.set_device_id(&connection_id, payload.device.id.clone()).await;
                                            }
                                        }

                                        // Handle mesh messages
                                        match mesh_msg.msg_type.as_str() {
                                            "device:announce" => {
                                                if let Ok(payload) = serde_json::from_value::<DeviceAnnouncePayload>(mesh_msg.payload.clone()) {
                                                    let mut dm = this_dm.write().await;
                                                    dm.handle_device_announce(&mesh_msg.from, &payload);
                                                }
                                            }
                                            "device:list" => {
                                                if let Ok(payload) = serde_json::from_value::<DeviceListPayload>(mesh_msg.payload.clone()) {
                                                    {
                                                        let mut dm = this_dm.write().await;
                                                        dm.handle_device_list(&mesh_msg.from, &payload);
                                                    }
                                                    if !payload.primary_id.is_empty() {
                                                        let mut el = this_election.write().await;
                                                        el.set_primary(&payload.primary_id);
                                                    }
                                                }
                                            }
                                            "device:goodbye" => {
                                                let mut dm = this_dm.write().await;
                                                dm.handle_device_goodbye(&mesh_msg.from);
                                            }
                                            "election:start" => {
                                                let mut el = this_election.write().await;
                                                el.handle_election_start(&mesh_msg.from);
                                            }
                                            "election:candidate" => {
                                                if let Ok(payload) = serde_json::from_value::<ElectionCandidatePayload>(mesh_msg.payload.clone()) {
                                                    let mut el = this_election.write().await;
                                                    el.handle_election_candidate(&mesh_msg.from, &payload);
                                                }
                                            }
                                            "election:result" => {
                                                if let Ok(payload) = serde_json::from_value::<ElectionResultPayload>(mesh_msg.payload.clone()) {
                                                    let mut el = this_election.write().await;
                                                    el.handle_election_result(&mesh_msg.from, &payload);
                                                }
                                            }
                                            "election:timeout" => {
                                                // Internal: decide election
                                                let mut el = this_election.write().await;
                                                el.decide_election();
                                            }
                                            _ => {
                                                tracing::debug!("Unknown mesh message type: {}", mesh_msg.msg_type);
                                            }
                                        }
                                    }
                                } else {
                                    // Route envelopes (route:message, route:broadcast)
                                    let is_primary = this_election.read().await.is_primary();
                                    match envelope.msg_type.as_str() {
                                        "route:message" if is_primary => {
                                            if let (Some(target), Some(inner)) = (
                                                envelope.payload.get("targetDeviceId").and_then(|v| v.as_str()),
                                                envelope.payload.get("envelope"),
                                            ) {
                                                if let Ok(inner_envelope) = serde_json::from_value::<MeshEnvelope>(inner.clone()) {
                                                    if let Some(conn) = this_conn_mgr.get_connection_by_device(target).await {
                                                        let mut env_ts = inner_envelope.clone();
                                                        if env_ts.timestamp.is_none() {
                                                            env_ts.timestamp = Some(current_timestamp_ms());
                                                        }
                                                        if let Ok(value) = serde_json::to_value(&env_ts) {
                                                            let _ = this_conn_mgr.send(&conn.id, &value).await;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        "route:broadcast" if is_primary => {
                                            if let Some(inner) = envelope.payload.get("envelope") {
                                                if let Ok(inner_envelope) = serde_json::from_value::<MeshEnvelope>(inner.clone()) {
                                                    let conns = this_conn_mgr.get_connections().await;
                                                    for c in &conns {
                                                        if let Some(ref did) = c.device_id {
                                                            if from_device_id.as_deref() != Some(did.as_str())
                                                                && c.status == ConnectionStatus::Connected {
                                                                let mut env_ts = inner_envelope.clone();
                                                                if env_ts.timestamp.is_none() {
                                                                    env_ts.timestamp = Some(current_timestamp_ms());
                                                                }
                                                                if let Ok(value) = serde_json::to_value(&env_ts) {
                                                                    let _ = this_conn_mgr.send(&c.id, &value).await;
                                                                }
                                                            }
                                                        }
                                                    }
                                                    // Local delivery
                                                    let incoming = IncomingMeshMessage {
                                                        from: from_device_id,
                                                        connection_id: connection_id.clone(),
                                                        namespace: inner_envelope.namespace,
                                                        msg_type: inner_envelope.msg_type,
                                                        payload: inner_envelope.payload,
                                                    };
                                                    let _ = this_event_tx.send(MeshNodeEvent::Message(incoming.clone()));
                                                    // Also dispatch to MessageBus for pub/sub subscribers
                                                    this_message_bus.dispatch(&super::message_bus::BusMessage {
                                                        from: incoming.from,
                                                        namespace: incoming.namespace,
                                                        msg_type: incoming.msg_type,
                                                        payload: incoming.payload,
                                                    }).await;
                                                }
                                            }
                                        }
                                        _ => {
                                            if !is_primary {
                                                tracing::warn!("Received route envelope but not primary");
                                            }
                                        }
                                    }
                                }
                            } else {
                                // Application-level message
                                let incoming = IncomingMeshMessage {
                                    from: from_device_id,
                                    connection_id: connection_id.clone(),
                                    namespace: envelope.namespace,
                                    msg_type: envelope.msg_type,
                                    payload: envelope.payload,
                                };
                                let _ = this_event_tx.send(MeshNodeEvent::Message(incoming.clone()));
                                // Also dispatch to MessageBus for pub/sub subscribers
                                this_message_bus.dispatch(&super::message_bus::BusMessage {
                                    from: incoming.from,
                                    namespace: incoming.namespace,
                                    msg_type: incoming.msg_type,
                                    payload: incoming.payload,
                                }).await;
                            }
                        }
                    }
                    Ok(TransportEvent::Reconnecting { device_id, attempt, delay }) => {
                        tracing::info!("Reconnecting to {device_id}: attempt {attempt}, delay {delay:?}");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Transport event receiver lagged by {n} messages");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::info!("Transport event channel closed");
                        break;
                    }
                }
            }
        });

        *self.event_loop_handle.write().await = Some(handle);
    }

    // ── Internal: announce interval ───────────────────────────────────────

    async fn start_announce_interval(&self) {
        self.stop_announce_interval().await;

        let interval = self.config.timing.announce_interval;
        let connection_manager = self.connection_manager.clone();
        let device_manager = self.device_manager.clone();
        let device_id = self.config.device_id.clone();

        let handle = tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(interval);
            interval_timer.tick().await; // Skip first immediate tick
            loop {
                interval_timer.tick().await;

                let local_device = device_manager.read().await.local_device().clone();
                let announce = MeshMessage::new(
                    "device:announce",
                    &device_id,
                    serde_json::to_value(&DeviceAnnouncePayload {
                        device: local_device,
                        protocol_version: 2,
                    }).unwrap_or_default(),
                );

                let envelope = MeshEnvelope {
                    namespace: MESH_NAMESPACE.to_string(),
                    msg_type: "message".to_string(),
                    payload: serde_json::to_value(&announce).unwrap_or_default(),
                    timestamp: Some(current_timestamp_ms()),
                };

                let conns = connection_manager.get_connections().await;
                for conn in &conns {
                    if conn.status == ConnectionStatus::Connected {
                        if let Ok(value) = serde_json::to_value(&envelope) {
                            let _ = connection_manager.send(&conn.id, &value).await;
                        }
                    }
                }
            }
        });

        *self.announce_handle.write().await = Some(handle.abort_handle());
    }

    async fn stop_announce_interval(&self) {
        let mut handle = self.announce_handle.write().await;
        if let Some(h) = handle.take() {
            h.abort();
        }
    }
}

fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::connection::TransportConfig;

    fn test_config() -> MeshNodeConfig {
        MeshNodeConfig {
            device_id: "test-dev-1".to_string(),
            device_name: "Test Device".to_string(),
            device_type: "desktop".to_string(),
            hostname_prefix: "app".to_string(),
            prefer_primary: false,
            capabilities: vec![],
            metadata: None,
            timing: MeshTimingConfig::default(),
        }
    }

    #[tokio::test]
    async fn mesh_node_creation() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, _event_rx) = MeshNode::new(test_config(), conn_mgr);

        assert!(!node.is_running().await);
        assert!(!node.is_primary().await);
        assert!(node.primary_id().await.is_none());
        assert_eq!(node.device_id().await, "test-dev-1");
    }

    #[tokio::test]
    async fn mesh_node_start_stop() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        node.start().await;
        assert!(node.is_running().await);

        // Should receive Started event
        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::Started));

        node.stop().await;
        assert!(!node.is_running().await);

        // Should receive Stopped event
        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::Stopped));
    }

    #[tokio::test]
    async fn send_envelope_to_self_delivers_locally() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        node.start().await;
        let _started = event_rx.recv().await; // consume Started

        let envelope = MeshEnvelope::new("custom", "test", serde_json::json!({"hello": "world"}));
        let result = node.send_envelope("test-dev-1", &envelope).await;
        assert!(result);

        let event = event_rx.recv().await.unwrap();
        match event {
            MeshNodeEvent::Message(msg) => {
                assert_eq!(msg.namespace, "custom");
                assert_eq!(msg.msg_type, "test");
                assert_eq!(msg.from, Some("test-dev-1".to_string()));
            }
            other => panic!("Expected Message event, got: {other:?}"),
        }

        node.stop().await;
    }

    #[tokio::test]
    async fn local_device_info() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, _event_rx) = MeshNode::new(test_config(), conn_mgr);

        let device = node.local_device().await;
        assert_eq!(device.id, "test-dev-1");
        assert_eq!(device.name, "Test Device");
        assert_eq!(device.device_type, "desktop");
        assert_eq!(device.tailscale_hostname, "app-desktop-test-dev-1");
    }
}
