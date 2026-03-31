//! NapiNode — Node.js wrapper for `Node<TailscaleProvider>`.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use tokio::task::JoinHandle;

use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::Node;

use crate::file_transfer::NapiFileTransfer;
use crate::synced_store::NapiSyncedStore;
use crate::types::{
    NapiHealthInfo, NapiNamespacedMessage, NapiNodeConfig, NapiNodeIdentity, NapiPeer,
    NapiPeerEvent, NapiPingResult,
};

/// Helper: convert a truffle-core `Peer` to a `NapiPeer`.
fn peer_to_napi(p: &truffle_core::node::Peer) -> NapiPeer {
    NapiPeer {
        id: p.id.clone(),
        name: p.name.clone(),
        ip: p.ip.to_string(),
        online: p.online,
        ws_connected: p.ws_connected,
        connection_type: p.connection_type.clone(),
        os: p.os.clone(),
        last_seen: p.last_seen.clone(),
    }
}

/// The main truffle node exposed to JavaScript.
///
/// Lifecycle:
/// 1. `new NapiNode()` — creates an empty wrapper.
/// 2. `await node.start(config)` — builds and starts the underlying `Node<TailscaleProvider>`.
/// 3. Use `getPeers()`, `send()`, `onPeerChange()`, etc.
/// 4. `await node.stop()` — shuts down the node.
#[napi]
pub struct NapiNode {
    node: Option<Arc<Node<TailscaleProvider>>>,
    /// Handles to spawned event-forwarding tasks (cancelled on stop).
    task_handles: Vec<JoinHandle<()>>,
}

#[napi]
impl NapiNode {
    /// Create a new (unstarted) NapiNode.
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            node: None,
            task_handles: Vec::new(),
        }
    }

    /// Start the node with the given configuration.
    ///
    /// Resolves when the Tailscale sidecar is connected and the node is ready.
    ///
    /// # Safety
    /// This takes `&mut self` in an async context. The caller must ensure that
    /// no other calls are made on this NapiNode while `start()` is in progress.
    #[napi]
    pub async unsafe fn start(&mut self, config: NapiNodeConfig) -> Result<()> {
        let mut builder = Node::<TailscaleProvider>::builder()
            .name(&config.name)
            .sidecar_path(&config.sidecar_path);

        if let Some(ref dir) = config.state_dir {
            builder = builder.state_dir(dir);
        }
        if let Some(ref key) = config.auth_key {
            builder = builder.auth_key(key);
        }
        if let Some(ephemeral) = config.ephemeral {
            builder = builder.ephemeral(ephemeral);
        }
        if let Some(port) = config.ws_port {
            builder = builder.ws_port(port);
        }

        let node = builder
            .build()
            .await
            .map_err(|e| Error::from_reason(format!("Failed to start node: {e}")))?;

        self.node = Some(Arc::new(node));
        Ok(())
    }

    /// Stop the node and release all resources.
    ///
    /// # Safety
    /// This takes `&mut self` in an async context. The caller must ensure that
    /// no other calls are made on this NapiNode while `stop()` is in progress.
    #[napi]
    pub async unsafe fn stop(&mut self) -> Result<()> {
        // Cancel all event-forwarding tasks
        for handle in self.task_handles.drain(..) {
            handle.abort();
        }
        if let Some(node) = self.node.take() {
            node.stop().await;
        }
        Ok(())
    }

    /// Get the local node's identity.
    #[napi]
    pub fn get_local_info(&self) -> Result<NapiNodeIdentity> {
        let node = self.require_node()?;
        let info = node.local_info();
        Ok(NapiNodeIdentity {
            id: info.id,
            hostname: info.hostname,
            name: info.name,
            dns_name: info.dns_name,
            ip: info.ip.map(|ip| ip.to_string()),
        })
    }

    /// Get all known peers.
    #[napi]
    pub async fn get_peers(&self) -> Result<Vec<NapiPeer>> {
        let node = self.require_node()?;
        let peers = node.peers().await;
        Ok(peers.iter().map(peer_to_napi).collect())
    }

    /// Resolve a peer identifier (name or ID) to the canonical node ID.
    #[napi]
    pub async fn resolve_peer_id(&self, peer_id: String) -> Result<String> {
        let node = self.require_node()?;
        node.resolve_peer_id(&peer_id)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Ping a peer and return latency info.
    #[napi]
    pub async fn ping(&self, peer_id: String) -> Result<NapiPingResult> {
        let node = self.require_node()?;
        let result = node
            .ping(&peer_id)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiPingResult {
            latency_ms: result.latency.as_secs_f64() * 1000.0,
            connection: result.connection,
            peer_addr: result.peer_addr,
        })
    }

    /// Get health information from the network layer.
    #[napi]
    pub async fn health(&self) -> Result<NapiHealthInfo> {
        let node = self.require_node()?;
        let info = node.health().await;
        Ok(NapiHealthInfo {
            state: info.state,
            key_expiry: info.key_expiry,
            warnings: info.warnings,
            healthy: info.healthy,
        })
    }

    /// Send a namespaced message to a specific peer.
    #[napi]
    pub async fn send(
        &self,
        peer_id: String,
        namespace: String,
        data: Buffer,
    ) -> Result<()> {
        let node = self.require_node()?;
        node.send(&peer_id, &namespace, data.as_ref())
            .await
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Broadcast a namespaced message to all connected peers.
    #[napi]
    pub async fn broadcast(&self, namespace: String, data: Buffer) -> Result<()> {
        let node = self.require_node()?;
        node.broadcast(&namespace, data.as_ref()).await;
        Ok(())
    }

    /// Subscribe to peer change events.
    ///
    /// The callback receives `NapiPeerEvent` objects whenever peers
    /// join, leave, connect, disconnect, or update.
    #[napi(
        ts_args_type = "callback: (event: PeerEvent) => void"
    )]
    pub fn on_peer_change(
        &mut self,
        callback: ThreadsafeFunction<NapiPeerEvent>,
    ) -> Result<()> {
        let node = self.require_node()?;
        let mut rx = node.on_peer_change();

        let handle = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let napi_event = convert_peer_event(&event);
                        let status = callback.call(
                            Ok(napi_event),
                            ThreadsafeFunctionCallMode::NonBlocking,
                        );
                        if status != Status::Ok {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "on_peer_change lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        self.task_handles.push(handle);

        Ok(())
    }

    /// Subscribe to messages on a specific namespace.
    ///
    /// The callback receives `NapiNamespacedMessage` objects.
    #[napi(
        ts_args_type = "namespace: string, callback: (msg: NamespacedMessage) => void"
    )]
    pub fn on_message(
        &mut self,
        namespace: String,
        callback: ThreadsafeFunction<NapiNamespacedMessage>,
    ) -> Result<()> {
        let node = self.require_node()?;
        let mut rx = node.subscribe(&namespace);

        let handle = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        let napi_msg = NapiNamespacedMessage {
                            from: msg.from,
                            namespace: msg.namespace,
                            msg_type: msg.msg_type,
                            payload: msg.payload,
                            timestamp: msg.timestamp.map(|t| t as f64),
                        };
                        let status = callback.call(
                            Ok(napi_msg),
                            ThreadsafeFunctionCallMode::NonBlocking,
                        );
                        if status != Status::Ok {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "on_message lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        self.task_handles.push(handle);

        Ok(())
    }

    /// Get a `NapiFileTransfer` handle for file transfer operations.
    #[napi]
    pub fn file_transfer(&self) -> Result<NapiFileTransfer> {
        let node = self.require_node()?;
        Ok(NapiFileTransfer::new(node))
    }

    /// Get a `NapiSyncedStore` handle for synchronized state operations.
    ///
    /// Each call creates a new store instance with the given `store_id`.
    /// Multiple stores can coexist with different IDs.
    #[napi]
    pub fn synced_store(&self, store_id: String) -> Result<NapiSyncedStore> {
        let node = self.require_node()?;
        Ok(NapiSyncedStore::new(node, &store_id))
    }

    // -- Internal helpers ----------------------------------------------------

    fn require_node(&self) -> Result<Arc<Node<TailscaleProvider>>> {
        self.node
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::from_reason("Node not started. Call start() first."))
    }
}

