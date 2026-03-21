use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, RwLock};

use crate::protocol::envelope::{MeshEnvelope, MESH_NAMESPACE};
use crate::protocol::message_types::{
    DeviceAnnouncePayload, DeviceGoodbyePayload,
    MeshMessage,
};
use crate::transport::connection::{ConnectionManager, ConnectionStatus, TransportEvent};
use crate::types::{AuthStatus, BaseDevice, TailnetPeer};

use super::device::{DeviceEvent, DeviceIdentity, DeviceManager};
use super::message_bus::MeshMessageBus;

/// Default announce interval (30 seconds).
const DEFAULT_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);

/// Default discovery timeout (5 seconds).
const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Timing configuration for the MeshNode.
#[derive(Debug, Clone)]
pub struct MeshTimingConfig {
    pub announce_interval: Duration,
    pub discovery_timeout: Duration,
}

impl Default for MeshTimingConfig {
    fn default() -> Self {
        Self {
            announce_interval: DEFAULT_ANNOUNCE_INTERVAL,
            discovery_timeout: DEFAULT_DISCOVERY_TIMEOUT,
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
    /// Tailscale authentication completed successfully.
    AuthComplete,
    DeviceDiscovered(BaseDevice),
    DeviceUpdated(BaseDevice),
    DeviceOffline(String),
    DevicesChanged(Vec<BaseDevice>),
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
/// Orchestrates device discovery and message serialization.
/// Mutable lifecycle state, grouped under a single lock (CS-6, ARCH-11).
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

/// MeshNode - The main entry point for Truffle mesh networking.
///
/// ## Lock ordering (ARCH-3)
///
/// Always acquire locks in this order to prevent deadlocks:
/// 1. `lifecycle`
/// 2. `device_manager`
pub struct MeshNode {
    config: MeshNodeConfig,
    device_manager: Arc<RwLock<DeviceManager>>,
    connection_manager: Arc<ConnectionManager>,
    message_bus: Arc<MeshMessageBus>,

    /// Grouped lifecycle state (CS-6: replaces 6 separate Arc<RwLock<>>).
    lifecycle: Arc<RwLock<MeshNodeLifecycle>>,

    /// Channel for MeshNode events to the application (broadcast supports multiple consumers).
    event_tx: broadcast::Sender<MeshNodeEvent>,

    /// Event receiver from DeviceManager, stored here so
    /// `start_event_loop()` can take it. Wrapped in Mutex<Option<>> because
    /// it is moved into the event loop once and cannot be reused.
    device_event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<DeviceEvent>>>,
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

        let message_bus = MeshMessageBus::new(bus_event_tx);

        let node = Self {
            config,
            device_manager: Arc::new(RwLock::new(device_manager)),
            connection_manager,
            message_bus: Arc::new(message_bus),
            lifecycle: Arc::new(RwLock::new(MeshNodeLifecycle::default())),
            event_tx,
            device_event_rx: tokio::sync::Mutex::new(Some(device_event_rx)),
        };

        (node, event_rx)
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Start the mesh node.
    /// Sets up event processing and periodic announcements.
    pub async fn start(&self) {
        {
            let lc = self.lifecycle.read().await;
            if lc.running {
                tracing::warn!("MeshNode already running");
                return;
            }
        }

        let now_ms = current_timestamp_ms();
        {
            let mut lc = self.lifecycle.write().await;
            lc.started_at = now_ms;
            lc.running = true;
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
        {
            let lc = self.lifecycle.read().await;
            if !lc.running {
                return;
            }
        }

        tracing::info!("MeshNode stopping");

        // Stop announce interval
        self.stop_announce_interval().await;

        // Broadcast goodbye
        self.broadcast_goodbye().await;

        // Give write pumps time to flush goodbye messages (BUG-7)
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Close all connections (BUG-6 / M-3)
        self.connection_manager.close_all().await;

        // Stop event loop AFTER connections are closed
        {
            let mut lc = self.lifecycle.write().await;
            if let Some(h) = lc.event_loop_handle.take() {
                h.abort();
            }
            lc.running = false;
            lc.auth_status = AuthStatus::Unknown;
            lc.auth_url = None;
        }

        // Clean up state
        {
            let mut dm = self.device_manager.write().await;
            dm.set_local_offline();
            dm.clear();
        }

        // Recreate internal event channels so start() can be called again.
        {
            let (device_event_tx, device_event_rx) = mpsc::channel(256);
            self.device_manager.write().await.replace_event_tx(device_event_tx);
            *self.device_event_rx.lock().await = Some(device_event_rx);
        }

        let _ = self.event_tx.send(MeshNodeEvent::Stopped);
    }

    pub async fn is_running(&self) -> bool {
        self.lifecycle.read().await.running
    }

    /// Access the config (needed by runtime.rs for hostname_prefix).
    pub fn config_ref(&self) -> &MeshNodeConfig {
        &self.config
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

    // ── Messaging ─────────────────────────────────────────────────────────

    /// Send an envelope to a specific device.
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

        tracing::warn!("No connection to device {device_id}");
        false
    }

    /// Broadcast an envelope to all connected devices.
    pub async fn broadcast_envelope(&self, envelope: &MeshEnvelope) {
        let local_id = self.config.device_id.clone();

        // Send directly to all connected devices
        {
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

    /// Get a snapshot of all WebSocket connections (for diagnostics).
    pub async fn connections(&self) -> Vec<crate::transport::connection::WSConnection> {
        self.connection_manager.get_connections().await
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

    // ── Auth state ────────────────────────────────────────────────────────

    /// Get the current Tailscale auth status.
    pub async fn auth_status(&self) -> AuthStatus {
        self.lifecycle.read().await.auth_status.clone()
    }

    /// Get the current Tailscale auth URL (if auth is required).
    pub async fn auth_url(&self) -> Option<String> {
        self.lifecycle.read().await.auth_url.clone()
    }

    /// Set auth status to Required and store the auth URL.
    /// Called when the sidecar emits an AuthRequired event.
    pub async fn set_auth_required(&self, url: &str) {
        {
            let mut lc = self.lifecycle.write().await;
            lc.auth_status = AuthStatus::Required;
            lc.auth_url = Some(url.to_string());
        }
        self.emit_event(MeshNodeEvent::AuthRequired(url.to_string()));
    }

    /// Set auth status to Authenticated and clear the URL.
    /// Called when the sidecar reports a Tailscale IP (auth completed).
    /// Idempotent — only emits AuthComplete once.
    pub async fn set_auth_authenticated(&self) {
        {
            let mut lc = self.lifecycle.write().await;
            if lc.auth_status == AuthStatus::Authenticated {
                return;
            }
            lc.auth_status = AuthStatus::Authenticated;
            lc.auth_url = None;
        }
        self.emit_event(MeshNodeEvent::AuthComplete);
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

    }

    /// Set the local device as online.
    pub async fn set_local_online(&self, tailscale_ip: &str, dns_name: Option<&str>) {
        let started_at = self.lifecycle.read().await.started_at;
        let mut dm = self.device_manager.write().await;
        dm.set_local_online(tailscale_ip, started_at, dns_name);
    }

    // Dead code methods (handle_incoming_data, handle_route_envelope, handle_mesh_message)
    // removed per CS-5 (ARCH-2). Logic now lives in mesh/handler.rs TransportHandler.

    // ── Internal: messaging helpers ───────────────────────────────────────

    /// Send a mesh message (wrapped in mesh namespace envelope) to a connection.
    async fn send_mesh_message_raw(&self, connection_id: &str, message: &MeshMessage) -> bool {
        let msg_payload = match serde_json::to_value(message) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize MeshMessage: {e}");
                return false;
            }
        };
        let envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "message".to_string(),
            payload: msg_payload,
            timestamp: Some(current_timestamp_ms()),
        };
        let value = match serde_json::to_value(&envelope) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize MeshEnvelope: {e}");
                return false;
            }
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

    // broadcast_announce() removed per CS-5 (logic moved to handler.rs)

    /// Broadcast goodbye to all peers.
    async fn broadcast_goodbye(&self) {
        let goodbye_payload = match serde_json::to_value(&DeviceGoodbyePayload {
            device_id: self.config.device_id.clone(),
            reason: "shutdown".to_string(),
        }) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize DeviceGoodbyePayload: {e}");
                return;
            }
        };
        let goodbye = MeshMessage::new(
            "device-goodbye",
            &self.config.device_id,
            goodbye_payload,
        );
        self.broadcast_mesh_message(&goodbye).await;
    }

    // broadcast_device_list() removed per CS-5 (logic moved to handler.rs and election event handler)

    // ── Internal: event loop ──────────────────────────────────────────────

    /// Start the event processing loop that handles transport events
    /// and device events.
    async fn start_event_loop(&self) {
        let mut transport_rx = self.connection_manager.subscribe();

        // Take the event receiver that was stored at construction.
        // It is connected to the sender inside DeviceManager.
        let mut device_event_rx = self
            .device_event_rx
            .lock()
            .await
            .take()
            .expect("device_event_rx already taken (start_event_loop called twice?)");

        let event_tx = self.event_tx.clone();
        let connection_manager = self.connection_manager.clone();
        let device_manager = self.device_manager.clone();
        let config = self.config.clone();
        let message_bus = self.message_bus.clone();

        // Spawn a task to process device events
        let event_tx_dev = event_tx.clone();
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
                    }
                    DeviceEvent::DevicesChanged(devs) => {
                        let _ = event_tx_dev.send(MeshNodeEvent::DevicesChanged(devs.clone()));
                    }
                    DeviceEvent::LocalDeviceChanged(_) => {
                        // Could trigger a broadcast announce, but we handle that
                        // via the periodic announce interval.
                    }
                }
            }
        });

        // Main transport event processing loop (CS-5: extracted to TransportHandler)
        let handler = Arc::new(super::handler::TransportHandler {
            config: config.clone(),
            connection_manager: connection_manager.clone(),
            device_manager: device_manager.clone(),
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

        self.lifecycle.write().await.event_loop_handle = Some(handle);
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
                let announce_payload = match serde_json::to_value(&DeviceAnnouncePayload {
                    device: local_device,
                    protocol_version: 2,
                }) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("Failed to serialize DeviceAnnouncePayload in announce interval: {e}");
                        continue;
                    }
                };
                let announce = MeshMessage::new(
                    "device-announce",
                    &device_id,
                    announce_payload,
                );

                let msg_payload = match serde_json::to_value(&announce) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("Failed to serialize announce MeshMessage: {e}");
                        continue;
                    }
                };
                let envelope = MeshEnvelope {
                    namespace: MESH_NAMESPACE.to_string(),
                    msg_type: "message".to_string(),
                    payload: msg_payload,
                    timestamp: Some(current_timestamp_ms()),
                };

                match serde_json::to_value(&envelope) {
                    Ok(value) => {
                        let conns = connection_manager.get_connections().await;
                        for conn in &conns {
                            if conn.status == ConnectionStatus::Connected {
                                let _ = connection_manager.send(&conn.id, &value).await;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to serialize announce envelope: {e}");
                    }
                }
            }
        });

        self.lifecycle.write().await.announce_handle = Some(handle.abort_handle());
    }

    async fn stop_announce_interval(&self) {
        let mut lc = self.lifecycle.write().await;
        if let Some(h) = lc.announce_handle.take() {
            h.abort();
        }
    }
}

