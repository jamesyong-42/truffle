use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use tokio::sync::{broadcast, mpsc};

use truffle_core::bridge::header::Direction;
use truffle_core::bridge::manager::{BridgeManager, ChannelHandler};
use truffle_core::bridge::shim::{GoShim, ShimConfig, ShimLifecycleEvent};
use truffle_core::mesh::election::ElectionTimingConfig;
use truffle_core::mesh::message_bus::MeshMessageBus;
use truffle_core::mesh::node::{MeshNode as CoreMeshNode, MeshNodeConfig, MeshNodeEvent, MeshTimingConfig};
use truffle_core::protocol::envelope::MeshEnvelope;
use truffle_core::transport::connection::{ConnectionManager, TransportConfig};
use truffle_core::types::TailnetPeer;

use crate::types::{
    mesh_event_to_napi, napi_peer_to_core, NapiBaseDevice,
    NapiMeshEvent, NapiMeshNodeConfig, NapiTailnetPeer,
};

/// NapiMeshNode - Node.js wrapper for truffle-core MeshNode.
///
/// Manages the full lifecycle: BridgeManager, GoShim sidecar, ConnectionManager,
/// and MeshNode. The sidecar provides Tailscale connectivity; the bridge routes
/// TCP streams; the connection manager upgrades them to WebSocket; and the mesh
/// node handles device discovery, election, and messaging.
#[napi]
pub struct NapiMeshNode {
    inner: Arc<CoreMeshNode>,
    connection_manager: Arc<ConnectionManager>,
    pending_callback: std::sync::Mutex<Option<ThreadsafeFunction<NapiMeshEvent>>>,
    /// Stored config fields needed at start() time for sidecar spawning.
    sidecar_path: Option<String>,
    state_dir: Option<String>,
    auth_key: Option<String>,
    hostname_prefix: String,
    device_id: String,
    device_type: String,
    /// GoShim handle, kept alive for the node's lifetime.
    shim: Arc<tokio::sync::Mutex<Option<GoShim>>>,
}

