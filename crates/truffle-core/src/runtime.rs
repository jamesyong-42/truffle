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
use crate::bridge::manager::{BridgeConnection, BridgeManager, ChannelHandler};
use crate::bridge::shim::{GoShim, ShimConfig, ShimLifecycleEvent};
use crate::http::proxy::{ProxyTarget, ReverseProxyHandler};
use crate::http::push::{PushApiHandler, PushManager};
use crate::http::pwa::{PwaConfig, PwaHandler};
use crate::http::router::{HttpHandler, HttpRouter, HttpRouterBridgeHandler, RouterError};
use crate::http::static_site::{StaticFile, StaticHandler};
use crate::http::ws_handler::WsUpgradeHandler;
use crate::mesh::election::ElectionTimingConfig;
use crate::mesh::message_bus::MeshMessageBus;
use crate::mesh::node::{IncomingMeshMessage, MeshNode, MeshNodeConfig, MeshNodeEvent, MeshTimingConfig};
use crate::protocol::envelope::MeshEnvelope;
use crate::transport::connection::{ConnectionManager, TransportConfig};
use crate::types::{BaseDevice, DeviceRole, TailnetPeer};

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

    // -- Device --
    /// A new device was discovered in the mesh.
    DeviceDiscovered(BaseDevice),
    /// An existing device's state was updated.
    DeviceUpdated(BaseDevice),
    /// A device went offline.
    DeviceOffline(String),
    /// The full device list changed.
    DevicesChanged(Vec<BaseDevice>),

    // -- Mesh / Election --
    /// This node's role changed.
    RoleChanged { role: DeviceRole, is_primary: bool },
    /// The mesh primary changed (None = no primary).
    PrimaryChanged { primary_id: Option<String> },

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
/// ```ignore
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
    prefer_primary: bool,
    capabilities: Vec<String>,
    features: Vec<String>,
    metadata: Option<HashMap<String, serde_json::Value>>,
    // Timing
    announce_interval: Option<Duration>,
    discovery_timeout: Option<Duration>,
    election_timeout: Option<Duration>,
    primary_loss_grace: Option<Duration>,
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
            prefer_primary: false,
            capabilities: vec![],
            features: vec![],
            metadata: None,
            announce_interval: None,
            discovery_timeout: None,
            election_timeout: None,
            primary_loss_grace: None,
        }
    }

    /// Set the hostname prefix (used in Tailscale hostname: `{prefix}-{type}-{id}`).
    /// **Required** -- build() will fail without this.
    pub fn hostname(mut self, prefix: impl Into<String>) -> Self {
        self.hostname_prefix = Some(prefix.into());
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

    /// Set whether this node should be a primary candidate.
    pub fn primary_candidate(mut self, val: bool) -> Self {
        self.prefer_primary = val;
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

    /// Set the election timeout.
    pub fn election_timeout(mut self, d: Duration) -> Self {
        self.election_timeout = Some(d);
        self
    }

    /// Set the primary loss grace period.
    pub fn primary_loss_grace(mut self, d: Duration) -> Self {
        self.primary_loss_grace = Some(d);
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

        let election_timing = ElectionTimingConfig {
            election_timeout: self.election_timeout
                .unwrap_or_else(|| ElectionTimingConfig::default().election_timeout),
            primary_loss_grace: self.primary_loss_grace
                .unwrap_or_else(|| ElectionTimingConfig::default().primary_loss_grace),
        };

        let mesh_timing = MeshTimingConfig {
            announce_interval: self.announce_interval
                .unwrap_or_else(|| MeshTimingConfig::default().announce_interval),
            discovery_timeout: self.discovery_timeout
                .unwrap_or_else(|| MeshTimingConfig::default().discovery_timeout),
            election: election_timing,
        };

        let mesh_config = MeshNodeConfig {
            device_id,
            device_name,
            device_type,
            hostname_prefix,
            prefer_primary: self.prefer_primary,
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

    /// Check if this node is the primary.
    pub async fn is_primary(&self) -> bool {
        self.mesh_node.is_primary().await
    }

    /// Send a mesh envelope to a specific device.
    pub async fn send_envelope(&self, device_id: &str, envelope: &MeshEnvelope) -> bool {
        self.mesh_node.send_envelope(device_id, envelope).await
    }

    /// Broadcast a mesh envelope to all connected devices.
    pub async fn broadcast_envelope(&self, envelope: &MeshEnvelope) {
        self.mesh_node.broadcast_envelope(envelope).await;
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
    pub async fn dial_peer(&self, target_dns: &str, port: u16) -> Result<(), String> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        // Step 1: Insert BEFORE dial command (prevents race)
        {
            let mut dials = self.bridge_pending_dials.lock().await;
            dials.insert(request_id.clone(), tx);
        }

        // Step 2: Tell Go to dial
        {
            let shim_guard = self.shim.lock().await;
            let shim = shim_guard.as_ref().ok_or("no sidecar running")?;
            if let Err(e) = shim.dial_raw(target_dns.to_string(), port, request_id.clone()).await {
                let mut dials = self.bridge_pending_dials.lock().await;
                dials.remove(&request_id);
                return Err(format!("dial command failed: {e}"));
            }
        } // Release shim lock during network wait

        // Step 3: Await bridge connection with timeout
        let timeout = Duration::from_secs(10);
        let bridge_conn = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(_)) => {
                return Err("dial cancelled (sender dropped)".into());
            }
            Err(_) => {
                let mut dials = self.bridge_pending_dials.lock().await;
                dials.remove(&request_id);
                return Err(format!("dial timed out after {timeout:?}"));
            }
        };

        // Step 4: Upgrade to WebSocket
        self.connection_manager.handle_outgoing(bridge_conn).await;
        Ok(())
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

        // Register the HTTP router as the bridge handler for port 443 incoming
        let router_handler = Arc::new(HttpRouterBridgeHandler::new(http_router));
        bridge_manager.add_handler(443, Direction::Incoming, router_handler);

        // Register outgoing WS handler (outgoing dials still go through
        // the connection manager directly, not the HTTP router)
        let (ws_out_tx, mut ws_out_rx) = mpsc::channel(64);
        bridge_manager.add_handler(443, Direction::Outgoing, Arc::new(ChannelHandler::new(ws_out_tx)));

        // Use the bridge manager's pending_dials Arc directly
        let bridge_pending = bridge_manager.pending_dials().clone();

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
                            // Request peers
                            let sg = shim.lock().await;
                            if let Some(ref s) = *sg {
                                let _ = s.get_peers().await;
                            }
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
        MeshNodeEvent::DeviceDiscovered(d) => TruffleEvent::DeviceDiscovered(d),
        MeshNodeEvent::DeviceUpdated(d) => TruffleEvent::DeviceUpdated(d),
        MeshNodeEvent::DeviceOffline(id) => TruffleEvent::DeviceOffline(id),
        MeshNodeEvent::DevicesChanged(devices) => TruffleEvent::DevicesChanged(devices),
        MeshNodeEvent::RoleChanged { role, is_primary } => TruffleEvent::RoleChanged { role, is_primary },
        MeshNodeEvent::PrimaryChanged(id) => TruffleEvent::PrimaryChanged { primary_id: id },
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
    /// ```ignore
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
    use crate::types::{BaseDevice, DeviceRole, DeviceStatus};

    fn test_runtime_config() -> RuntimeConfig {
        RuntimeConfig {
            mesh: MeshNodeConfig {
                device_id: "test-dev-1".to_string(),
                device_name: "Test Device".to_string(),
                device_type: "desktop".to_string(),
                hostname_prefix: "app".to_string(),
                prefer_primary: false,
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
            role: Some(DeviceRole::Secondary),
            status: DeviceStatus::Online,
            capabilities: vec!["pty".to_string()],
            metadata: None,
            last_seen: Some(1710764400),
            started_at: Some(1710760000),
            os: Some("darwin".to_string()),
            latency_ms: Some(12.5),
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
            .primary_candidate(true)
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
        assert!(!config.mesh.prefer_primary, "default prefer_primary should be false");
    }

    #[test]
    fn builder_primary_candidate_maps_to_prefer_primary() {
        let (runtime, _rx) = TruffleRuntime::builder()
            .hostname("primary-test")
            .primary_candidate(true)
            .build()
            .expect("builder should succeed");

        let config = runtime.config();
        assert!(config.mesh.prefer_primary, "primary_candidate(true) should set prefer_primary");
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
            TruffleEvent::DeviceDiscovered(test_base_device()),
            TruffleEvent::DeviceUpdated(test_base_device()),
            TruffleEvent::DeviceOffline("dev-abc".to_string()),
            TruffleEvent::RoleChanged { role: DeviceRole::Primary, is_primary: true },
            TruffleEvent::PrimaryChanged { primary_id: Some("dev-abc".to_string()) },
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
            TruffleEvent::DeviceDiscovered(test_base_device()),
            TruffleEvent::DeviceUpdated(test_base_device()),
            TruffleEvent::DeviceOffline("dev-abc".to_string()),
            TruffleEvent::RoleChanged { role: DeviceRole::Secondary, is_primary: false },
            TruffleEvent::PrimaryChanged { primary_id: None },
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
        let _discovered = TruffleEvent::DeviceDiscovered(test_base_device());
        let _updated = TruffleEvent::DeviceUpdated(test_base_device());
        let _offline = TruffleEvent::DeviceOffline("some-id".to_string());
        let _role = TruffleEvent::RoleChanged { role: DeviceRole::Primary, is_primary: true };
        let _primary = TruffleEvent::PrimaryChanged { primary_id: Some("x".to_string()) };
        let _primary_none = TruffleEvent::PrimaryChanged { primary_id: None };
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
        let discovered = mesh_event_to_truffle(MeshNodeEvent::DeviceDiscovered(device.clone()));
        assert!(matches!(discovered, TruffleEvent::DeviceDiscovered(d) if d.id == "dev-abc"));

        let updated = mesh_event_to_truffle(MeshNodeEvent::DeviceUpdated(device.clone()));
        assert!(matches!(updated, TruffleEvent::DeviceUpdated(d) if d.id == "dev-abc"));

        let offline = mesh_event_to_truffle(MeshNodeEvent::DeviceOffline("dev-abc".into()));
        assert!(matches!(offline, TruffleEvent::DeviceOffline(id) if id == "dev-abc"));

        let devices_changed = mesh_event_to_truffle(MeshNodeEvent::DevicesChanged(vec![device]));
        assert!(matches!(devices_changed, TruffleEvent::DevicesChanged(devices) if devices.len() == 1));

        let role = mesh_event_to_truffle(MeshNodeEvent::RoleChanged { role: DeviceRole::Primary, is_primary: true });
        assert!(matches!(role, TruffleEvent::RoleChanged { role: DeviceRole::Primary, is_primary: true }));

        let primary = mesh_event_to_truffle(MeshNodeEvent::PrimaryChanged(Some("dev-abc".into())));
        assert!(matches!(primary, TruffleEvent::PrimaryChanged { primary_id: Some(id) } if id == "dev-abc"));

        let primary_none = mesh_event_to_truffle(MeshNodeEvent::PrimaryChanged(None));
        assert!(matches!(primary_none, TruffleEvent::PrimaryChanged { primary_id: None }));

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
        assert!(!runtime.is_primary().await);

        let local = runtime.local_device().await;
        assert_eq!(local.id, "acc-dev");
        assert_eq!(local.device_type, "mobile");

        // devices() returns discovered peers; before start() there are none
        let devices = runtime.devices().await;
        assert_eq!(devices.len(), 0);
    }
}
