use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use tokio::sync::broadcast;

use truffle_core::mesh::election::ElectionTimingConfig;
use truffle_core::mesh::message_bus::MeshMessageBus;
use truffle_core::mesh::node::{MeshNodeConfig, MeshTimingConfig};
use truffle_core::protocol::envelope::MeshEnvelope;
use truffle_core::runtime::{RuntimeConfig, TruffleEvent, TruffleRuntime};
use truffle_core::transport::connection::TransportConfig;

use crate::types::{
    napi_peer_to_core, truffle_event_to_napi, NapiBaseDevice,
    NapiMeshNodeConfig, NapiTailnetPeer, NapiTruffleEvent,
};

/// NapiMeshNode - Node.js wrapper for TruffleRuntime.
///
/// Manages the full lifecycle: BridgeManager, GoShim sidecar, ConnectionManager,
/// and MeshNode via the unified `TruffleRuntime`. The sidecar provides Tailscale
/// connectivity; the bridge routes TCP streams; the connection manager upgrades
/// them to WebSocket; and the mesh node handles device discovery, election,
/// and messaging.
///
/// ## RFC 008 Phase 3
/// This wrapper now delegates to `TruffleRuntime` instead of manually wiring
/// individual components. Events are delivered as `TruffleEvent` through the
/// unified channel, converted to `NapiTruffleEvent` for JS consumption.
#[napi]
pub struct NapiMeshNode {
    runtime: Arc<TruffleRuntime>,
    pending_callback: std::sync::Mutex<Option<ThreadsafeFunction<NapiTruffleEvent>>>,
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

        let runtime_config = RuntimeConfig {
            mesh: core_config,
            transport: TransportConfig::default(),
            sidecar_path: config.sidecar_path.map(std::path::PathBuf::from),
            state_dir: config.state_dir,
            auth_key: config.auth_key,
        };

        let (runtime, _event_rx) = TruffleRuntime::new(&runtime_config);