#[napi]
impl NapiMeshNode {
    /// Create a new MeshNode.
    ///
    /// The `config` parameter matches the TypeScript `MeshNodeConfig` interface.
    #[napi(constructor)]
    pub fn new(config: NapiMeshNodeConfig) -> Result<Self> {
        let timing = config.timing.as_ref();

        let election_timing = ElectionTimingConfig {
            election_timeout: Duration::from_millis(
                timing.and_then(|t| t.election_timeout_ms).unwrap_or(10_000) as u64,
            ),
            primary_loss_grace: Duration::from_millis(
                timing.and_then(|t| t.primary_loss_grace_ms).unwrap_or(15_000) as u64,
            ),
        };

        let mesh_timing = MeshTimingConfig {
            announce_interval: Duration::from_millis(
                timing.and_then(|t| t.announce_interval_ms).unwrap_or(30_000) as u64,
            ),
            discovery_timeout: Duration::from_millis(
                timing.and_then(|t| t.discovery_timeout_ms).unwrap_or(5_000) as u64,
            ),
            election: election_timing,
        };

        let core_config = MeshNodeConfig {
            device_id: config.device_id.clone(),
            device_name: config.device_name,
            device_type: config.device_type.clone(),
            hostname_prefix: config.hostname_prefix.clone(),
            prefer_primary: config.prefer_primary.unwrap_or(false),
            capabilities: config.capabilities.unwrap_or_default(),
            metadata: None,
            timing: mesh_timing,
        };

        let transport_config = TransportConfig::default();
        let (connection_manager, _transport_event_rx) = ConnectionManager::new(transport_config);
        let connection_manager = Arc::new(connection_manager);

        let (inner, _event_rx) = CoreMeshNode::new(core_config, connection_manager.clone());

        Ok(Self {
            inner: Arc::new(inner),
            connection_manager,
            pending_callback: std::sync::Mutex::new(None),
            sidecar_path: config.sidecar_path,
            state_dir: config.state_dir,
            auth_key: config.auth_key,
            hostname_prefix: config.hostname_prefix,
            device_id: config.device_id,
            device_type: config.device_type,
            shim: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    /// Start the mesh node.
    ///
    /// If `sidecarPath` was provided, this spawns the Go sidecar process,
    /// creates a BridgeManager to route connections, and wires everything
    /// together for full Tailscale mesh networking.
    #[napi]
    pub async fn start(&self) -> Result<()> {
        // Spawn the JS event loop if a callback was registered before start
        let callback = self
            .pending_callback
            .lock()
            .map_err(|_| Error::from_reason("pending_callback lock poisoned"))?
            .take();

        if let Some(cb) = callback {
            let rx = self.inner.subscribe_events();
            tokio::spawn(async move {
                Self::event_loop(rx, cb).await;
            });
        }

        // If sidecar_path is provided, wire up the full bridge + sidecar stack
        if let Some(ref sidecar_path) = self.sidecar_path {
            self.start_with_sidecar(sidecar_path.clone()).await?;
        }

        // Start the core mesh node (begins event processing loop)
        self.inner.start().await;
        Ok(())
    }

    /// Wire up BridgeManager + GoShim + ConnectionManager for full connectivity.
    async fn start_with_sidecar(&self, sidecar_path: String) -> Result<()> {
        // 1. Generate a random session token
        let mut session_token = [0u8; 32];
        getrandom::getrandom(&mut session_token)
            .map_err(|e| Error::from_reason(format!("failed to generate session token: {e}")))?;

        let session_token_hex = hex::encode(session_token);

        // 2. Bind the BridgeManager on an ephemeral port
        let mut bridge_manager = BridgeManager::bind(session_token)
            .await
            .map_err(|e| Error::from_reason(format!("failed to bind bridge: {e}")))?;

        let bridge_port = bridge_manager.local_port();
        tracing::info!("Bridge listening on 127.0.0.1:{bridge_port}");

        // 3. Register handlers for incoming WebSocket connections (port 443)
        let (ws_tx, mut ws_rx) = mpsc::channel(64);
        bridge_manager.add_handler(
            443,
            Direction::Incoming,
            Arc::new(ChannelHandler::new(ws_tx)),
        );

        // 4. Spawn the bridge accept loop
        tokio::spawn(async move {
            bridge_manager.run().await;
        });

        // 5. Spawn task to feed incoming bridge connections to the ConnectionManager
        let conn_mgr = self.connection_manager.clone();
        tokio::spawn(async move {
            while let Some(bridge_conn) = ws_rx.recv().await {
                conn_mgr.handle_incoming(bridge_conn).await;
            }
            tracing::info!("Bridge incoming handler loop ended");
        });

        // 6. Build the hostname: prefix-type-id (matches protocol::hostname format)
        let hostname = format!(
            "{}-{}-{}",
            self.hostname_prefix, self.device_type, self.device_id
        );

        // 7. Determine state directory
        let state_dir = self
            .state_dir
            .clone()
            .unwrap_or_else(|| format!(".truffle-state/{}", self.device_id));

        // Ensure state directory exists
        std::fs::create_dir_all(&state_dir).map_err(|e| {
            Error::from_reason(format!("failed to create state dir '{}': {e}", state_dir))
        })?;

        // 8. Spawn the Go sidecar
        let shim_config = ShimConfig {
            binary_path: PathBuf::from(sidecar_path),
            hostname,
            state_dir,
            auth_key: self.auth_key.clone(),
            bridge_port,
            session_token: session_token_hex,
            auto_restart: true,
        };

        let (shim, mut lifecycle_rx) = GoShim::spawn(shim_config)
            .await
            .map_err(|e| Error::from_reason(format!("failed to spawn sidecar: {e}")))?;

        tracing::info!("Go sidecar spawned");

        // 9. Store shim handle so it stays alive
        {
            let mut shim_guard = self.shim.lock().await;
            *shim_guard = Some(shim);
        }

        // 10. Spawn lifecycle event loop: maps sidecar events to mesh node actions
        let inner = self.inner.clone();
        let shim_handle = self.shim.clone();
        tokio::spawn(async move {
            loop {
                match lifecycle_rx.recv().await {
                    Ok(event) => match event {
                        ShimLifecycleEvent::Started => {
                            tracing::info!("Sidecar started");
                        }
                        ShimLifecycleEvent::Status(status) => {
                            tracing::info!(
                                "Sidecar status: state={}, ip={}, dns={}",
                                status.state,
                                status.tailscale_ip,
                                status.dns_name
                            );
                            if !status.tailscale_ip.is_empty() {
                                let dns = if status.dns_name.is_empty() {
                                    None
                                } else {
                                    Some(status.dns_name.as_str())
                                };
                                inner.set_local_online(&status.tailscale_ip, dns).await;
                                inner.set_auth_authenticated().await;

                                // Auto-request peer list now that we're online
                                let shim_guard = shim_handle.lock().await;
                                if let Some(ref shim) = *shim_guard {
                                    tracing::info!("Requesting peer list after auth");
                                    if let Err(e) = shim.get_peers().await {
                                        tracing::warn!("Failed to request peers after auth: {e}");
                                    }
                                }
                            }
                        }
                        ShimLifecycleEvent::AuthRequired { auth_url } => {
                            tracing::info!("Tailscale auth required: {auth_url}");
                            // Delegate to core: sets state + emits event
                            inner.set_auth_required(&auth_url).await;
                        }
                        ShimLifecycleEvent::Peers(peers_data) => {
                            let core_peers: Vec<TailnetPeer> = peers_data
                                .peers
                                .iter()
                                .map(|p| TailnetPeer {
                                    id: p.id.clone(),
                                    hostname: p.hostname.clone(),
                                    dns_name: p.dns_name.clone(),
                                    tailscale_ips: p.tailscale_ips.clone(),
                                    online: p.online,
                                    os: if p.os.is_empty() {
                                        None
                                    } else {
                                        Some(p.os.clone())
                                    },
                                })
                                .collect();
                            inner.handle_tailnet_peers(&core_peers).await;
                        }
                        ShimLifecycleEvent::Crashed {
                            exit_code,
                            stderr_tail,
                        } => {
                            let msg = format!(
                                "Sidecar crashed (exit_code={:?}): {}",
                                exit_code,
                                stderr_tail.chars().take(200).collect::<String>()
                            );
                            tracing::error!("{msg}");
                            inner.emit_event(MeshNodeEvent::Error(msg));
                        }
                        ShimLifecycleEvent::Stopped => {
                            tracing::info!("Sidecar stopped");
                        }
                    },
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Sidecar lifecycle receiver lagged by {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!("Sidecar lifecycle channel closed");
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Stop the mesh node and sidecar.
    #[napi]
    pub async fn stop(&self) -> Result<()> {
        // Stop sidecar first
        let shim = {
            let mut guard = self.shim.lock().await;
            guard.take()
        };
        if let Some(shim) = shim {
            if let Err(e) = shim.stop().await {
                tracing::warn!("Error stopping sidecar: {e}");
            }
        }

        self.inner.stop().await;
        Ok(())
    }

    /// Check if the node is running.
    #[napi]
    pub async fn is_running(&self) -> Result<bool> {
        Ok(self.inner.is_running().await)
    }

    /// Get the local device info.
    #[napi]
    pub async fn local_device(&self) -> Result<NapiBaseDevice> {
        let device = self.inner.local_device().await;
        Ok(NapiBaseDevice::from(device))
    }

    /// Get the local device ID.
    #[napi]
    pub async fn device_id(&self) -> Result<String> {
        Ok(self.inner.device_id().await)
    }

    /// Get all known devices.
    #[napi]
    pub async fn devices(&self) -> Result<Vec<NapiBaseDevice>> {
        let devices = self.inner.devices().await;
        Ok(devices.into_iter().map(NapiBaseDevice::from).collect())
    }

    /// Get a device by ID.
    #[napi]
    pub async fn device_by_id(&self, id: String) -> Result<Option<NapiBaseDevice>> {
        let device = self.inner.device_by_id(&id).await;
        Ok(device.map(NapiBaseDevice::from))
    }

    /// Check if this node is the primary.
    #[napi]
    pub async fn is_primary(&self) -> Result<bool> {
        Ok(self.inner.is_primary().await)
    }

    /// Get the current primary device ID.
    #[napi]
    pub async fn primary_id(&self) -> Result<Option<String>> {
        Ok(self.inner.primary_id().await)
    }

    /// Get the current role as a string ("primary" or "secondary").
    #[napi]
    pub async fn role(&self) -> Result<String> {
        let role = self.inner.role().await;
        Ok(match role {
            truffle_core::types::DeviceRole::Primary => "primary".to_string(),
            truffle_core::types::DeviceRole::Secondary => "secondary".to_string(),
        })
    }

    /// Send a mesh envelope to a specific device.
    /// Returns true if the message was sent successfully.
    #[napi]
    pub async fn send_envelope(
        &self,
        device_id: String,
        namespace: String,
        msg_type: String,
        payload: serde_json::Value,
    ) -> Result<bool> {
        let envelope = MeshEnvelope::new(namespace, msg_type, payload);
        Ok(self.inner.send_envelope(&device_id, &envelope).await)
    }

    /// Broadcast a mesh envelope to all connected devices.
    #[napi]
    pub async fn broadcast_envelope(
        &self,
        namespace: String,
        msg_type: String,
        payload: serde_json::Value,
    ) -> Result<()> {
        let envelope = MeshEnvelope::new(namespace, msg_type, payload);
        self.inner.broadcast_envelope(&envelope).await;
        Ok(())
    }

    /// Get the current auth status ("unknown", "required", "authenticated").
    #[napi]
    pub async fn auth_status(&self) -> Result<String> {
        let status = self.inner.auth_status().await;
        Ok(match status {
            truffle_core::types::AuthStatus::Unknown => "unknown".to_string(),
            truffle_core::types::AuthStatus::Required => "required".to_string(),
            truffle_core::types::AuthStatus::Authenticated => "authenticated".to_string(),
        })
    }

    /// Get the current auth URL, if auth is required.
    #[napi]
    pub async fn auth_url(&self) -> Result<Option<String>> {
        Ok(self.inner.auth_url().await)
    }

    /// Get the message bus for namespace-based pub/sub.
    #[napi]
    pub fn message_bus(&self) -> NapiMessageBus {
        NapiMessageBus {
            inner: self.inner.message_bus(),
        }
    }

    /// Handle tailnet peers update from the sidecar.
    #[napi]
    pub async fn handle_tailnet_peers(&self, peers: Vec<NapiTailnetPeer>) -> Result<()> {
        let core_peers: Vec<_> = peers.iter().map(napi_peer_to_core).collect();
        self.inner.handle_tailnet_peers(&core_peers).await;
        Ok(())
    }

    /// Set the local device as online with its Tailscale IP.
    #[napi]
    pub async fn set_local_online(
        &self,
        tailscale_ip: String,
        dns_name: Option<String>,
    ) -> Result<()> {
        self.inner
            .set_local_online(&tailscale_ip, dns_name.as_deref())
            .await;
        Ok(())
    }

    /// Subscribe to mesh events.
    ///
    /// Uses ThreadsafeFunction with Blocking mode per RFC 003:
    /// - Critical events (device join/leave, election, errors) are never dropped
    /// - If the JS event queue is full, Rust waits rather than dropping events
    ///
    /// The callback receives NapiMeshEvent objects with event_type, device_id, and payload.
    ///
    /// Can be called multiple times to add multiple subscribers.
    /// Can be called before or after `start()`. If called before `start()`, the
    /// event loop is spawned when `start()` is called.
    #[napi(ts_args_type = "callback: (err: null | Error, event: NapiMeshEvent) => void")]
    pub fn on_event(&self, callback: ThreadsafeFunction<NapiMeshEvent>) -> Result<()> {
        // If a Tokio runtime is available (called after start or from async context),
        // spawn the event loop immediately with a new subscriber.
        if let Ok(_handle) = tokio::runtime::Handle::try_current() {
            let rx = self.inner.subscribe_events();
            tokio::spawn(async move {
                Self::event_loop(rx, callback).await;
            });
        } else {
            // No Tokio runtime — store callback for start() to spawn.
            // If one is already pending, we need to handle both.
            let mut guard = self
                .pending_callback
                .lock()
                .map_err(|_| Error::from_reason("pending_callback lock poisoned"))?;
            if guard.is_some() {
                return Err(Error::from_reason(
                    "on_event called twice before start() - call start() first, then add more subscribers",
                ));
            }
            *guard = Some(callback);
        }

        Ok(())
    }

    /// Internal event loop that forwards MeshNodeEvents to JS.
    ///
    /// IMPORTANT: This function must NEVER panic. All conversions use
    /// fallible operations. If a conversion fails, the event is logged
    /// and skipped rather than crashing the Node.js process.
    async fn event_loop(
        mut rx: broadcast::Receiver<MeshNodeEvent>,
        callback: ThreadsafeFunction<NapiMeshEvent>,
    ) {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let napi_event = mesh_event_to_napi(&event);
                    // Blocking mode: waits if JS event queue is full.
                    // This ensures critical events are never dropped.
                    let status = callback.call(Ok(napi_event), ThreadsafeFunctionCallMode::Blocking);
                    if status != Status::Ok {
                        tracing::warn!("Failed to deliver mesh event to JS: {:?}", status);
                        if status == Status::Closing || status == Status::InvalidArg {
                            tracing::info!("Event callback closed, stopping event loop");
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Mesh event receiver lagged by {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::info!("Mesh event loop ended (channel closed)");
                    break;
                }
            }
        }
    }
}

/// NapiMessageBus - Node.js wrapper for MeshMessageBus.
///
/// Provides namespace-based pub/sub for application-level messages.
#[napi]
pub struct NapiMessageBus {
    inner: Arc<MeshMessageBus>,
}

#[napi]
impl NapiMessageBus {
    /// Get all subscribed namespaces.
    #[napi]
    pub async fn subscribed_namespaces(&self) -> Result<Vec<String>> {
        Ok(self.inner.subscribed_namespaces().await)
    }

    /// Dispose the message bus, clearing all handlers.
    #[napi]
    pub async fn dispose(&self) -> Result<()> {
        self.inner.dispose().await;
        Ok(())
    }
}
