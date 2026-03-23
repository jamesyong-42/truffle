//! TruffleRuntime -- owns the full lifecycle of a Truffle mesh node.
//!
//! Wires together BridgeManager, GoShim, ConnectionManager, and MeshNode.
//! NAPI and Tauri consume this as a thin wrapper.
//!
//! ## Unified Event API (RFC 008 Phase 3)
//!
//! `TruffleEvent` is the public event surface. It unifies lifecycle, auth,
//! sidecar, device, mesh, and error events into a single broadcast channel.
//! Internal types (`MeshNodeEvent`, `ShimLifecycleEvent`) remain internal.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, oneshot, Mutex};

use crate::bridge::header::Direction;
use crate::bridge::manager::{BridgeConnection, BridgeHandler, BridgeManager, ChannelHandler};
use crate::bridge::shim::{GoShim, ShimConfig, ShimLifecycleEvent};
use crate::http::proxy::{ProxyTarget, ReverseProxyHandler};
use crate::http::push::{PushApiHandler, PushManager};
use crate::http::pwa::{PwaConfig, PwaHandler};
use crate::http::router::{HttpHandler, HttpRouter, HttpRouterBridgeHandler, RouterError};
use crate::http::static_site::{StaticFile, StaticHandler};
use crate::http::ws_handler::WsUpgradeHandler;
use crate::mesh::message_bus::MeshMessageBus;
use crate::mesh::node::{IncomingMeshMessage, MeshNode, MeshNodeConfig, MeshNodeEvent, MeshTimingConfig};
use crate::protocol::envelope::MeshEnvelope;
use crate::transport::connection::{ConnectionManager, TransportConfig};
use crate::types::{BaseDevice, TailnetPeer};

// ═══════════════════════════════════════════════════════════════════════════
// TruffleEvent -- unified public event enum
// ═══════════════════════════════════════════════════════════════════════════

/// Unified event enum covering all Truffle runtime events.
///
/// This is the **public** event surface. Consumers subscribe to
/// `broadcast::Receiver<TruffleEvent>` and never need to know about
/// internal `MeshNodeEvent` or `ShimLifecycleEvent`.
#[derive(Debug, Clone)]
pub enum TruffleEvent {
    // -- Lifecycle --
    /// The runtime has started (mesh node began processing).
    Started,
    /// The runtime has stopped.
    Stopped,

    // -- Auth --
    /// Tailscale authentication is required; the user must visit the URL.
    AuthRequired { auth_url: String },
    /// Tailscale authentication completed successfully.
    AuthComplete,

    // -- Network --
    /// The node came online with a Tailscale IP and DNS name.
    Online { ip: String, dns_name: String },

    // -- Sidecar --
    /// The Go sidecar process started.
    SidecarStarted,
    /// The Go sidecar process stopped gracefully.
    SidecarStopped,
    /// The Go sidecar crashed.
    SidecarCrashed { exit_code: Option<i32>, stderr_tail: String },
    /// Tailscale state changed (e.g. NeedsLogin, Running, Stopped).
    SidecarStateChanged { state: String },
    /// Node needs admin approval to join the tailnet.
    SidecarNeedsApproval,
    /// Tailscale key is approaching expiry.
    SidecarKeyExpiring { expires_at: String },
    /// Tailscale health subsystem warnings.
    SidecarHealthWarning { warnings: Vec<String> },

    // -- Peer --
    /// A new peer was discovered in the mesh.
    PeerDiscovered(BaseDevice),
    /// An existing peer's state was updated.
    PeerUpdated(BaseDevice),
    /// A peer went offline.
    PeerOffline(String),
    /// The full peer list changed.
    PeersChanged(Vec<BaseDevice>),
    /// A WebSocket transport connection was established with a peer.
    PeerConnected { connection_id: String, peer_dns: Option<String> },
    /// A WebSocket transport connection was closed.
    PeerDisconnected { connection_id: String, reason: String },

    // -- Message --
    /// An application-level message was received from a peer.
    Message(IncomingMeshMessage),

    // -- Error --
    /// A runtime error occurred.
    Error(String),
}

// ═══════════════════════════════════════════════════════════════════════════
// RuntimeConfig
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for the Truffle runtime.
pub struct RuntimeConfig {
    pub mesh: MeshNodeConfig,
    pub transport: TransportConfig,
    pub sidecar_path: Option<PathBuf>,
    pub state_dir: Option<String>,
    pub auth_key: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
// TruffleRuntimeBuilder
// ═══════════════════════════════════════════════════════════════════════════

/// Builder for `TruffleRuntime`. Provides a fluent API for configuration.
///
/// # Example
/// ```text
/// let (runtime, event_rx) = TruffleRuntime::builder()
///     .hostname("my-app")
///     .sidecar_path("/usr/local/bin/truffle-sidecar")
///     .auth_key("tskey-auth-xxx")
///     .ephemeral(true)
///     .build()?;
/// ```
pub struct TruffleRuntimeBuilder {
    hostname_prefix: Option<String>,
    device_id: Option<String>,
    device_name: Option<String>,
    device_type: Option<String>,
    sidecar_path: Option<PathBuf>,
    state_dir: Option<String>,
    auth_key: Option<String>,
    ephemeral: Option<bool>,
    tags: Option<Vec<String>>,
    capabilities: Vec<String>,
    features: Vec<String>,
    metadata: Option<HashMap<String, serde_json::Value>>,
    // Timing
    announce_interval: Option<Duration>,
    discovery_timeout: Option<Duration>,
    // Extra bridge handlers registered by the application layer
    extra_bridge_handlers: Vec<(u16, Direction, Arc<dyn BridgeHandler>)>,
}

impl TruffleRuntimeBuilder {
    /// Create a new builder with defaults. Call `.hostname()` before `.build()`.
    pub fn new() -> Self {
        Self {
            hostname_prefix: None,
            device_id: None,
            device_name: None,
            device_type: None,
            sidecar_path: None,
            state_dir: None,
            auth_key: None,
            ephemeral: None,
            tags: None,
            capabilities: vec![],
            features: vec![],
            metadata: None,
            announce_interval: None,
            discovery_timeout: None,
            extra_bridge_handlers: vec![],
        }
    }

    /// Set the hostname prefix (used in Tailscale hostname: `{prefix}-{type}-{id}`).
    /// **Required** -- build() will fail without this.
    pub fn hostname(mut self, prefix: impl Into<String>) -> Self {
        self.hostname_prefix = Some(prefix.into());
        self
    }