        Ok(Self {
            runtime: Arc::new(runtime),
            pending_callback: std::sync::Mutex::new(None),
        })
    }

    /// Start the mesh node.
    ///
    /// If `sidecarPath` was provided, this spawns the Go sidecar process,
    /// creates a BridgeManager to route connections, and wires everything
    /// together for full Tailscale mesh networking.
    ///
    /// Events are delivered through the unified `TruffleEvent` channel,
    /// converted to `NapiTruffleEvent` for JS consumption.
    #[napi]
    pub async fn start(&self) -> Result<()> {
        // Spawn the JS event loop if a callback was registered before start
        let callback = self
            .pending_callback
            .lock()
            .map_err(|_| Error::from_reason("pending_callback lock poisoned"))?
            .take();

        if let Some(cb) = callback {
            let rx = self.runtime.subscribe();
            tokio::spawn(async move {
                Self::event_loop(rx, cb).await;
            });
        }

        // Start the runtime (handles sidecar, bridge, mesh node, event forwarding)
        self.runtime.start().await
            .map_err(|e| Error::from_reason(format!("runtime start failed: {e}")))?;

        Ok(())
    }

    /// Stop the mesh node and sidecar.
    #[napi]
    pub async fn stop(&self) -> Result<()> {
        self.runtime.stop().await;
        Ok(())
    }

    /// Check if the node is running.
    #[napi]
    pub async fn is_running(&self) -> Result<bool> {
        Ok(self.runtime.mesh_node().is_running().await)
    }

    /// Get the local device info.
    #[napi]
    pub async fn local_device(&self) -> Result<NapiBaseDevice> {
        let device = self.runtime.local_device().await;
        Ok(NapiBaseDevice::from(device))
    }

    /// Get the local device ID.
    #[napi]
    pub async fn device_id(&self) -> Result<String> {
        Ok(self.runtime.device_id().await)
    }

    /// Get all known devices.
    #[napi]
    pub async fn devices(&self) -> Result<Vec<NapiBaseDevice>> {
        let devices = self.runtime.devices().await;
        Ok(devices.into_iter().map(NapiBaseDevice::from).collect())
    }

    /// Get a device by ID.
    #[napi]
    pub async fn device_by_id(&self, id: String) -> Result<Option<NapiBaseDevice>> {
        let device = self.runtime.mesh_node().device_by_id(&id).await;
        Ok(device.map(NapiBaseDevice::from))
    }

    /// Check if this node is the primary.
    #[napi]
    pub async fn is_primary(&self) -> Result<bool> {
        Ok(self.runtime.is_primary().await)
    }

    /// Get the current primary device ID.
    #[napi]
    pub async fn primary_id(&self) -> Result<Option<String>> {
        Ok(self.runtime.mesh_node().primary_id().await)
    }

    /// Get the current role as a string ("primary" or "secondary").
    #[napi]
    pub async fn role(&self) -> Result<String> {
        let role = self.runtime.mesh_node().role().await;
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
        Ok(self.runtime.send_envelope(&device_id, &envelope).await)
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
        self.runtime.broadcast_envelope(&envelope).await;
        Ok(())
    }

    /// Get the current auth status ("unknown", "required", "authenticated").
    #[napi]
    pub async fn auth_status(&self) -> Result<String> {
        let status = self.runtime.mesh_node().auth_status().await;
        Ok(match status {
            truffle_core::types::AuthStatus::Unknown => "unknown".to_string(),
            truffle_core::types::AuthStatus::Required => "required".to_string(),
            truffle_core::types::AuthStatus::Authenticated => "authenticated".to_string(),
        })
    }

    /// Get the current auth URL, if auth is required.
    #[napi]
    pub async fn auth_url(&self) -> Result<Option<String>> {
        Ok(self.runtime.mesh_node().auth_url().await)
    }

    /// Get the message bus for namespace-based pub/sub.
    #[napi]
    pub fn message_bus(&self) -> NapiMessageBus {
        NapiMessageBus {
            inner: self.runtime.bus(),
        }
    }

    /// Handle tailnet peers update from the sidecar.
    #[napi]
    pub async fn handle_tailnet_peers(&self, peers: Vec<NapiTailnetPeer>) -> Result<()> {
        let core_peers: Vec<_> = peers.iter().map(napi_peer_to_core).collect();
        self.runtime.mesh_node().handle_tailnet_peers(&core_peers).await;
        Ok(())
    }

    /// Set the local device as online with its Tailscale IP.
    #[napi]
    pub async fn set_local_online(
        &self,
        tailscale_ip: String,
        dns_name: Option<String>,
    ) -> Result<()> {
        self.runtime.mesh_node()
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
    /// The callback receives NapiTruffleEvent objects with event_type, device_id, and payload.
    ///
    /// Can be called multiple times to add multiple subscribers.
    /// Can be called before or after `start()`. If called before `start()`, the
    /// event loop is spawned when `start()` is called.
    #[napi(ts_args_type = "callback: (err: null | Error, event: NapiMeshEvent) => void")]
    pub fn on_event(&self, callback: ThreadsafeFunction<NapiTruffleEvent>) -> Result<()> {
        // If a Tokio runtime is available (called after start or from async context),
        // spawn the event loop immediately with a new subscriber.
        if let Ok(_handle) = tokio::runtime::Handle::try_current() {
            let rx = self.runtime.subscribe();
            tokio::spawn(async move {
                Self::event_loop(rx, callback).await;
            });
        } else {
            // No Tokio runtime -- store callback for start() to spawn.
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

    /// Internal event loop that forwards TruffleEvents to JS.
    ///
    /// IMPORTANT: This function must NEVER panic. All conversions use
    /// fallible operations. If a conversion fails, the event is logged
    /// and skipped rather than crashing the Node.js process.
    async fn event_loop(
        mut rx: broadcast::Receiver<TruffleEvent>,
        callback: ThreadsafeFunction<NapiTruffleEvent>,
    ) {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let napi_event = truffle_event_to_napi(&event);
                    // Blocking mode: waits if JS event queue is full.
                    // This ensures critical events are never dropped.
                    let status = callback.call(Ok(napi_event), ThreadsafeFunctionCallMode::Blocking);
                    if status != Status::Ok {
                        tracing::warn!("Failed to deliver truffle event to JS: {:?}", status);
                        if status == Status::Closing || status == Status::InvalidArg {
                            tracing::info!("Event callback closed, stopping event loop");
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Truffle event receiver lagged by {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::info!("Truffle event loop ended (channel closed)");
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
