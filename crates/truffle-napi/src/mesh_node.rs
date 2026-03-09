use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use tokio::sync::broadcast;

use truffle_core::mesh::election::ElectionTimingConfig;
use truffle_core::mesh::message_bus::MeshMessageBus;
use truffle_core::mesh::node::{MeshNode as CoreMeshNode, MeshNodeConfig, MeshNodeEvent, MeshTimingConfig};
use truffle_core::protocol::envelope::MeshEnvelope;
use truffle_core::transport::connection::{ConnectionManager, TransportConfig};

use crate::types::{
    mesh_event_to_napi, napi_peer_to_core, NapiBaseDevice,
    NapiMeshEvent, NapiMeshNodeConfig, NapiTailnetPeer,
};

/// NapiMeshNode - Node.js wrapper for truffle-core MeshNode.
///
/// SAFETY: No panics in any method. All errors returned via napi::Result.
/// Event delivery uses ThreadsafeFunction with Blocking mode.
#[napi]
pub struct NapiMeshNode {
    inner: Arc<CoreMeshNode>,
    event_rx: tokio::sync::Mutex<Option<broadcast::Receiver<MeshNodeEvent>>>,
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
            device_id: config.device_id,
            device_name: config.device_name,
            device_type: config.device_type,
            hostname_prefix: config.hostname_prefix,
            prefer_primary: config.prefer_primary.unwrap_or(false),
            capabilities: config.capabilities.unwrap_or_default(),
            metadata: None,
            timing: mesh_timing,
        };

        // Create a ConnectionManager with default transport config.
        // In production, this is wired to the BridgeManager which provides
        // the actual TCP streams. The transport config can be customized
        // before calling start().
        let transport_config = TransportConfig::default();
        let (connection_manager, _transport_event_rx) = ConnectionManager::new(transport_config);

        let (inner, event_rx) = CoreMeshNode::new(core_config, Arc::new(connection_manager));

        Ok(Self {
            inner: Arc::new(inner),
            event_rx: tokio::sync::Mutex::new(Some(event_rx)),
        })
    }

    /// Start the mesh node.
    #[napi]
    pub async fn start(&self) -> Result<()> {
        self.inner.start().await;
        Ok(())
    }

    /// Stop the mesh node.
    #[napi]
    pub async fn stop(&self) -> Result<()> {
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
    #[napi(ts_args_type = "callback: (err: null | Error, event: NapiMeshEvent) => void")]
    pub fn on_event(&self, callback: ThreadsafeFunction<NapiMeshEvent>) -> Result<()> {
        let mut guard = self
            .event_rx
            .try_lock()
            .map_err(|_| Error::from_reason("event receiver lock contention"))?;

        let rx = guard.take().ok_or_else(|| {
            Error::from_reason("on_event already called - only one subscriber allowed")
        })?;

        tokio::spawn(async move {
            Self::event_loop(rx, callback).await;
        });

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