    /// Register an extra bridge handler for a port/direction.
    /// These are added to the BridgeManager before it starts running.
    pub fn bridge_handler(mut self, port: u16, direction: Direction, handler: Arc<dyn BridgeHandler>) -> Self {
        self.extra_bridge_handlers.push((port, direction, handler));
        self
    }

    /// Set the state directory for sidecar persistence.
    pub fn state_dir(mut self, dir: impl Into<String>) -> Self {
        self.state_dir = Some(dir.into());
        self
    }

    /// Set the path to the Go sidecar binary.
    pub fn sidecar_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.sidecar_path = Some(path.into());
        self
    }

    /// Set the Tailscale auth key.
    pub fn auth_key(mut self, key: impl Into<String>) -> Self {
        self.auth_key = Some(key.into());
        self
    }

    /// Set whether the node is ephemeral (cleaned up when offline).
    pub fn ephemeral(mut self, val: bool) -> Self {
        self.ephemeral = Some(val);
        self
    }

    /// Set ACL tags to advertise (e.g. `["tag:truffle"]`).
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Set the device name.
    pub fn device_name(mut self, name: impl Into<String>) -> Self {
        self.device_name = Some(name.into());
        self
    }

    /// Set the device type (e.g. "desktop", "mobile", "server").
    pub fn device_type(mut self, dt: impl Into<String>) -> Self {
        self.device_type = Some(dt.into());
        self
    }

    /// Set the device ID. Auto-generated if not set.
    pub fn device_id(mut self, id: impl Into<String>) -> Self {
        self.device_id = Some(id.into());
        self
    }

    /// Set feature flags.
    pub fn features(mut self, features: Vec<String>) -> Self {
        self.features = features;
        self
    }

    /// Set device capabilities.
    pub fn capabilities(mut self, caps: Vec<String>) -> Self {
        self.capabilities = caps;
        self
    }

    /// Set device metadata.
    pub fn metadata(mut self, meta: HashMap<String, serde_json::Value>) -> Self {
        self.metadata = Some(meta);
        self
    }

    /// Set the announce interval.
    pub fn announce_interval(mut self, d: Duration) -> Self {
        self.announce_interval = Some(d);
        self
    }

    /// Set the discovery timeout.
    pub fn discovery_timeout(mut self, d: Duration) -> Self {
        self.discovery_timeout = Some(d);
        self
    }

    /// Build the `TruffleRuntime`.
    ///
    /// Returns `(TruffleRuntime, broadcast::Receiver<MeshNodeEvent>)` for
    /// backward-compat with code that still reads `MeshNodeEvent`.
    /// New code should use `runtime.subscribe()` for `TruffleEvent` instead.
    pub fn build(self) -> Result<(TruffleRuntime, broadcast::Receiver<MeshNodeEvent>), RuntimeError> {
        let hostname_prefix = self.hostname_prefix
            .ok_or_else(|| RuntimeError::Bootstrap("hostname is required".to_string()))?;

        let device_id = self.device_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let device_name = self.device_name
            .unwrap_or_else(|| format!("{hostname_prefix}-node"));
        let device_type = self.device_type
            .unwrap_or_else(|| "desktop".to_string());

        let mesh_timing = MeshTimingConfig {
            announce_interval: self.announce_interval
                .unwrap_or_else(|| MeshTimingConfig::default().announce_interval),
            discovery_timeout: self.discovery_timeout
                .unwrap_or_else(|| MeshTimingConfig::default().discovery_timeout),
        };

        let mesh_config = MeshNodeConfig {
            device_id,
            device_name,
            device_type,
            hostname_prefix,
            capabilities: self.capabilities,
            metadata: self.metadata,
            timing: mesh_timing,
        };

        let mut transport_config = TransportConfig::default();
        transport_config.local_device_id = Some(mesh_config.device_id.clone());

        let runtime_config = RuntimeConfig {
            mesh: mesh_config,
            transport: transport_config,
            sidecar_path: self.sidecar_path,
            state_dir: self.state_dir,
            auth_key: self.auth_key,
        };

        let (connection_manager, _transport_rx) = ConnectionManager::new(runtime_config.transport.clone());
        let connection_manager = Arc::new(connection_manager);
        let (mesh_node, event_rx) = MeshNode::new(runtime_config.mesh.clone(), connection_manager.clone());

        let (event_tx, _) = broadcast::channel(256);

        let runtime = TruffleRuntime {
            mesh_node: Arc::new(mesh_node),
            connection_manager,
            shim: Arc::new(Mutex::new(None)),
            bridge_pending_dials: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            http_router: Arc::new(Mutex::new(None)),
            push_manager: Arc::new(Mutex::new(None)),
            config: runtime_config,
            ephemeral: self.ephemeral,
            tags: self.tags,
            extra_bridge_handlers: Mutex::new(self.extra_bridge_handlers),
            bridge_manager_pending: Mutex::new(None),
        };

        Ok((runtime, event_rx))
    }
}

impl Default for TruffleRuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// TruffleRuntime
// ═══════════════════════════════════════════════════════════════════════════

/// The full Truffle runtime -- owns sidecar, bridge, and mesh node.
pub struct TruffleRuntime {
    mesh_node: Arc<MeshNode>,
    connection_manager: Arc<ConnectionManager>,
    shim: Arc<Mutex<Option<GoShim>>>,
    bridge_pending_dials: Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>>,
    /// Unified event broadcast channel.
    event_tx: broadcast::Sender<TruffleEvent>,
    /// HTTP router for path-based routing on port 443 (RFC 008 Phase 4).
    http_router: Arc<Mutex<Option<Arc<HttpRouter>>>>,
    /// Push notification manager (RFC 008 Phase 5).
    push_manager: Arc<Mutex<Option<Arc<PushManager>>>>,
    /// Stored config for sidecar bootstrap at start() time.
    config: RuntimeConfig,
    /// Whether the tsnet node is ephemeral.
    ephemeral: Option<bool>,
    /// ACL tags to advertise.
    tags: Option<Vec<String>>,
    /// Extra bridge handlers registered by the application layer.
    extra_bridge_handlers: Mutex<Vec<(u16, Direction, Arc<dyn BridgeHandler>)>>,
    /// Bridge manager's pending_dials Arc (set during start(), used by external DialFns).
    bridge_manager_pending: Mutex<Option<Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>>>>,
}

impl TruffleRuntime {
    /// Create a `TruffleRuntimeBuilder`.
    pub fn builder() -> TruffleRuntimeBuilder {
        TruffleRuntimeBuilder::new()
    }