// ---------------------------------------------------------------------------
// PeerEvent conversion
// ---------------------------------------------------------------------------

fn convert_peer_event(event: &truffle_core::session::PeerEvent) -> NapiPeerEvent {
    use truffle_core::session::PeerEvent;

    match event {
        PeerEvent::Joined(state) => NapiPeerEvent {
            event_type: "joined".to_string(),
            peer_id: state.id.clone(),
            peer: Some(NapiPeer {
                id: state.id.clone(),
                name: state.name.clone(),
                ip: state.ip.to_string(),
                online: state.online,
                ws_connected: state.ws_connected,
                connection_type: state.connection_type.clone(),
                os: state.os.clone(),
                last_seen: state.last_seen.clone(),
            }),
            auth_url: None,
        },
        PeerEvent::Left(id) => NapiPeerEvent {
            event_type: "left".to_string(),
            peer_id: id.clone(),
            peer: None,
            auth_url: None,
        },
        PeerEvent::Updated(state) => NapiPeerEvent {
            event_type: "updated".to_string(),
            peer_id: state.id.clone(),
            peer: Some(NapiPeer {
                id: state.id.clone(),
                name: state.name.clone(),
                ip: state.ip.to_string(),
                online: state.online,
                ws_connected: state.ws_connected,
                connection_type: state.connection_type.clone(),
                os: state.os.clone(),
                last_seen: state.last_seen.clone(),
            }),
            auth_url: None,
        },
        PeerEvent::WsConnected(id) => NapiPeerEvent {
            event_type: "ws_connected".to_string(),
            peer_id: id.clone(),
            peer: None,
            auth_url: None,
        },
        PeerEvent::WsDisconnected(id) => NapiPeerEvent {
            event_type: "ws_disconnected".to_string(),
            peer_id: id.clone(),
            peer: None,
            auth_url: None,
        },
        PeerEvent::AuthRequired { url } => NapiPeerEvent {
            event_type: "auth_required".to_string(),
            peer_id: String::new(),
            peer: None,
            auth_url: Some(url.clone()),
        },
    }
}