pub(crate) fn current_timestamp_ms() -> u64 {
    crate::util::current_timestamp_ms()
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

    // ── CS-4: Shutdown tests (BUG-5,6,7) ──────────────────────────────────

    /// BUG-5,6: stop() must emit Stopped event.
    #[tokio::test]
    async fn stop_emits_stopped_event() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        node.start().await;
        let _started = event_rx.recv().await; // consume Started

        node.stop().await;

        // Should receive Stopped event
        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::Stopped),
            "stop() must emit MeshNodeEvent::Stopped");
    }

    /// stop() must recreate event channels so start() can be called again.
    #[tokio::test]
    async fn stop_then_start_works() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        // First cycle
        node.start().await;
        assert!(node.is_running().await);
        let _ = event_rx.recv().await; // Started

        node.stop().await;
        assert!(!node.is_running().await);
        let _ = event_rx.recv().await; // Stopped

        // Second cycle: should work without panic
        node.start().await;
        assert!(node.is_running().await);
        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::Started),
            "After stop + start, node must emit Started again");

        node.stop().await;
    }

    /// stop() must clear device state.
    #[tokio::test]
    async fn stop_clears_devices() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        node.start().await;
        let _ = event_rx.recv().await; // Started

        // Set local online
        node.set_local_online("100.64.0.1", None).await;

        node.stop().await;

        // After stop, local device should be offline
        let device = node.local_device().await;
        assert_eq!(device.status, crate::types::DeviceStatus::Offline,
            "stop() must set local device offline");

        // Devices list should be empty
        let devices = node.devices().await;
        assert!(devices.is_empty(), "stop() must clear remote devices");
    }

    /// stop() must reset auth state.
    #[tokio::test]
    async fn stop_resets_auth_state() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        node.start().await;
        let _ = event_rx.recv().await;

        // Set some auth state
        node.set_auth_required("https://login.tailscale.com/test").await;
        assert_eq!(node.auth_status().await, AuthStatus::Required);

        node.stop().await;

        assert_eq!(node.auth_status().await, AuthStatus::Unknown,
            "stop() must reset auth status to Unknown");
        assert!(node.auth_url().await.is_none(),
            "stop() must clear auth URL");
    }

    /// Double stop should not panic.
    #[tokio::test]
    async fn double_stop_safe() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        node.start().await;
        let _ = event_rx.recv().await;

        node.stop().await;
        node.stop().await; // Should not panic
    }

    /// Start without prior stop should be idempotent (already running).
    #[tokio::test]
    async fn double_start_idempotent() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        node.start().await;
        let _ = event_rx.recv().await;

        // Should not panic or double-start
        node.start().await;
        assert!(node.is_running().await);

        node.stop().await;
    }

    // ── Auth state tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn set_auth_required_and_authenticated() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);
        node.start().await;
        let _ = event_rx.recv().await;

        // Set auth required
        node.set_auth_required("https://login.tailscale.com/abc").await;
        assert_eq!(node.auth_status().await, AuthStatus::Required);
        assert_eq!(node.auth_url().await, Some("https://login.tailscale.com/abc".to_string()));

        // Consume AuthRequired event
        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::AuthRequired(_)));

        // Set authenticated
        node.set_auth_authenticated().await;
        assert_eq!(node.auth_status().await, AuthStatus::Authenticated);
        assert!(node.auth_url().await.is_none());

        // Consume AuthComplete event
        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::AuthComplete));

        // Second call should be idempotent (no extra events)
        node.set_auth_authenticated().await;
        // No additional event should be emitted - verify with timeout
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            event_rx.recv(),
        ).await;
        assert!(result.is_err(), "set_auth_authenticated should be idempotent");

        node.stop().await;
    }

    // ── Event subscription tests ──────────────────────────────────────────

    #[tokio::test]
    async fn subscribe_events_receives_events() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, _event_rx) = MeshNode::new(test_config(), conn_mgr);

        // Additional subscriber
        let mut sub2 = node.subscribe_events();

        node.start().await;

        let event = sub2.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::Started));

        node.stop().await;
    }

    // ── CS-9: current_timestamp_ms() ──────────────────────────────────────

    #[test]
    fn current_timestamp_ms_returns_reasonable_value() {
        let ts = current_timestamp_ms();
        // Should be after 2020-01-01 and before 2100-01-01
        assert!(ts > 1577836800000, "Timestamp should be after 2020-01-01");
        assert!(ts < 4102444800000, "Timestamp should be before 2100-01-01");
    }

    // ── Message bus integration ───────────────────────────────────────────

    #[tokio::test]
    async fn message_bus_accessible() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, _event_rx) = MeshNode::new(test_config(), conn_mgr);
        let bus = node.message_bus();

        // Should be able to subscribe
        let ns = bus.subscribed_namespaces().await;
        assert!(ns.is_empty(), "New message bus should have no subscriptions");
    }

    // ══════════════════════════════════════════════════════════════════════
    // Adversarial / edge-case tests
    // ══════════════════════════════════════════════════════════════════════

    /// 24. Start while already running: second start() must be a no-op (no
    ///     panic, no duplicate event loops). Verify only one Started event.
    #[tokio::test]
    async fn start_while_running_is_noop() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        node.start().await;
        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::Started),
            "First start must emit Started");

        // Second start while running
        node.start().await;
        assert!(node.is_running().await);

        // Give a brief window for any spurious events
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should NOT receive a second Started event
        match event_rx.try_recv() {
            Ok(MeshNodeEvent::Started) => {
                panic!("start() while already running must NOT emit a second Started event");
            }
            _ => {} // Expected: no event or a different event
        }

        node.stop().await;
    }

    /// 25. Stop while not started: must not panic.
    #[tokio::test]
    async fn stop_while_not_started_no_panic() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        assert!(!node.is_running().await);

        // Stop on never-started node: should be a silent no-op
        node.stop().await;

        assert!(!node.is_running().await);

        // Should not emit Stopped (was never started)
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(event_rx.try_recv().is_err(),
            "stop() on never-started node must not emit Stopped event");
    }

    /// 26. Subscribe before start: subscriber created before start() should
    ///     receive events emitted after start.
    #[tokio::test]
    async fn subscribe_before_start_receives_events() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, _default_rx) = MeshNode::new(test_config(), conn_mgr);

        // Subscribe BEFORE start
        let mut pre_start_sub = node.subscribe_events();

        node.start().await;

        // The pre-start subscriber should receive Started
        let event = pre_start_sub.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::Started),
            "Pre-start subscriber must receive Started event");

        node.stop().await;

        let event = pre_start_sub.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::Stopped),
            "Pre-start subscriber must receive Stopped event");
    }

    /// 27. Many subscribers: 100 subscribers all receive the same event.
    #[tokio::test]
    async fn many_subscribers_all_receive_events() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, _default_rx) = MeshNode::new(test_config(), conn_mgr);

        // Create 100 subscribers
        let mut subscribers: Vec<broadcast::Receiver<MeshNodeEvent>> = (0..100)
            .map(|_| node.subscribe_events())
            .collect();

        node.start().await;

        // All 100 should receive Started
        for (i, sub) in subscribers.iter_mut().enumerate() {
            let event = tokio::time::timeout(Duration::from_secs(2), sub.recv()).await;
            match event {
                Ok(Ok(MeshNodeEvent::Started)) => {} // expected
                other => panic!(
                    "Subscriber {i} did not receive Started event: {other:?}"
                ),
            }
        }

        node.stop().await;
    }

    /// 28. Event channel capacity: emit many events rapidly (channel capacity
    ///     is 256). Subscriber that falls behind gets Lagged, not a deadlock.
    #[tokio::test]
    async fn event_channel_capacity_no_deadlock() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, _default_rx) = MeshNode::new(test_config(), conn_mgr);

        // Subscribe but DON'T read from it (will fall behind)
        let mut slow_sub = node.subscribe_events();

        node.start().await;

        // Emit 300 events rapidly (channel capacity is 256)
        for i in 0..300 {
            node.emit_event(MeshNodeEvent::Error(format!("test-event-{i}")));
        }

        // Give time for events to propagate
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Now try to read from the slow subscriber.
        // It should get Lagged errors but NOT deadlock.
        let mut lagged = false;
        let mut received_count = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);

        loop {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            match slow_sub.try_recv() {
                Ok(_) => received_count += 1,
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    lagged = true;
                    // After Lagged, we can still receive subsequent events
                    let _ = n;
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }

        // The subscriber should have experienced lag (300 events > 256 capacity)
        assert!(lagged || received_count > 0,
            "Slow subscriber must either receive events or get Lagged, not deadlock. lagged={lagged}, received={received_count}");

        node.stop().await;
    }

    /// emit_event works without start() — events are delivered to existing subscribers.
    #[tokio::test]
    async fn emit_event_without_start() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        // emit_event without start() — should not panic
        node.emit_event(MeshNodeEvent::Error("test error".to_string()));

        let event = event_rx.recv().await.unwrap();
        match event {
            MeshNodeEvent::Error(msg) => assert_eq!(msg, "test error"),
            other => panic!("Expected Error event, got: {other:?}"),
        }
    }

    /// Multiple start/stop cycles: verify each cycle produces correct events.
    #[tokio::test]
    async fn multiple_start_stop_cycles() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        for cycle in 0..3 {
            node.start().await;

            // Drain events until we find Started (stop may emit cleanup events
            // that the event loop forwards before Started arrives)
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let mut found_started = false;
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                    event_rx.recv(),
                ).await {
                    Ok(Ok(MeshNodeEvent::Started)) => { found_started = true; break; }
                    Ok(Ok(_)) => continue, // other events (DevicesChanged, etc.)
                    Ok(Err(_)) => break,
                    Err(_) => break,
                }
            }
            assert!(found_started, "Cycle {cycle}: start must emit Started");
            assert!(node.is_running().await);

            node.stop().await;

            // Drain events until we find Stopped
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let mut found_stopped = false;
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                    event_rx.recv(),
                ).await {
                    Ok(Ok(MeshNodeEvent::Stopped)) => { found_stopped = true; break; }
                    Ok(Ok(_)) => continue,
                    Ok(Err(_)) => break,
                    Err(_) => break,
                }
            }
            assert!(found_stopped, "Cycle {cycle}: stop must emit Stopped");
            assert!(!node.is_running().await);
        }
    }

    /// After stop, local device is reset to offline.
    #[tokio::test]
    async fn stop_fully_resets_state() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        node.start().await;
        let _ = event_rx.recv().await; // Started

        // Set some state
        node.set_local_online("100.64.0.1", Some("app.ts.net")).await;
        node.set_auth_required("https://auth.test").await;

        node.stop().await;
        let _ = event_rx.recv().await; // Stopped (or AuthRequired from set_auth_required)

        // Verify everything is reset
        assert!(!node.is_running().await);
        assert_eq!(node.auth_status().await, AuthStatus::Unknown);
        assert!(node.auth_url().await.is_none());

        let device = node.local_device().await;
        assert_eq!(device.status, crate::types::DeviceStatus::Offline);
    }

    /// send_envelope to unknown device returns false (no connection).
    #[tokio::test]
    async fn send_envelope_to_unknown_device_returns_false() {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let conn_mgr = Arc::new(conn_mgr);

        let (node, mut event_rx) = MeshNode::new(test_config(), conn_mgr);

        node.start().await;
        let _ = event_rx.recv().await; // Started

        let envelope = MeshEnvelope::new("test", "msg", serde_json::json!({}));
        let result = node.send_envelope("nonexistent-device", &envelope).await;
        assert!(!result,
            "send_envelope to unknown device with no connection should return false");

        node.stop().await;
    }
}