    /// Create a new runtime (does not start anything).
    ///
    /// **Deprecated**: Use `TruffleRuntime::builder()` instead. This constructor
    /// is kept for backward compatibility with existing code.
    pub fn new(config: &RuntimeConfig) -> (Self, broadcast::Receiver<MeshNodeEvent>) {
        let (connection_manager, _transport_rx) = ConnectionManager::new(config.transport.clone());
        let connection_manager = Arc::new(connection_manager);
        let (mesh_node, event_rx) = MeshNode::new(config.mesh.clone(), connection_manager.clone());

        let (event_tx, _) = broadcast::channel(256);

        let runtime = Self {
            mesh_node: Arc::new(mesh_node),
            connection_manager,
            shim: Arc::new(Mutex::new(None)),
            bridge_pending_dials: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            http_router: Arc::new(Mutex::new(None)),
            push_manager: Arc::new(Mutex::new(None)),
            config: RuntimeConfig {
                mesh: config.mesh.clone(),
                transport: config.transport.clone(),
                sidecar_path: config.sidecar_path.clone(),
                state_dir: config.state_dir.clone(),
                auth_key: config.auth_key.clone(),
            },
            ephemeral: None,
            tags: None,
            extra_bridge_handlers: Mutex::new(vec![]),
            bridge_manager_pending: Mutex::new(None),
        };

        (runtime, event_rx)
    }

    /// Start the runtime: spawn sidecar (if configured), start mesh node.
    ///
    /// When called on a runtime created via `TruffleRuntimeBuilder`, the
    /// stored config is used automatically. Returns a `TruffleEvent` receiver.
    ///
    /// For runtimes created via deprecated `new()`, use `start_legacy()` instead.
    pub async fn start(&self) -> Result<broadcast::Receiver<TruffleEvent>, RuntimeError> {
        if let Some(ref sidecar_path) = self.config.sidecar_path {
            self.bootstrap_sidecar(sidecar_path.clone(), &self.config).await?;
        }

        // Spawn the mesh event forwarder before starting the node
        self.spawn_mesh_event_forwarder();

        self.mesh_node.start().await;
        Ok(self.event_tx.subscribe())
    }

    /// Start the runtime with an explicit config (backward-compat path).
    ///
    /// Used by code that still calls `TruffleRuntime::new()`.
    pub async fn start_legacy(&self, config: &RuntimeConfig) -> Result<(), RuntimeError> {
        if let Some(ref sidecar_path) = config.sidecar_path {
            self.bootstrap_sidecar(sidecar_path.clone(), config).await?;
        }

        // Spawn the mesh event forwarder before starting the node
        self.spawn_mesh_event_forwarder();

        self.mesh_node.start().await;
        Ok(())
    }

    /// Subscribe to the unified event stream. Returns a new receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<TruffleEvent> {
        self.event_tx.subscribe()
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

    // ── Convenience accessors ──────────────────────────────────────────

    /// Access the stored runtime config.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Access the mesh node (internal).
    pub fn mesh_node(&self) -> &Arc<MeshNode> {
        &self.mesh_node
    }

    /// Access the connection manager.
    pub fn connection_manager(&self) -> &Arc<ConnectionManager> {
        &self.connection_manager
    }

    /// Access the sidecar shim (for bridge dialing).
    pub fn shim(&self) -> &Arc<Mutex<Option<GoShim>>> {
        &self.shim
    }

    /// Access the pending bridge dials map (for bridge dialing).
    /// After start(), returns the BridgeManager's Arc (shared with the bridge accept loop).
    /// Before start(), returns the runtime's own Arc (which won't receive bridge connections).
    pub async fn pending_dials(&self) -> Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>> {
        let guard = self.bridge_manager_pending.lock().await;
        if let Some(ref arc) = *guard {
            arc.clone()
        } else {
            self.bridge_pending_dials.clone()
        }
    }

    /// Access the message bus for namespace-based pub/sub.
    pub fn bus(&self) -> Arc<MeshMessageBus> {
        self.mesh_node.message_bus()
    }

    /// Get the local device ID.
    pub async fn device_id(&self) -> String {
        self.mesh_node.device_id().await
    }

    /// Get the local device info.
    pub async fn local_device(&self) -> BaseDevice {
        self.mesh_node.local_device().await
    }

    /// Get all known devices in the mesh.
    pub async fn devices(&self) -> Vec<BaseDevice> {
        self.mesh_node.devices().await
    }

    /// Send a mesh envelope to a specific device.
    pub async fn send_envelope(&self, device_id: &str, envelope: &MeshEnvelope) -> bool {
        self.mesh_node.send_envelope(device_id, envelope).await
    }

    /// Broadcast a mesh envelope to all connected devices.
    pub async fn broadcast_envelope(&self, envelope: &MeshEnvelope) {
        self.mesh_node.broadcast_envelope(envelope).await;
    }

    /// Get a snapshot of all WebSocket connections (for diagnostics).
    pub async fn connections(&self) -> Vec<crate::transport::connection::WSConnection> {
        self.mesh_node.connections().await
    }

    /// Get a handle to the HTTP router for managing routes.
    ///
    /// Returns `None` if the sidecar hasn't been bootstrapped yet
    /// (the router is created during `bootstrap_sidecar`).
    pub async fn http(&self) -> Option<HttpHandle> {
        let guard = self.http_router.lock().await;
        guard.as_ref().map(|router| HttpHandle {
            router: router.clone(),
            push_state: self.push_manager.clone(),
        })
    }

    /// Get a reference to the push manager, if PWA/push has been enabled.
    ///
    /// Returns `None` if `enable_pwa()` hasn't been called yet.
    pub async fn push(&self) -> Option<Arc<PushManager>> {
        let guard = self.push_manager.lock().await;
        guard.clone()
    }

    // ── Event forwarding ───────────────────────────────────────────────

