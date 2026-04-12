//! NapiNode — Node.js wrapper for `Node<TailscaleProvider>`.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Status, Unknown};
use napi_derive::napi;
use tokio::task::JoinHandle;

use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::Node;

use crate::crdt_doc::NapiCrdtDoc;
use crate::file_transfer::NapiFileTransfer;
use crate::proxy::NapiProxy;
use crate::synced_store::NapiSyncedStore;
use crate::types::{
    NapiHealthInfo, NapiNamespacedMessage, NapiNodeConfig, NapiNodeIdentity, NapiPeer,
    NapiPeerEvent, NapiPingResult,
};

/// Helper: convert a truffle-core `Peer` to a `NapiPeer`.
///
/// RFC 017: the user-facing identity is `device_id` / `device_name`, with
/// the Tailscale stable ID preserved as an escape hatch for diagnostics.
fn peer_to_napi(p: &truffle_core::node::Peer) -> NapiPeer {
    NapiPeer {
        device_id: p.device_id.clone(),
        device_name: p.device_name.clone(),
        ip: p.ip.to_string(),
        online: p.online,
        ws_connected: p.ws_connected,
        connection_type: p.connection_type.clone(),
        os: p.os.clone(),
        last_seen: p.last_seen.clone(),
        tailscale_id: p.tailscale_id.clone(),
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
    /// Pre-start auth-required callback. Installed via `on_auth_required()`
    /// and consumed by `start()`, which wires it into
    /// `NodeBuilder::build_with_auth_handler`. Necessary because auth fires
    /// during `start()` — before a post-start `on_peer_change` subscription
    /// could possibly exist.
    ///
    /// `CalleeHandled = false` so the JS callback receives `(url)` directly
    /// rather than `(err, url)` — there's no async error path here.
    pending_auth_callback:
        Option<ThreadsafeFunction<String, Unknown<'static>, String, Status, false>>,
}

#[napi]
impl NapiNode {
    /// Create a new (unstarted) NapiNode.
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            node: None,
            task_handles: Vec::new(),
            pending_auth_callback: None,
        }
    }

    /// Install a callback to receive the Tailscale auth URL.
    ///
    /// Must be called BEFORE `start()`. During startup the Tailscale
    /// provider may require interactive authentication; if so, this
    /// callback is invoked with the auth URL while `start()` is still
    /// waiting. If no callback is installed, `start()` uses the plain
    /// build path and may reject with "timed out waiting for
    /// authentication" after 5 minutes.
    #[napi(ts_args_type = "callback: (url: string) => void")]
    pub fn on_auth_required(
        &mut self,
        callback: ThreadsafeFunction<String, Unknown<'static>, String, Status, false>,
    ) -> Result<()> {
        self.pending_auth_callback = Some(callback);
        Ok(())
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
        // RFC 017: app_id is required and validated; device_name /
        // device_id are optional overrides that the builder defaults when
        // absent (OS hostname / auto-generated ULID).
        let mut builder = Node::<TailscaleProvider>::builder()
            .app_id(&config.app_id)
            .map_err(|e| Error::from_reason(e.to_string()))?
            .sidecar_path(&config.sidecar_path);

        if let Some(ref device_name) = config.device_name {
            builder = builder.device_name(device_name);
        }
        if let Some(ref device_id) = config.device_id {
            builder = builder
                .device_id(device_id)
                .map_err(|e| Error::from_reason(e.to_string()))?;
        }
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

        // If the caller installed a pre-start auth handler via
        // `on_auth_required()`, use `build_with_auth_handler` so the auth
        // URL is forwarded to JS while the provider is still waiting for
        // login. Otherwise fall back to the plain build path.
        let node = match self.pending_auth_callback.take() {
            Some(cb) => builder
                .build_with_auth_handler(move |url| {
                    // Fatal error strategy: pass the value directly.
                    cb.call(url, ThreadsafeFunctionCallMode::NonBlocking);
                })
                .await
                .map_err(|e| Error::from_reason(format!("Failed to start node: {e}")))?,
            None => builder
                .build()
                .await
                .map_err(|e| Error::from_reason(format!("Failed to start node: {e}")))?,
        };

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
            app_id: info.app_id,
            device_id: info.device_id,
            device_name: info.device_name,
            tailscale_hostname: info.tailscale_hostname,
            tailscale_id: info.tailscale_id,
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
    pub async fn send(&self, peer_id: String, namespace: String, data: Buffer) -> Result<()> {
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
    #[napi(ts_args_type = "callback: (event: PeerEvent) => void")]
    pub fn on_peer_change(
        &mut self,
        callback: ThreadsafeFunction<NapiPeerEvent, Unknown<'static>, NapiPeerEvent, Status, false>,
    ) -> Result<()> {
        let node = self.require_node()?;
        let mut rx = node.on_peer_change();
        let node_for_lookup = node.clone();

        // Use napi-rs's managed Tokio runtime. Bare `tokio::spawn` panics
        // with "there is no reactor running" because this is a sync NAPI
        // method invoked on the Node.js main thread, which has no Tokio
        // runtime in context.
        let handle = napi::bindgen_prelude::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let napi_event = convert_peer_event(&event, Some(&node_for_lookup)).await;
                        let status =
                            callback.call(napi_event, ThreadsafeFunctionCallMode::NonBlocking);
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
    #[napi(ts_args_type = "namespace: string, callback: (msg: NamespacedMessage) => void")]
    pub fn on_message(
        &mut self,
        namespace: String,
        callback: ThreadsafeFunction<
            NapiNamespacedMessage,
            Unknown<'static>,
            NapiNamespacedMessage,
            Status,
            false,
        >,
    ) -> Result<()> {
        let node = self.require_node()?;
        let mut rx = node.subscribe(&namespace);

        // Use napi-rs's managed Tokio runtime. Bare `tokio::spawn` panics
        // with "there is no reactor running" because this is a sync NAPI
        // method invoked on the Node.js main thread, which has no Tokio
        // runtime in context.
        let handle = napi::bindgen_prelude::spawn(async move {
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
                        let status =
                            callback.call(napi_msg, ThreadsafeFunctionCallMode::NonBlocking);
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

    /// Get a `NapiProxy` handle for reverse proxy operations.
    #[napi]
    pub fn proxy(&self) -> Result<NapiProxy> {
        let node = self.require_node()?;
        Ok(NapiProxy::new(node))
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

    /// Get a `NapiCrdtDoc` handle for CRDT document operations.
    ///
    /// Each call creates a new document instance with the given `doc_id`.
    /// The document syncs automatically with peers on namespace `"crdt:{doc_id}"`.
    ///
    /// This is intentionally **synchronous** (matching `synced_store()`).
    /// `CrdtDoc::new` is async and calls `tokio::spawn` internally; we enter
    /// the napi-rs managed Tokio runtime so the spawn hits a live reactor.
    #[napi]
    pub fn crdt_doc(&self, doc_id: String) -> Result<NapiCrdtDoc> {
        let node = self.require_node()?;
        NapiCrdtDoc::new(node, &doc_id)
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

/// Convert a core `PeerEvent` into its NAPI shape.
///
/// For `joined` / `updated` events we derive `peer_id` from the
/// RFC 017 device_id carried on the hello envelope (via `Peer::from`).
/// For `left` / `ws_connected` / `ws_disconnected` the session layer
/// only gives us a Tailscale stable ID; if a `node` is provided we
/// attempt to resolve that to a `device_id` via the node's peer cache
/// so JS callers can key every `peer_id` by the same identifier.
/// If resolution fails (e.g. transient races during disconnect) we
/// fall back to the Tailscale stable ID — callers should be aware.
async fn convert_peer_event(
    event: &truffle_core::session::PeerEvent,
    node: Option<&Arc<Node<TailscaleProvider>>>,
) -> NapiPeerEvent {
    use truffle_core::session::PeerEvent;

    // Helper: funnel through `Peer::from(PeerState)` so the `device_id`
    // / `device_name` fallback logic lives in one place (see core/node.rs).
    // The resulting `Peer` has `device_id == identity.device_id` when the
    // hello has landed, otherwise it falls back to the Tailscale stable ID.
    let from_state = |state: &truffle_core::session::PeerState| -> NapiPeer {
        let peer: truffle_core::node::Peer = state.clone().into();
        peer_to_napi(&peer)
    };

    // Resolve a Tailscale stable ID to the hello-advertised device_id by
    // scanning the node's cached peer list. Returns the input on miss.
    async fn resolve_device_id(
        node: Option<&Arc<Node<TailscaleProvider>>>,
        tailscale_id: &str,
    ) -> String {
        if let Some(node) = node {
            for p in node.peers().await {
                if p.tailscale_id == tailscale_id {
                    return p.device_id.clone();
                }
            }
        }
        tailscale_id.to_string()
    }

    match event {
        PeerEvent::Joined(state) => {
            let peer = from_state(state);
            NapiPeerEvent {
                event_type: "joined".to_string(),
                peer_id: peer.device_id.clone(),
                peer: Some(peer),
                auth_url: None,
            }
        }
        PeerEvent::Left(id) => NapiPeerEvent {
            event_type: "left".to_string(),
            peer_id: resolve_device_id(node, id).await,
            peer: None,
            auth_url: None,
        },
        PeerEvent::Updated(state) => {
            let peer = from_state(state);
            NapiPeerEvent {
                event_type: "updated".to_string(),
                peer_id: peer.device_id.clone(),
                peer: Some(peer),
                auth_url: None,
            }
        }
        PeerEvent::WsConnected(id) => NapiPeerEvent {
            event_type: "ws_connected".to_string(),
            peer_id: resolve_device_id(node, id).await,
            peer: None,
            auth_url: None,
        },
        PeerEvent::WsDisconnected(id) => NapiPeerEvent {
            event_type: "ws_disconnected".to_string(),
            peer_id: resolve_device_id(node, id).await,
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