    /// Spawns a task that maps `MeshNodeEvent` to `TruffleEvent` and
    /// emits on the unified channel.
    fn spawn_mesh_event_forwarder(&self) {
        let mut rx = self.mesh_node.subscribe_events();
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(mesh_event) => {
                        let truffle_event = mesh_event_to_truffle(mesh_event);
                        let _ = tx.send(truffle_event);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Mesh event forwarder lagged by {n}");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // ── Dial / Sidecar ─────────────────────────────────────────────────

    /// Dial a peer through the full bridge pipeline.
    ///
    /// Uses the BridgeManager's pending_dials map (when available) so that
    /// outgoing bridge connections are correctly routed back to this dial.
    pub async fn dial_peer(&self, target_dns: &str, port: u16) -> Result<(), String> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        // Use the bridge manager's pending_dials (Map B) when available,
        // falling back to the runtime's own map (Map A) before start().
        let pending = self.pending_dials().await;

        // Step 1: Insert BEFORE dial command (prevents race)
        {
            let mut dials = pending.lock().await;
            dials.insert(request_id.clone(), tx);
        }

        // Step 2: Tell Go to dial (release shim lock before any further awaits)
        let dial_result = {
            let shim_guard = self.shim.lock().await;
            match shim_guard.as_ref() {
                Some(shim) => Ok(shim.dial_raw(target_dns.to_string(), port, request_id.clone()).await),
                None => Err("no sidecar running"),
            }
        }; // shim lock released HERE
        match dial_result {
            Err(msg) => {
                pending.lock().await.remove(&request_id);
                return Err(msg.to_string());
            }
            Ok(Err(e)) => {
                pending.lock().await.remove(&request_id);
                return Err(format!("dial command failed: {e}"));
            }
            Ok(Ok(())) => {}
        }

        // Step 3: Await bridge connection with timeout
        let timeout = Duration::from_secs(10);
        let bridge_conn = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(_)) => {
                return Err("dial cancelled (sender dropped)".into());
            }
            Err(_) => {
                let mut dials = pending.lock().await;
                dials.remove(&request_id);
                return Err(format!("dial timed out after {timeout:?}"));
            }
        };

        // Step 4: Upgrade to WebSocket
        self.connection_manager.handle_outgoing(bridge_conn).await;
        Ok(())
    }

    /// Ensure a mesh WebSocket connection exists to the given device.
    ///
    /// If no connection exists, looks up the device's DNS name from the device
    /// manager and dials via the sidecar on port 9417 (plain TCP mesh transport).
    /// Returns `Ok(())` when a connection is confirmed, or `Err` if the device
    /// is unknown, has no DNS name, or the dial fails.
    pub async fn ensure_connected(&self, device_id: &str) -> Result<(), String> {
        // Check if we already have a connection
        if self.connection_manager.get_connection_by_device(device_id).await.is_some() {
            return Ok(());
        }

        // Look up the device to get its DNS name
        let device = self.mesh_node.device_by_id(device_id).await
            .ok_or_else(|| format!("device not found: {device_id}"))?;

        let dns_name = device.tailscale_dns_name
            .ok_or_else(|| format!("device {device_id} has no DNS name"))?;

        tracing::info!(
            device_id = %device_id,
            dns_name = %dns_name,
            "No mesh connection to device, dialing..."
        );

        // Dial via port 9417 (plain TCP, avoids double-TLS issue with port 443)
        self.dial_peer(&dns_name, 9417).await
    }

    async fn bootstrap_sidecar(
        &self,
        sidecar_path: PathBuf,
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

        // Create the HTTP router for port 443 incoming connections
        let http_router = Arc::new(HttpRouter::new());

        // Register the /ws WebSocket handler on the router
        let ws_handler = Arc::new(WsUpgradeHandler::new(self.connection_manager.clone()));
        http_router
            .add_route("/ws", "WebSocket", "websocket", ws_handler)
            .await
            .map_err(|e| RuntimeError::Bootstrap(format!("ws route: {e}")))?;

        // Store the router for runtime use
        {
            let mut guard = self.http_router.lock().await;
            *guard = Some(http_router.clone());
        }

        // Register the HTTP router as the bridge handler for port 443 incoming (TLS)
        let router_handler = Arc::new(HttpRouterBridgeHandler::new(http_router.clone()));
        bridge_manager.add_handler(443, Direction::Incoming, router_handler);

        // Register port 9417 incoming (plain TCP) with the same HTTP router
        let router_handler_9417 = Arc::new(HttpRouterBridgeHandler::new(http_router));
        bridge_manager.add_handler(9417, Direction::Incoming, router_handler_9417);

        // Register outgoing WS handler for both ports
        let (ws_out_tx, mut ws_out_rx) = mpsc::channel(64);
        bridge_manager.add_handler(443, Direction::Outgoing, Arc::new(ChannelHandler::new(ws_out_tx.clone())));
        bridge_manager.add_handler(9417, Direction::Outgoing, Arc::new(ChannelHandler::new(ws_out_tx)));

        // Register any extra bridge handlers from the builder
        {
            let mut handlers = self.extra_bridge_handlers.lock().await;
            for (port, direction, handler) in handlers.drain(..) {
                bridge_manager.add_handler(port, direction, handler);
            }
        }

        // Get the bridge manager's pending_dials Arc.
        // This is the authoritative map — the BridgeManager routes outgoing
        // connections here. Store it for external DialFns (file transfer).
        let bridge_pending = bridge_manager.pending_dials().clone();
        *self.bridge_manager_pending.lock().await = Some(bridge_pending.clone());

        // Spawn bridge
        tokio::spawn(async move {
            bridge_manager.run(tokio_util::sync::CancellationToken::new()).await;
        });

        // Spawn outgoing WS connection handler
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
            binary_path: sidecar_path,
            hostname,
            state_dir,
            auth_key: config.auth_key.clone(),
            bridge_port,
            session_token: session_token_hex,
            auto_restart: true,
            ephemeral: self.ephemeral,
            tags: self.tags.clone(),
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
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ShimLifecycleEvent::Started) => {
                        let _ = event_tx.send(TruffleEvent::SidecarStarted);
                    }
                    Ok(ShimLifecycleEvent::Status(status)) => {
                        let _ = event_tx.send(TruffleEvent::SidecarStateChanged {
                            state: status.state.clone(),
                        });
                        if !status.tailscale_ip.is_empty() {
                            let dns_name = status.dns_name.clone();
                            let dns = if dns_name.is_empty() { None } else { Some(dns_name.as_str()) };
                            node.set_local_online(&status.tailscale_ip, dns).await;
                            node.set_auth_authenticated().await;
                            // Emit Online event
                            let _ = event_tx.send(TruffleEvent::Online {
                                ip: status.tailscale_ip.clone(),
                                dns_name,
                            });
                            // Request peers (sidecar auto-listens on 443 TLS + 9417 TCP)
                            let sg = shim.lock().await;
                            if let Some(ref s) = *sg {
                                let _ = s.get_peers().await;
                            }
                            drop(sg);
                            // Start periodic peer polling (every 30s)
                            let poll_shim = shim.clone();
                            tokio::spawn(async move {
                                let mut interval = tokio::time::interval(Duration::from_secs(30));
                                interval.tick().await; // skip first immediate tick
                                loop {
                                    interval.tick().await;
                                    let sg = poll_shim.lock().await;
                                    if let Some(ref s) = *sg {
                                        let _ = s.get_peers().await;
                                    }
                                }
                            });
                        }
                    }
                    Ok(ShimLifecycleEvent::AuthRequired { auth_url }) => {
                        node.set_auth_required(&auth_url).await;
                        // Note: set_auth_required emits MeshNodeEvent::AuthRequired,
                        // which the mesh forwarder will convert to TruffleEvent::AuthRequired.
                    }
                    Ok(ShimLifecycleEvent::NeedsApproval) => {
                        let _ = event_tx.send(TruffleEvent::SidecarNeedsApproval);
                        node.emit_event(MeshNodeEvent::Error(
                            "Node needs admin approval to join tailnet".to_string(),
                        ));
                    }
                    Ok(ShimLifecycleEvent::StateChanged { state, .. }) => {
                        let _ = event_tx.send(TruffleEvent::SidecarStateChanged { state });
                    }
                    Ok(ShimLifecycleEvent::KeyExpiring { expires_at }) => {
                        let _ = event_tx.send(TruffleEvent::SidecarKeyExpiring { expires_at });
                    }
                    Ok(ShimLifecycleEvent::HealthWarning { warnings }) => {
                        let _ = event_tx.send(TruffleEvent::SidecarHealthWarning { warnings });
                    }
                    Ok(ShimLifecycleEvent::Peers(peers_data)) => {
                        let core_peers: Vec<TailnetPeer> = peers_data.peers.iter()
                            .map(|p| p.to_canonical())
                            .collect();
                        node.handle_tailnet_peers(&core_peers).await;

                        // Dial online truffle peers via port 9417 (plain TCP).
                        // Port 443 uses ListenTLS which causes double-TLS on dial.
                        let local_dns = node.local_device().await
                            .tailscale_dns_name.unwrap_or_default();
                        for peer in &peers_data.peers {
                            if !peer.online || peer.dns_name.is_empty() { continue; }
                            if peer.dns_name == local_dns { continue; }
                            if !peer.hostname.contains(&hostname_prefix) { continue; }

                            // Skip if we already have a connection to this peer
                            if conn_mgr.has_connection_for_dns(&peer.dns_name).await {
                                continue;
                            }

                            let dns = peer.dns_name.clone();
                            let bp = bridge_pending.clone();
                            let sh = shim.clone();
                            let cm = conn_mgr.clone();
                            tokio::spawn(async move {
                                // Prepare the dial under the shim lock, then release
                                // BEFORE any network wait to avoid blocking other dialers.
                                let rid = uuid::Uuid::new_v4().to_string();
                                let (tx, rx) = oneshot::channel();
                                bp.lock().await.insert(rid.clone(), tx);

                                let dial_ok = {
                                    let sg = sh.lock().await;
                                    if let Some(ref s) = *sg {
                                        s.dial_raw(dns.clone(), 9417, rid.clone()).await.is_ok()
                                    } else {
                                        false
                                    }
                                }; // shim lock released HERE

                                if dial_ok {
                                    match tokio::time::timeout(Duration::from_secs(10), rx).await {
                                        Ok(Ok(bc)) => {
                                            tracing::info!("Mesh connection established to {dns}");
                                            cm.handle_outgoing(bc).await;
                                        }
                                        _ => { bp.lock().await.remove(&rid); }
                                    }
                                } else {
                                    tracing::warn!("dial_raw command failed for {dns}");
                                    bp.lock().await.remove(&rid);
                                }
                            });
                        }
                    }
                    Ok(ShimLifecycleEvent::DialFailed { request_id, error }) => {
                        tracing::warn!("Dial failed: {request_id}: {error}");
                        bridge_pending.lock().await.remove(&request_id);
                    }
                    Ok(ShimLifecycleEvent::Crashed { exit_code, stderr_tail }) => {
                        let _ = event_tx.send(TruffleEvent::SidecarCrashed {
                            exit_code,
                            stderr_tail: stderr_tail.chars().take(200).collect(),
                        });
                        node.emit_event(MeshNodeEvent::Error(format!(
                            "Sidecar crashed (exit={exit_code:?}): {}",
                            stderr_tail.chars().take(200).collect::<String>()
                        )));
                    }
                    Ok(ShimLifecycleEvent::Stopped) => {
                        let _ = event_tx.send(TruffleEvent::SidecarStopped);
                    }
                    // Phase 5: New sidecar events — forwarded via lifecycle broadcast.
                    // These are consumed by higher-level code (e.g. CLI daemon) via
                    // GoShim::subscribe(), not by the runtime's mesh forwarder.
                    Ok(ShimLifecycleEvent::Listening { port }) => {
                        tracing::info!("Dynamic listener started on port {port}");
                    }
                    Ok(ShimLifecycleEvent::Unlistened { port }) => {
                        tracing::info!("Dynamic listener stopped on port {port}");
                    }
                    Ok(ShimLifecycleEvent::PingResult(data)) => {
                        if data.error.is_empty() {
                            tracing::debug!(
                                "Ping result: {} latency={:.1}ms direct={}",
                                data.target, data.latency_ms, data.direct
                            );
                        } else {
                            tracing::debug!("Ping failed for {}: {}", data.target, data.error);
                        }
                    }
                    Ok(ShimLifecycleEvent::PushFileResult(data)) => {
                        if data.success {
                            tracing::debug!("File push succeeded");
                        } else {
                            tracing::debug!("File push failed: {}", data.error);
                        }
                    }
                    Ok(ShimLifecycleEvent::WaitingFilesResult(data)) => {
                        tracing::debug!("Waiting files: {} file(s)", data.files.len());
                    }
                    Ok(ShimLifecycleEvent::GetWaitingFileResult(data)) => {
                        if data.success {
                            tracing::debug!("Got waiting file: {}", data.file_name);
                        } else {
                            tracing::debug!("Get waiting file failed: {}", data.error);
                        }
                    }
                    Ok(ShimLifecycleEvent::DeleteWaitingFileResult(data)) => {
                        if data.success {
                            tracing::debug!("Deleted waiting file: {}", data.file_name);
                        } else {
                            tracing::debug!("Delete waiting file failed: {}", data.error);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Lifecycle receiver lagged by {n}");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Conversion helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Map a `MeshNodeEvent` to a `TruffleEvent`.
pub fn mesh_event_to_truffle(event: MeshNodeEvent) -> TruffleEvent {
    match event {
        MeshNodeEvent::Started => TruffleEvent::Started,
        MeshNodeEvent::Stopped => TruffleEvent::Stopped,
        MeshNodeEvent::AuthRequired(url) => TruffleEvent::AuthRequired { auth_url: url },
        MeshNodeEvent::AuthComplete => TruffleEvent::AuthComplete,
        MeshNodeEvent::PeerDiscovered(d) => TruffleEvent::PeerDiscovered(d),
        MeshNodeEvent::PeerUpdated(d) => TruffleEvent::PeerUpdated(d),
        MeshNodeEvent::PeerOffline(id) => TruffleEvent::PeerOffline(id),
        MeshNodeEvent::PeersChanged(devices) => TruffleEvent::PeersChanged(devices),
        MeshNodeEvent::PeerConnected { connection_id, peer_dns } =>
            TruffleEvent::PeerConnected { connection_id, peer_dns },
        MeshNodeEvent::PeerDisconnected { connection_id, reason } =>
            TruffleEvent::PeerDisconnected { connection_id, reason },
        MeshNodeEvent::Message(msg) => TruffleEvent::Message(msg),
        MeshNodeEvent::Error(err) => TruffleEvent::Error(err),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// HttpHandle -- convenience API for the HTTP router
// ═══════════════════════════════════════════════════════════════════════════

/// Handle for managing HTTP routes at runtime.
///
/// Obtained via `runtime.http().await`. Provides a convenient API for
/// registering reverse proxies, static file handlers, SPA routes,
/// PWA support, and push notification endpoints.
pub struct HttpHandle {
    router: Arc<HttpRouter>,
    /// Shared push manager state -- written by `enable_pwa`, read by `push_manager()`.
    push_state: Arc<Mutex<Option<Arc<PushManager>>>>,
}

impl HttpHandle {
    /// Register a reverse proxy route.
    ///
    /// Requests matching `prefix` are forwarded to `target_addr` (e.g., "127.0.0.1:3000").
    /// The prefix is stripped before forwarding by default.
    pub async fn proxy(
        &self,
        prefix: &str,
        target_addr: &str,
    ) -> Result<(), RouterError> {
        let target = ProxyTarget::http(target_addr);
        let handler: Arc<dyn HttpHandler> = Arc::new(ReverseProxyHandler::new(prefix, target));
        self.router
            .add_route(prefix, &format!("proxy -> {target_addr}"), "proxy", handler)
            .await
    }

    /// Register a static file serving route from disk.
    pub async fn serve_static(
        &self,
        prefix: &str,
        root_dir: impl Into<PathBuf>,
    ) -> Result<(), RouterError> {
        let handler: Arc<dyn HttpHandler> = Arc::new(StaticHandler::from_dir(root_dir));
        self.router
            .add_route(prefix, "static files", "static", handler)
            .await
    }

    /// Register a SPA (Single Page Application) handler.
    ///
    /// Serves static files from `root_dir`, falling back to `index.html`
    /// for unknown paths (client-side routing).
    pub async fn serve_spa(
        &self,
        prefix: &str,
        root_dir: impl Into<PathBuf>,
    ) -> Result<(), RouterError> {
        let handler: Arc<dyn HttpHandler> = Arc::new(
            StaticHandler::from_dir(root_dir).with_spa_fallback(true),
        );
        self.router
            .add_route(prefix, "SPA", "spa", handler)
            .await
    }

    /// Register a static file serving route from in-memory files.
    pub async fn serve_memory(
        &self,
        prefix: &str,
        files: Vec<StaticFile>,
    ) -> Result<(), RouterError> {
        let handler: Arc<dyn HttpHandler> = Arc::new(StaticHandler::from_memory(files));
        self.router
            .add_route(prefix, "static (memory)", "static-memory", handler)
            .await
    }

    /// Register a custom HTTP handler.
    pub async fn add_route(
        &self,
        prefix: &str,
        name: &str,
        handler: Arc<dyn HttpHandler>,
    ) -> Result<(), RouterError> {
        self.router
            .add_route(prefix, name, "custom", handler)
            .await
    }

    /// Remove a route by prefix.
    pub async fn remove_route(&self, prefix: &str) -> Result<(), RouterError> {
        self.router.remove_route(prefix).await
    }

    /// List all registered routes.
    pub async fn routes(&self) -> Vec<crate::http::router::Route> {
        self.router.list_routes().await
    }

    /// Enable PWA support by registering a PWA handler at the given prefix.
    ///
    /// Serves the PWA manifest, service worker, and icons. Optionally sets up
    /// push notification endpoints if a VAPID key path is provided.
    ///
    /// # Example
    /// ```text
    /// let http = runtime.http().await.unwrap();
    /// http.enable_pwa(PwaConfig {
    ///     name: "My App".to_string(),
    ///     short_name: "MyApp".to_string(),
    ///     ..PwaConfig::default()
    /// }, "/pwa", None).await?;
    /// ```
    pub async fn enable_pwa(
        &self,
        config: PwaConfig,
        prefix: &str,
        vapid_key_path: Option<PathBuf>,
    ) -> Result<Option<Arc<PushManager>>, RouterError> {
        // Register the PWA handler
        let pwa_handler: Arc<dyn HttpHandler> = Arc::new(PwaHandler::new(&config, prefix));
        self.router
            .add_route(prefix, "PWA", "pwa", pwa_handler)
            .await?;

        // Optionally set up push notifications
        if let Some(key_path) = vapid_key_path {
            let push_manager = PushManager::load_or_create(&key_path)
                .await
                .map_err(|e| RouterError::InvalidPrefix(format!("VAPID key error: {e}")))?;
            let push_manager = Arc::new(push_manager);

            // Register push API endpoints
            let push_handler: Arc<dyn HttpHandler> =
                Arc::new(PushApiHandler::new(push_manager.clone(), "/api/push"));
            self.router
                .add_route("/api/push", "Push API", "push-api", push_handler)
                .await?;

            // Store in shared state so runtime.push() works
            {
                let mut guard = self.push_state.lock().await;
                *guard = Some(push_manager.clone());
            }

            return Ok(Some(push_manager));
        }

        Ok(None)
    }

    /// Get a reference to the push manager, if push has been enabled.
    pub async fn push_manager(&self) -> Option<Arc<PushManager>> {
        let guard = self.push_state.lock().await;
        guard.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("bootstrap failed: {0}")]
    Bootstrap(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::node::IncomingMeshMessage;
    use crate::types::{BaseDevice, DeviceStatus};

    fn test_runtime_config() -> RuntimeConfig {
        RuntimeConfig {
            mesh: MeshNodeConfig {
                device_id: "test-dev-1".to_string(),
                device_name: "Test Device".to_string(),
                device_type: "desktop".to_string(),
                hostname_prefix: "app".to_string(),
                capabilities: vec![],
                metadata: None,
                timing: crate::mesh::node::MeshTimingConfig::default(),
            },
            transport: TransportConfig::default(),
            sidecar_path: None,
            state_dir: None,
            auth_key: None,
        }
    }

    fn test_base_device() -> BaseDevice {
        BaseDevice {
            id: "dev-abc".to_string(),
            device_type: "desktop".to_string(),
            name: "Test Peer".to_string(),
            tailscale_hostname: "app-desktop-dev-abc".to_string(),
            tailscale_dns_name: Some("app-desktop-dev-abc.tail1234.ts.net".to_string()),
            tailscale_ip: Some("100.64.0.2".to_string()),
            status: DeviceStatus::Online,
            capabilities: vec!["pty".to_string()],
            metadata: None,
            last_seen: Some(1710764400),
            started_at: Some(1710760000),
            os: Some("darwin".to_string()),
            latency_ms: Some(12.5),
            cur_addr: None,
            relay: None,
        }
    }

    fn test_incoming_message() -> IncomingMeshMessage {
        IncomingMeshMessage {
            from: Some("dev-abc".to_string()),
            connection_id: "conn-1".to_string(),
            namespace: "chat".to_string(),
            msg_type: "text".to_string(),
            payload: serde_json::json!({"text": "hello"}),
        }
    }

    #[tokio::test]
    async fn runtime_creates_and_starts() {
        let config = test_runtime_config();
        let (runtime, mut rx) = TruffleRuntime::new(&config);
        runtime.start_legacy(&config).await.unwrap();

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::Started));

        runtime.stop().await;
    }

    #[tokio::test]
    async fn runtime_stop_without_start() {
        let config = test_runtime_config();
        let (runtime, _rx) = TruffleRuntime::new(&config);
        runtime.stop().await; // Should not panic
    }

    #[tokio::test]
    async fn runtime_mesh_node_accessible() {
        let config = test_runtime_config();
        let (runtime, _rx) = TruffleRuntime::new(&config);
        let device_id = runtime.mesh_node().device_id().await;
        assert_eq!(device_id, "test-dev-1");
    }

    // ═══════════════════════════════════════════════════════════════════
    // RFC 008 Phase 3: TruffleRuntimeBuilder tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn builder_requires_hostname() {
        // build() without hostname must return Err
        let result = TruffleRuntime::builder().build();
        assert!(result.is_err(), "builder without hostname should fail");
    }

    #[test]
    fn builder_with_hostname_succeeds() {
        // Minimal config: just hostname should work
        let result = TruffleRuntime::builder()
            .hostname("my-app")
            .build();
        assert!(result.is_ok(), "builder with hostname should succeed");
    }

    #[test]
    fn builder_all_options() {
        // Every option set -- should not panic
        let result = TruffleRuntime::builder()
            .hostname("my-app")
            .state_dir("/tmp/truffle-test-state")
            .sidecar_path("/usr/local/bin/truffle-sidecar")
            .auth_key("tskey-auth-xxx")
            .ephemeral(false)
            .device_name("My Laptop")
            .device_type("server")
            .features(vec!["pty".to_string(), "file-transfer".to_string()])
            .build();
        assert!(result.is_ok(), "builder with all options should succeed");
    }

    #[test]
    fn builder_defaults() {
        // When only hostname is set, verify sane defaults
        let (runtime, _rx) = TruffleRuntime::builder()
            .hostname("defaults-test")
            .build()
            .expect("minimal builder should succeed");

        let config = runtime.config();
        assert_eq!(config.mesh.device_type, "desktop", "default device_type should be 'desktop'");
    }

    // ═══════════════════════════════════════════════════════════════════
    // RFC 008 Phase 3: TruffleEvent tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn truffle_event_debug_format() {
        // Debug on each variant should not panic
        let events: Vec<TruffleEvent> = vec![
            TruffleEvent::Started,
            TruffleEvent::Stopped,
            TruffleEvent::AuthRequired { auth_url: "https://login.tailscale.com/a/xxx".to_string() },
            TruffleEvent::Online { ip: "100.64.0.1".to_string(), dns_name: "app.tail.ts.net".to_string() },
            TruffleEvent::PeerDiscovered(test_base_device()),
            TruffleEvent::PeerUpdated(test_base_device()),
            TruffleEvent::PeerOffline("dev-abc".to_string()),
            TruffleEvent::PeersChanged(vec![test_base_device()]),
            TruffleEvent::PeerConnected { connection_id: "c1".to_string(), peer_dns: Some("peer.ts.net".to_string()) },
            TruffleEvent::PeerDisconnected { connection_id: "c1".to_string(), reason: "closed".to_string() },
            TruffleEvent::Message(test_incoming_message()),
            TruffleEvent::Error("something went wrong".to_string()),
            TruffleEvent::SidecarCrashed { exit_code: Some(1), stderr_tail: "panic".to_string() },
        ];

        for event in &events {
            let debug = format!("{:?}", event);
            assert!(!debug.is_empty(), "Debug format should produce non-empty output");
        }
    }

    #[test]
    fn truffle_event_clone() {
        // Clone works for all variants
        let events: Vec<TruffleEvent> = vec![
            TruffleEvent::Started,
            TruffleEvent::Stopped,
            TruffleEvent::AuthRequired { auth_url: "https://login.tailscale.com/a/xxx".to_string() },
            TruffleEvent::Online { ip: "100.64.0.1".to_string(), dns_name: "app.tail.ts.net".to_string() },
            TruffleEvent::PeerDiscovered(test_base_device()),
            TruffleEvent::PeerUpdated(test_base_device()),
            TruffleEvent::PeerOffline("dev-abc".to_string()),
            TruffleEvent::PeersChanged(vec![test_base_device()]),
            TruffleEvent::PeerConnected { connection_id: "c1".to_string(), peer_dns: None },
            TruffleEvent::PeerDisconnected { connection_id: "c1".to_string(), reason: "timeout".to_string() },
            TruffleEvent::Message(test_incoming_message()),
            TruffleEvent::Error("err".to_string()),
            TruffleEvent::SidecarCrashed { exit_code: None, stderr_tail: String::new() },
        ];

        for event in &events {
            let cloned = event.clone();
            // Debug representations must match after clone
            assert_eq!(format!("{:?}", event), format!("{:?}", cloned));
        }
    }

    #[test]
    fn truffle_event_all_variants_constructible() {
        // Verify every variant can be created without panic
        let _started = TruffleEvent::Started;
        let _stopped = TruffleEvent::Stopped;
        let _auth = TruffleEvent::AuthRequired { auth_url: "https://example.com".to_string() };
        let _online = TruffleEvent::Online { ip: "100.64.0.1".to_string(), dns_name: "host.ts.net".to_string() };
        let _discovered = TruffleEvent::PeerDiscovered(test_base_device());
        let _updated = TruffleEvent::PeerUpdated(test_base_device());
        let _offline = TruffleEvent::PeerOffline("some-id".to_string());
        let _changed = TruffleEvent::PeersChanged(vec![test_base_device()]);
        let _connected = TruffleEvent::PeerConnected { connection_id: "c1".to_string(), peer_dns: Some("p.ts.net".to_string()) };
        let _disconnected = TruffleEvent::PeerDisconnected { connection_id: "c1".to_string(), reason: "bye".to_string() };
        let _msg = TruffleEvent::Message(test_incoming_message());
        let _err = TruffleEvent::Error("test error".to_string());
        let _crash = TruffleEvent::SidecarCrashed { exit_code: Some(137), stderr_tail: "killed".to_string() };
    }

    // ═══════════════════════════════════════════════════════════════════
    // RFC 008 Phase 3: Runtime lifecycle tests (new API)
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn runtime_subscribe_returns_receiver() {
        // subscribe() works before start
        let (runtime, _rx) = TruffleRuntime::builder()
            .hostname("subscribe-test")
            .build()
            .expect("builder should succeed");

        let rx = runtime.subscribe();
        // Should have a valid receiver (not dropped)
        drop(rx);
    }

    #[tokio::test]
    async fn runtime_stop_without_start_safe() {
        // stop() on a runtime that was never started should not panic
        let (runtime, _rx) = TruffleRuntime::builder()
            .hostname("stop-test")
            .build()
            .expect("builder should succeed");

        runtime.stop().await; // Must not panic
    }

    #[test]
    fn runtime_bus_accessible() {
        // bus() returns a valid handle
        let (runtime, _rx) = TruffleRuntime::builder()
            .hostname("bus-test")
            .build()
            .expect("builder should succeed");

        let bus = runtime.bus();
        // Should return a valid Arc<MeshMessageBus> -- not None or panic
        assert!(Arc::strong_count(&bus) >= 1);
    }

    #[test]
    fn runtime_mesh_node_accessible_via_builder() {
        // mesh_node() returns a valid handle from builder-constructed runtime
        let (runtime, _rx) = TruffleRuntime::builder()
            .hostname("mesh-node-test")
            .build()
            .expect("builder should succeed");

        let node = runtime.mesh_node();
        assert!(Arc::strong_count(node) >= 1);
    }

    // ═══════════════════════════════════════════════════════════════════
    // RFC 008 Phase 3: mesh_event_to_truffle conversion tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn mesh_event_to_truffle_conversion() {
        let started = mesh_event_to_truffle(MeshNodeEvent::Started);
        assert!(matches!(started, TruffleEvent::Started));

        let stopped = mesh_event_to_truffle(MeshNodeEvent::Stopped);
        assert!(matches!(stopped, TruffleEvent::Stopped));

        let auth = mesh_event_to_truffle(MeshNodeEvent::AuthRequired("https://example.com".into()));
        assert!(matches!(auth, TruffleEvent::AuthRequired { auth_url } if auth_url == "https://example.com"));

        let auth_complete = mesh_event_to_truffle(MeshNodeEvent::AuthComplete);
        assert!(matches!(auth_complete, TruffleEvent::AuthComplete));

        let err = mesh_event_to_truffle(MeshNodeEvent::Error("something broke".into()));
        assert!(matches!(err, TruffleEvent::Error(msg) if msg == "something broke"));

        let device = test_base_device();
        let discovered = mesh_event_to_truffle(MeshNodeEvent::PeerDiscovered(device.clone()));
        assert!(matches!(discovered, TruffleEvent::PeerDiscovered(d) if d.id == "dev-abc"));

        let updated = mesh_event_to_truffle(MeshNodeEvent::PeerUpdated(device.clone()));
        assert!(matches!(updated, TruffleEvent::PeerUpdated(d) if d.id == "dev-abc"));

        let offline = mesh_event_to_truffle(MeshNodeEvent::PeerOffline("dev-abc".into()));
        assert!(matches!(offline, TruffleEvent::PeerOffline(id) if id == "dev-abc"));

        let peers_changed = mesh_event_to_truffle(MeshNodeEvent::PeersChanged(vec![device]));
        assert!(matches!(peers_changed, TruffleEvent::PeersChanged(devices) if devices.len() == 1));

        let connected = mesh_event_to_truffle(MeshNodeEvent::PeerConnected {
            connection_id: "c1".into(),
            peer_dns: Some("peer.ts.net".into()),
        });
        assert!(matches!(connected, TruffleEvent::PeerConnected { connection_id, peer_dns }
            if connection_id == "c1" && peer_dns == Some("peer.ts.net".to_string())));

        let disconnected = mesh_event_to_truffle(MeshNodeEvent::PeerDisconnected {
            connection_id: "c2".into(),
            reason: "timeout".into(),
        });
        assert!(matches!(disconnected, TruffleEvent::PeerDisconnected { connection_id, reason }
            if connection_id == "c2" && reason == "timeout"));

        let msg = mesh_event_to_truffle(MeshNodeEvent::Message(test_incoming_message()));
        assert!(matches!(msg, TruffleEvent::Message(_)));
    }

    #[tokio::test]
    async fn builder_runtime_starts_and_emits_events() {
        let (runtime, _legacy_rx) = TruffleRuntimeBuilder::new()
            .hostname("evt-test")
            .build()
            .unwrap();

        let mut rx = runtime.start().await.unwrap();

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, TruffleEvent::Started));

        runtime.stop().await;
    }

    #[tokio::test]
    async fn builder_subscribe_returns_independent_receiver() {
        let (runtime, _legacy_rx) = TruffleRuntimeBuilder::new()
            .hostname("multi-sub-test")
            .build()
            .unwrap();

        let mut rx1 = runtime.subscribe();
        let mut rx2 = runtime.subscribe();

        runtime.start().await.unwrap();

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert!(matches!(e1, TruffleEvent::Started));
        assert!(matches!(e2, TruffleEvent::Started));

        runtime.stop().await;
    }

    #[tokio::test]
    async fn convenience_accessors_work() {
        let (runtime, _rx) = TruffleRuntimeBuilder::new()
            .hostname("acc-test")
            .device_id("acc-dev")
            .device_name("Accessor Device")
            .device_type("mobile")
            .build()
            .unwrap();

        assert_eq!(runtime.device_id().await, "acc-dev");

        let local = runtime.local_device().await;
        assert_eq!(local.id, "acc-dev");
        assert_eq!(local.device_type, "mobile");

        // devices() returns discovered peers; before start() there are none
        let devices = runtime.devices().await;
        assert_eq!(devices.len(), 0);
    }
}
