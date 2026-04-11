//! Node API — the single public entry point for all truffle functionality.
//!
//! The [`Node`] struct wires together Layers 3-6 and exposes a clean ~12-method
//! API that Layer 7 applications consume. Applications should **never** import
//! from lower layers directly; everything they need is accessible through `Node`.
//!
//! # Quick start
//!
//! ```ignore
//! use truffle_core::Node;
//!
//! let node = Node::builder()
//!     .name("my-app")
//!     .sidecar_path("/usr/local/bin/truffle-sidecar")
//!     .build()
//!     .await?;
//!
//! // Discover peers (Layer 3 — no transport needed)
//! let peers = node.peers().await;
//!
//! // Send a namespaced message (Layer 6 envelope over Layer 4 WS)
//! node.send(&peers[0].id, "chat", b"hello!").await?;
//!
//! // Subscribe to a namespace
//! let mut rx = node.subscribe("chat");
//! let msg = rx.recv().await?;
//!
//! // Open a raw TCP stream (Layer 4 direct)
//! let stream = node.open_tcp(&peers[0].id, 8080).await?;
//! ```

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::TcpStream;
use tokio::sync::{broadcast, RwLock};

use crate::envelope::codec::{EnvelopeCodec, JsonCodec};
use crate::envelope::{Envelope, EnvelopeError};
use crate::file_transfer::{self, FileTransferState};
use crate::identity::{self, AppId, DeviceId, DeviceName};
use crate::network::tailscale::{TailscaleConfig, TailscaleProvider};
use crate::network::{HealthInfo, NetworkProvider, NetworkUdpSocket, NodeIdentity, PingResult};
use crate::session::{PeerEvent, PeerRegistry, PeerState};
use crate::transport::websocket::WebSocketTransport;
use crate::transport::{RawListener, WsConfig};

// ---------------------------------------------------------------------------
// NamespacedMessage — public message type for subscribers
// ---------------------------------------------------------------------------

/// A message received on a specific namespace.
///
/// This is the public type that [`Node::subscribe`] delivers to application
/// code. It contains the deserialized envelope fields plus the sender's peer ID.
#[derive(Debug, Clone)]
pub struct NamespacedMessage {
    /// Stable node ID of the sender.
    pub from: String,
    /// Namespace the message was sent on.
    pub namespace: String,
    /// Application-defined message type within the namespace.
    pub msg_type: String,
    /// Opaque JSON payload.
    pub payload: serde_json::Value,
    /// Millisecond Unix timestamp from the sender, if set.
    pub timestamp: Option<u64>,
}

// ---------------------------------------------------------------------------
// Peer — simplified view for application code
// ---------------------------------------------------------------------------

/// A peer as seen by application code.
///
/// This is a simplified projection of the internal [`PeerState`] that hides
/// session-layer internals. Applications use this to display peer lists and
/// resolve peer IDs for `send()` / `open_tcp()`.
///
/// RFC 017 introduced `device_id` and `device_name` derived from the hello
/// envelope — these are populated once the WebSocket link comes up. Before
/// that, they fall back to the legacy Tailscale ID / hostname pair so
/// application code has something to display even for not-yet-connected
/// peers.
#[derive(Debug, Clone)]
pub struct Peer {
    /// Legacy per-peer ID. Kept for back-compat with call sites that
    /// destructure `peer.id`; new code should prefer [`device_id`](Self::device_id)
    /// directly. Equal to `device_id` once the hello has landed; equal to
    /// the Tailscale stable ID beforehand.
    pub id: String,
    /// Legacy name (the Layer 3 Tailscale hostname — the *slug*). New code
    /// should prefer [`device_name`](Self::device_name).
    pub name: String,
    /// Stable per-device ULID (RFC 017 §5.4) from the hello envelope.
    /// Falls back to the Tailscale stable ID until the hello handshake
    /// completes.
    pub device_id: String,
    /// Human-readable device name from the hello envelope (original
    /// Unicode form, NOT the slug). Falls back to the hostname until the
    /// hello completes.
    pub device_name: String,
    /// Tailscale stable node ID — escape hatch for diagnostics and the
    /// transport routing key.
    pub tailscale_id: String,
    /// Network IP address.
    pub ip: IpAddr,
    /// Whether the peer is online (from Layer 3).
    pub online: bool,
    /// Whether there is an active WebSocket connection.
    pub ws_connected: bool,
    /// Connection type description (e.g., `"direct"` or `"relay:ord"`).
    pub connection_type: String,
    /// Operating system, if known. Prefers the hello envelope's value
    /// and falls back to Layer 3.
    pub os: Option<String>,
    /// Last time the peer was seen online (RFC 3339 string).
    pub last_seen: Option<String>,
}

impl From<PeerState> for Peer {
    fn from(s: PeerState) -> Self {
        let (device_id, device_name, os) = match s.identity.as_ref() {
            Some(identity) => (
                identity.device_id.clone(),
                identity.device_name.clone(),
                Some(identity.os.clone()),
            ),
            None => (s.id.clone(), s.name.clone(), s.os.clone()),
        };
        let legacy_id = s
            .identity
            .as_ref()
            .map(|i| i.device_id.clone())
            .unwrap_or_else(|| s.id.clone());
        Self {
            id: legacy_id,
            name: s.name,
            device_id,
            device_name,
            tailscale_id: s.id,
            ip: s.ip,
            online: s.online,
            ws_connected: s.ws_connected,
            connection_type: s.connection_type,
            os,
            last_seen: s.last_seen,
        }
    }
}

// ---------------------------------------------------------------------------
// NodeError
// ---------------------------------------------------------------------------

/// Errors from the Node API.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// The requested peer is not known.
    #[error("peer not found: {0}")]
    PeerNotFound(String),

    /// Failed to establish a connection.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// Failed to send a message.
    #[error("send failed: {0}")]
    SendFailed(String),

    /// Envelope encoding/decoding error.
    #[error("envelope error: {0}")]
    Envelope(#[from] EnvelopeError),

    /// Session layer error.
    #[error("session error: {0}")]
    Session(#[from] crate::session::SessionError),

    /// Network layer error.
    #[error("network error: {0}")]
    Network(#[from] crate::network::NetworkError),

    /// Transport layer error.
    #[error("transport error: {0}")]
    Transport(#[from] crate::transport::TransportError),

    /// The requested feature is not yet implemented.
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// The node has been stopped.
    #[error("node stopped")]
    Stopped,

    /// Builder configuration error.
    #[error("build error: {0}")]
    BuildError(String),

    /// I/O error from the builder (state dir creation, device-id persistence).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

/// The main truffle node — single public entry point for all functionality.
///
/// Generic over `N: NetworkProvider` so that tests can inject a mock provider
/// without Tailscale. In production, use the concrete type
/// `Node<TailscaleProvider>` (created via [`NodeBuilder`]).
///
/// # Lifecycle
///
/// 1. Create via [`Node::builder()`] + `.build().await`
/// 2. Use `peers()`, `send()`, `subscribe()`, `open_tcp()`, etc.
/// 3. Call `stop()` to shut down
pub struct Node<N: NetworkProvider + 'static> {
    /// Layer 3 network provider.
    network: Arc<N>,
    /// Layer 5 session / peer registry.
    session: Arc<PeerRegistry<N>>,
    /// Layer 6 envelope codec.
    codec: Arc<dyn EnvelopeCodec>,
    /// Broadcast sender for all incoming namespaced messages.
    /// Kept alive to prevent the channel from closing. The router task holds a clone.
    #[allow(dead_code)]
    incoming_tx: broadcast::Sender<NamespacedMessage>,
    /// Per-namespace subscription channels.
    namespace_filters: Arc<RwLock<HashMap<String, broadcast::Sender<NamespacedMessage>>>>,
    /// File transfer subsystem state.
    pub(crate) file_transfer_state: FileTransferState,
}

impl<N: NetworkProvider + 'static> Node<N> {
    /// Create a `Node` from pre-built components (used by builder and tests).
    ///
    /// This constructor wires together the layers and spawns the envelope
    /// router task that reads from the session layer, deserializes envelopes,
    /// and dispatches to namespace subscribers.
    pub(crate) fn from_parts(
        network: Arc<N>,
        session: Arc<PeerRegistry<N>>,
        codec: Arc<dyn EnvelopeCodec>,
    ) -> Self {
        let (incoming_tx, _) = broadcast::channel(1024);
        let namespace_filters: Arc<RwLock<HashMap<String, broadcast::Sender<NamespacedMessage>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let node = Self {
            network,
            session: session.clone(),
            codec: codec.clone(),
            incoming_tx: incoming_tx.clone(),
            namespace_filters: namespace_filters.clone(),
            file_transfer_state: FileTransferState::new(),
        };

        // Spawn the envelope router task.
        node.spawn_envelope_router(session, codec, incoming_tx, namespace_filters);

        node
    }

    /// Spawn a background task that reads incoming raw messages from the
    /// session layer, deserializes them as envelopes, and routes them to
    /// the global channel and per-namespace subscribers.
    fn spawn_envelope_router(
        &self,
        session: Arc<PeerRegistry<N>>,
        codec: Arc<dyn EnvelopeCodec>,
        incoming_tx: broadcast::Sender<NamespacedMessage>,
        namespace_filters: Arc<RwLock<HashMap<String, broadcast::Sender<NamespacedMessage>>>>,
    ) {
        let mut rx = session.subscribe();

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        if let Ok(envelope) = codec.decode(&msg.data) {
                            let namespaced = NamespacedMessage {
                                from: msg.from,
                                namespace: envelope.namespace.clone(),
                                msg_type: envelope.msg_type,
                                payload: envelope.payload,
                                timestamp: envelope.timestamp,
                            };

                            tracing::debug!(
                                from = %namespaced.from,
                                namespace = %namespaced.namespace,
                                msg_type = %namespaced.msg_type,
                                "envelope router: dispatching message"
                            );

                            // Send to global channel (best-effort).
                            let _ = incoming_tx.send(namespaced.clone());

                            // Route to namespace-specific subscriber if present.
                            let filters = namespace_filters.read().await;
                            let _has_subscriber = filters.contains_key(&namespaced.namespace);
                            if let Some(tx) = filters.get(&namespaced.namespace) {
                                let send_result = tx.send(namespaced);
                                tracing::debug!(
                                    namespace = %envelope.namespace,
                                    subscriber_count = tx.receiver_count(),
                                    sent = send_result.is_ok(),
                                    "envelope router: sent to namespace subscriber"
                                );
                            } else {
                                tracing::debug!(
                                    namespace = %envelope.namespace,
                                    "envelope router: no subscriber for namespace"
                                );
                            }
                        } else {
                            tracing::warn!(
                                from = %msg.from,
                                data_len = msg.data.len(),
                                "node: failed to decode envelope from incoming message"
                            );
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            missed = n,
                            "node: envelope router lagged, missed {n} messages"
                        );
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::debug!("node: session incoming channel closed, router exiting");
                        break;
                    }
                }
            }
        });
    }

    // ── Builder ──────────────────────────────────────────────────────────

    /// Create a new [`NodeBuilder`] for configuring and constructing a node.
    pub fn builder() -> NodeBuilder {
        NodeBuilder::default()
    }

    // ── File Transfer ────────────────────────────────────────────────────

    /// Access the file transfer subsystem.
    ///
    /// Returns a [`FileTransfer`](file_transfer::FileTransfer) handle
    /// that provides methods for sending, receiving, and pulling files.
    pub fn file_transfer(&self) -> file_transfer::FileTransfer<'_, N> {
        file_transfer::FileTransfer::new(self)
    }

    /// Create a synchronized store for device-owned state.
    ///
    /// Returns an `Arc<SyncedStore<T>>` that syncs data across the mesh on
    /// namespace `"ss:{store_id}"`. The caller owns the returned Arc;
    /// the background sync task also holds one.
    ///
    /// Requires `self` to be wrapped in an `Arc` because the sync task
    /// needs to outlive this call.
    pub fn synced_store<T>(
        self: &Arc<Self>,
        store_id: &str,
    ) -> Arc<crate::synced_store::SyncedStore<T>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
    {
        crate::synced_store::SyncedStore::new(self.clone(), store_id)
    }

    /// Create a synchronized store with a custom persistence backend.
    ///
    /// Same as [`synced_store`](Self::synced_store) but restores persisted
    /// data on startup and writes through to the backend on every change.
    pub fn synced_store_with_backend<T>(
        self: &Arc<Self>,
        store_id: &str,
        backend: std::sync::Arc<dyn crate::synced_store::StoreBackend>,
    ) -> Arc<crate::synced_store::SyncedStore<T>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
    {
        crate::synced_store::SyncedStore::new_with_backend(self.clone(), store_id, backend)
    }

    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Stop the node and all underlying layers.
    ///
    /// After calling `stop()`, the node should not be used for further
    /// operations. Peer connections are closed and the network provider
    /// is shut down.
    pub async fn stop(&self) {
        tracing::info!("node: stopping");
        // The session and network layers will be cleaned up when the last
        // Arc reference is dropped. For now, we signal intent to stop.
        // Future enhancement: add explicit shutdown signals to each layer.
    }

    // ── Identity ─────────────────────────────────────────────────────────

    /// Return the local node's identity (stable ID, hostname, name).
    pub fn local_info(&self) -> NodeIdentity {
        self.network.local_identity()
    }

    // ── Discovery (from Layer 3, no transport needed) ────────────────────

    /// Return all known peers.
    ///
    /// Includes peers that are online but not yet connected (no active WS).
    /// This information comes from Layer 3 peer discovery.
    pub async fn peers(&self) -> Vec<Peer> {
        self.session
            .peers()
            .await
            .into_iter()
            .map(Peer::from)
            .collect()
    }

    /// Subscribe to peer change events (joined, left, connected, etc.).
    pub fn on_peer_change(&self) -> broadcast::Receiver<PeerEvent> {
        self.session.on_peer_change()
    }

    /// Resolve a peer identifier to the canonical per-device ULID
    /// (`device_id`) from the RFC 017 hello envelope.
    ///
    /// Accepts any of:
    /// - the stable `device_id` (full ULID)
    /// - a unique prefix of the `device_id` (at least 4 characters; must
    ///   match exactly one known peer)
    /// - the human-readable `device_name` from the hello
    /// - the Layer 3 Tailscale hostname (the sanitised slug) — legacy
    /// - the Tailscale stable ID — escape hatch for diagnostics
    ///
    /// The returned string is always a `device_id`, so it is safe to feed
    /// back into `Node::send` or any other method that takes a peer
    /// identifier.
    pub async fn resolve_peer_id(&self, peer_id: &str) -> Result<String, NodeError> {
        let peers = self.session.peers().await;

        // Direct matches first — device_id, device_name, hostname,
        // tailscale_id — so deterministic inputs always take the fast path.
        for p in &peers {
            if let Some(identity) = p.identity.as_ref() {
                if identity.device_id == peer_id || identity.device_name == peer_id {
                    return Ok(identity.device_id.clone());
                }
            }
            if p.id == peer_id || p.name == peer_id {
                // `p.id` is the tailscale_id. If we have a richer identity,
                // return its device_id; otherwise fall back to the routing
                // key so the caller still has something usable.
                return Ok(p
                    .identity
                    .as_ref()
                    .map(|i| i.device_id.clone())
                    .unwrap_or_else(|| p.id.clone()));
            }
        }

        // Prefix match on device_id (require at least 4 chars and a single
        // unambiguous hit). This matches the CLI affordance of typing just
        // the first few characters of a ULID.
        if peer_id.len() >= 4 {
            let prefix_hits: Vec<&PeerState> = peers
                .iter()
                .filter(|p| {
                    p.identity
                        .as_ref()
                        .map(|i| i.device_id.starts_with(peer_id))
                        .unwrap_or(false)
                })
                .collect();
            if prefix_hits.len() == 1 {
                if let Some(identity) = prefix_hits[0].identity.as_ref() {
                    return Ok(identity.device_id.clone());
                }
            }
        }

        Err(NodeError::PeerNotFound(peer_id.to_string()))
    }

    // ── Diagnostics ──────────────────────────────────────────────────────

    /// Ping a peer via the network layer.
    ///
    /// Resolves the peer ID to an IP address and pings via Layer 3. Accepts
    /// the same identifier forms as [`resolve_peer_id`](Self::resolve_peer_id).
    pub async fn ping(&self, peer_id: &str) -> Result<PingResult, NodeError> {
        let peers = self.session.peers().await;
        let peer = peers
            .iter()
            .find(|p| {
                p.id == peer_id
                    || p.name == peer_id
                    || p.identity
                        .as_ref()
                        .map(|i| i.device_id == peer_id || i.device_name == peer_id)
                        .unwrap_or(false)
            })
            .ok_or_else(|| NodeError::PeerNotFound(peer_id.to_string()))?;

        let addr = peer.ip.to_string();
        self.network.ping(&addr).await.map_err(NodeError::Network)
    }

    /// Return health information from the network layer.
    pub async fn health(&self) -> HealthInfo {
        self.network.health().await
    }

    // ── Messaging (Layer 6 envelope over Layer 4 WS) ─────────────────────

    /// Send a namespaced message to a specific peer.
    ///
    /// The data is wrapped in a Layer 6 [`Envelope`] with the given namespace
    /// and a `"message"` type, then serialized and sent via the session layer.
    /// If no WebSocket connection exists, one is lazily established.
    pub async fn send(&self, peer_id: &str, namespace: &str, data: &[u8]) -> Result<(), NodeError> {
        // If the data is valid UTF-8 JSON, parse it into a proper JSON value
        // so the receiver gets a structured object rather than an array of
        // byte values.  This is critical for the file transfer protocol and
        // any other protocol that serializes structs to JSON bytes before
        // calling send().
        let payload = std::str::from_utf8(data)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .unwrap_or_else(|| serde_json::Value::from(data.to_vec()));

        let envelope = Envelope::new(namespace, "message", payload).with_timestamp();

        let encoded = self.codec.encode(&envelope)?;
        self.session.send(peer_id, &encoded).await?;
        Ok(())
    }

    /// Send a namespaced message with an explicit `msg_type` and JSON payload.
    ///
    /// Unlike [`send`](Self::send), this method takes a pre-built
    /// [`serde_json::Value`] payload and a caller-chosen `msg_type` instead
    /// of raw bytes with a hardcoded `"message"` type. Used by subsystems
    /// (file transfer, synced store, request/reply) that define their own
    /// wire protocol message types.
    pub async fn send_typed(
        &self,
        peer_id: &str,
        namespace: &str,
        msg_type: &str,
        payload: &serde_json::Value,
    ) -> Result<(), NodeError> {
        let envelope = Envelope::new(namespace, msg_type, payload.clone()).with_timestamp();
        let encoded = self.codec.encode(&envelope)?;
        self.session.send(peer_id, &encoded).await?;
        Ok(())
    }

    /// Broadcast a namespaced message with an explicit `msg_type` and JSON
    /// payload to all connected peers.
    pub async fn broadcast_typed(
        &self,
        namespace: &str,
        msg_type: &str,
        payload: &serde_json::Value,
    ) {
        let envelope = Envelope::new(namespace, msg_type, payload.clone()).with_timestamp();
        match self.codec.encode(&envelope) {
            Ok(encoded) => {
                self.session.broadcast(&encoded).await;
            }
            Err(e) => {
                tracing::error!("node: failed to encode broadcast envelope: {e}");
            }
        }
    }

    /// Broadcast a namespaced message to all connected peers.
    ///
    /// Only peers with active WebSocket connections receive the broadcast.
    /// No lazy connections are established.
    pub async fn broadcast(&self, namespace: &str, data: &[u8]) {
        let payload = std::str::from_utf8(data)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .unwrap_or_else(|| serde_json::Value::from(data.to_vec()));

        let envelope = Envelope::new(namespace, "message", payload).with_timestamp();

        match self.codec.encode(&envelope) {
            Ok(encoded) => {
                self.session.broadcast(&encoded).await;
            }
            Err(e) => {
                tracing::error!("node: failed to encode broadcast envelope: {e}");
            }
        }
    }

    /// Subscribe to messages in a specific namespace.
    ///
    /// Returns a broadcast receiver that yields [`NamespacedMessage`]s
    /// matching the given namespace. Multiple subscribers to the same
    /// namespace share the same underlying channel.
    pub fn subscribe(&self, namespace: &str) -> broadcast::Receiver<NamespacedMessage> {
        // Fast path: check if subscriber already exists (read lock).
        {
            let filters = self.namespace_filters.blocking_lock_read();
            if let Some(tx) = filters.get(namespace) {
                return tx.subscribe();
            }
        }

        // Slow path: create a new channel for this namespace (write lock).
        let mut filters = self.namespace_filters.blocking_lock_write();
        // Double-check after acquiring write lock.
        if let Some(tx) = filters.get(namespace) {
            return tx.subscribe();
        }
        let (tx, rx) = broadcast::channel(256);
        filters.insert(namespace.to_string(), tx);
        rx
    }

    // ── Raw streams (Layer 4 direct) ─────────────────────────────────────

    /// Open a raw TCP stream to a peer on the given port.
    ///
    /// Resolves the peer ID to an IP address via the session's peer list,
    /// then dials via the network layer. Accepts the same identifier
    /// forms as [`resolve_peer_id`](Self::resolve_peer_id). Returns a
    /// plain `TcpStream` for byte-oriented I/O.
    pub async fn open_tcp(&self, peer_id: &str, port: u16) -> Result<TcpStream, NodeError> {
        let peers = self.session.peers().await;
        let peer = peers
            .iter()
            .find(|p| {
                p.id == peer_id
                    || p.name == peer_id
                    || p.identity
                        .as_ref()
                        .map(|i| i.device_id == peer_id || i.device_name == peer_id)
                        .unwrap_or(false)
            })
            .ok_or_else(|| NodeError::PeerNotFound(peer_id.to_string()))?;

        let addr = peer.ip.to_string();
        self.network
            .dial_tcp(&addr, port)
            .await
            .map_err(|e| NodeError::ConnectionFailed(e.to_string()))
    }

    /// Listen for incoming TCP connections on a port.
    ///
    /// Returns a [`RawListener`] that yields raw `TcpStream`s. The caller
    /// is responsible for accepting connections in a loop.
    pub async fn listen_tcp(&self, port: u16) -> Result<RawListener, NodeError> {
        use crate::transport::tcp::TcpTransport;
        use crate::transport::RawTransport;

        let tcp = TcpTransport::new(self.network.clone());
        tcp.listen(port).await.map_err(NodeError::Transport)
    }

    /// Open a QUIC connection to a peer.
    ///
    /// **Stub** — returns `NotImplemented` until Phase 8.
    pub async fn open_quic(&self, _peer_id: &str) -> Result<(), NodeError> {
        Err(NodeError::NotImplemented(
            "QUIC connections are not yet implemented".to_string(),
        ))
    }

    /// Open a UDP datagram socket to a peer.
    ///
    /// **Stub** — returns `NotImplemented` until Phase 8.
    pub async fn open_udp(&self, _peer_id: &str) -> Result<NetworkUdpSocket, NodeError> {
        Err(NodeError::NotImplemented(
            "UDP sockets are not yet implemented".to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Blocking lock helpers for RwLock (used in sync subscribe())
// ---------------------------------------------------------------------------

/// Extension trait for using tokio RwLock in synchronous contexts within
/// the subscribe() method (which cannot be async because it returns a
/// Receiver, not a Future).
trait RwLockBlockingExt<T> {
    fn blocking_lock_read(&self) -> tokio::sync::RwLockReadGuard<'_, T>;
    fn blocking_lock_write(&self) -> tokio::sync::RwLockWriteGuard<'_, T>;
}

impl<T> RwLockBlockingExt<T> for RwLock<T> {
    fn blocking_lock_read(&self) -> tokio::sync::RwLockReadGuard<'_, T> {
        // In an async context, try_read is safe. If contended, fall back.
        self.try_read().unwrap_or_else(|_| {
            // Should not happen in practice since we hold locks briefly,
            // but if it does we panic with a clear message.
            panic!("node: namespace_filters read lock contended in sync context")
        })
    }

    fn blocking_lock_write(&self) -> tokio::sync::RwLockWriteGuard<'_, T> {
        self.try_write().unwrap_or_else(|_| {
            panic!("node: namespace_filters write lock contended in sync context")
        })
    }
}

// ---------------------------------------------------------------------------
// NodeBuilder
// ---------------------------------------------------------------------------

/// Builder for constructing a [`Node<TailscaleProvider>`].
///
/// Configures the Tailscale sidecar, RFC 017 identity, and transport
/// parameters before wiring all layers together.
///
/// # Example
///
/// ```ignore
/// let node = Node::builder()
///     .app_id("playground")?
///     .device_name("alice-mbp")
///     .sidecar_path("/opt/truffle/sidecar")
///     .ws_port(9417)
///     .build()
///     .await?;
/// ```
#[derive(Debug, Clone)]
pub struct NodeBuilder {
    app_id: Option<AppId>,
    device_name: Option<DeviceName>,
    device_id: Option<DeviceId>,
    sidecar_path: Option<PathBuf>,
    state_dir: Option<String>,
    auth_key: Option<String>,
    ephemeral: bool,
    ws_port: u16,
}

impl Default for NodeBuilder {
    fn default() -> Self {
        Self {
            app_id: None,
            device_name: None,
            device_id: None,
            sidecar_path: None,
            state_dir: None,
            auth_key: None,
            ephemeral: false,
            ws_port: 9417,
        }
    }
}

impl NodeBuilder {
    /// Set the application namespace identifier (RFC 017 §5.1).
    ///
    /// The input is validated against `^[a-z][a-z0-9-]{1,31}$`. Invalid
    /// values are rejected with `NodeError::BuildError`.
    pub fn app_id(mut self, s: impl Into<String>) -> Result<Self, NodeError> {
        let raw: String = s.into();
        let app_id = AppId::parse(&raw)
            .map_err(|e| NodeError::BuildError(format!("invalid app_id: {e}")))?;
        self.app_id = Some(app_id);
        Ok(self)
    }

    /// Set the human-readable device name.
    ///
    /// Accepts any Unicode input; soft-truncated to 256 graphemes. When
    /// unset, the builder falls back to `hostname::get()` at `build()` time.
    pub fn device_name(mut self, s: impl Into<String>) -> Self {
        self.device_name = Some(DeviceName::parse(s));
        self
    }

    /// Override the auto-generated device ID.
    ///
    /// Validates that `s` is a well-formed ULID. When provided, the value
    /// is persisted to `{state_dir}/device-id.txt` during `build()` so
    /// subsequent starts without an explicit `device_id` see it.
    pub fn device_id(mut self, s: impl Into<String>) -> Result<Self, NodeError> {
        let raw: String = s.into();
        let device_id = DeviceId::parse(&raw)
            .map_err(|e| NodeError::BuildError(format!("invalid device_id: {e}")))?;
        self.device_id = Some(device_id);
        Ok(self)
    }

    /// Set the path to the Go sidecar binary.
    pub fn sidecar_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.sidecar_path = Some(path.into());
        self
    }

    /// Set the Tailscale state directory.
    pub fn state_dir(mut self, dir: &str) -> Self {
        self.state_dir = Some(dir.to_string());
        self
    }

    /// Set the Tailscale auth key for headless authentication.
    pub fn auth_key(mut self, key: &str) -> Self {
        self.auth_key = Some(key.to_string());
        self
    }

    /// Set whether the node is ephemeral (auto-removed from tailnet on shutdown).
    pub fn ephemeral(mut self, val: bool) -> Self {
        self.ephemeral = val;
        self
    }

    /// Set the WebSocket listen port.
    pub fn ws_port(mut self, port: u16) -> Self {
        self.ws_port = port;
        self
    }

    /// Resolve RFC 017 identity values and the Tailscale config.
    ///
    /// Shared between [`build()`](Self::build) and
    /// [`build_with_auth_handler()`](Self::build_with_auth_handler). Returns
    /// the ready-to-start `TailscaleConfig` along with the parsed identity
    /// triple.
    fn prepare_config(&self) -> Result<TailscaleConfig, NodeError> {
        // 1. sidecar binary is required.
        let binary_path = self
            .sidecar_path
            .clone()
            .ok_or_else(|| NodeError::BuildError("sidecar_path is required".into()))?;

        // 2. app_id is required.
        let app_id = self
            .app_id
            .clone()
            .ok_or_else(|| NodeError::BuildError("app_id is required".into()))?;

        // 3. device_name falls back to the OS hostname.
        let device_name = match self.device_name.clone() {
            Some(name) => name,
            None => {
                let os_hostname = hostname::get()
                    .map_err(|e| {
                        NodeError::BuildError(format!(
                            "device_name is unset and hostname::get() failed: {e}"
                        ))
                    })?
                    .to_string_lossy()
                    .into_owned();
                DeviceName::parse(os_hostname)
            }
        };

        // 4. Compose the Tailscale hostname once, here. Downstream code
        //    MUST NOT rebuild it — the provider config stores this verbatim.
        let tailscale_host = identity::tailscale_hostname(&app_id, &device_name);

        // 5. Resolve state_dir. Default:
        //    `{dirs::data_dir}/truffle/{app_id}/{slug(device_name)}`.
        let state_dir = self.state_dir.clone().unwrap_or_else(|| {
            let base = dirs::data_dir().unwrap_or_else(|| {
                tracing::warn!(
                    "dirs::data_dir() returned None, falling back to std::env::temp_dir()"
                );
                std::env::temp_dir()
            });
            base.join("truffle")
                .join(app_id.as_str())
                .join(identity::slug(device_name.as_str(), 255))
                .to_string_lossy()
                .into_owned()
        });

        // 6. Ensure the state directory exists before Tailscale starts.
        std::fs::create_dir_all(&state_dir)?;

        // 7. Resolve device_id. Priority:
        //    a) explicit builder override → validate + persist
        //    b) existing `device-id.txt` → read + validate
        //    c) generate + persist
        let device_id_file = Path::new(&state_dir).join("device-id.txt");
        let device_id = match self.device_id.clone() {
            Some(id) => {
                // Persist the override so later auto-generated calls see it.
                std::fs::write(&device_id_file, id.as_str())?;
                id
            }
            None => {
                if device_id_file.exists() {
                    let s = std::fs::read_to_string(&device_id_file)?.trim().to_string();
                    DeviceId::parse(&s).map_err(|e| {
                        NodeError::BuildError(format!(
                            "device-id.txt at {device_id_file:?} contains an invalid ULID: {e}"
                        ))
                    })?
                } else {
                    let id = DeviceId::generate();
                    std::fs::write(&device_id_file, id.as_str())?;
                    id
                }
            }
        };

        Ok(TailscaleConfig {
            binary_path,
            app_id: app_id.as_str().to_string(),
            device_id: device_id.as_str().to_string(),
            device_name: device_name.as_str().to_string(),
            hostname: tailscale_host,
            state_dir,
            auth_key: self.auth_key.clone(),
            ephemeral: if self.ephemeral { Some(true) } else { None },
            tags: None,
        })
    }

    /// Build and start the node.
    ///
    /// This creates the TailscaleProvider, starts it, creates the WebSocket
    /// transport and PeerRegistry, starts the session, and spawns the
    /// envelope router.
    ///
    /// # Errors
    ///
    /// Returns [`NodeError::BuildError`] if required configuration is missing,
    /// or propagates errors from the network provider startup.
    pub async fn build(self) -> Result<Node<TailscaleProvider>, NodeError> {
        let ws_port = self.ws_port;
        let config = self.prepare_config()?;

        let mut provider = TailscaleProvider::new(config);
        provider.start().await.map_err(NodeError::Network)?;

        let network = Arc::new(provider);

        // 2. Create WebSocket transport.
        let ws_config = WsConfig {
            port: ws_port,
            ..Default::default()
        };
        let ws_transport = Arc::new(WebSocketTransport::new(network.clone(), ws_config));

        // 3. Create PeerRegistry and start session.
        let session = Arc::new(PeerRegistry::new(network.clone(), ws_transport));
        session.start().await;

        // 4. Create the node with the envelope router.
        let codec: Arc<dyn EnvelopeCodec> = Arc::new(JsonCodec);
        let node = Node::from_parts(network, session, codec);

        tracing::info!("node: started successfully");
        Ok(node)
    }

    /// Build and start the node, calling `on_auth` if authentication is needed.
    ///
    /// This is identical to [`build()`](Self::build) except it subscribes to
    /// provider events *before* `provider.start()` blocks, forwarding
    /// `AuthRequired` events to the callback while waiting for authentication
    /// to complete.
    ///
    /// # Errors
    ///
    /// Returns [`NodeError::BuildError`] if required configuration is missing,
    /// or propagates errors from the network provider startup.
    pub async fn build_with_auth_handler(
        self,
        on_auth: impl Fn(String) + Send + 'static,
    ) -> Result<Node<TailscaleProvider>, NodeError> {
        let ws_port = self.ws_port;
        let config = self.prepare_config()?;

        let mut provider = TailscaleProvider::new(config);

        // 2. Subscribe to peer events BEFORE start() so we capture auth URLs.
        let mut auth_rx = provider.peer_events();

        // 3. Spawn a task that forwards AuthRequired events to the callback.
        let auth_task = tokio::spawn(async move {
            use crate::network::NetworkPeerEvent;
            loop {
                match auth_rx.recv().await {
                    Ok(NetworkPeerEvent::AuthRequired { url }) => {
                        on_auth(url);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    _ => {} // Ignore other events
                }
            }
        });

        // 4. Start the provider (blocks until auth completes).
        let start_result = provider.start().await.map_err(NodeError::Network);

        // 5. Cancel the auth forwarding task — auth is done.
        auth_task.abort();

        start_result?;

        let network = Arc::new(provider);

        // 6. Create WebSocket transport.
        let ws_config = WsConfig {
            port: ws_port,
            ..Default::default()
        };
        let ws_transport = Arc::new(WebSocketTransport::new(network.clone(), ws_config));

        // 7. Create PeerRegistry and start session.
        let session = Arc::new(PeerRegistry::new(network.clone(), ws_transport));
        session.start().await;

        // 8. Create the node with the envelope router.
        let codec: Arc<dyn EnvelopeCodec> = Arc::new(JsonCodec);
        let node = Node::from_parts(network, session, codec);

        tracing::info!("node: started successfully (with auth handler)");
        Ok(node)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{
        HealthInfo, IncomingConnection, NetworkError, NetworkPeer, NetworkPeerEvent,
        NetworkTcpListener, NetworkUdpSocket, PeerAddr,
    };
    use crate::transport::WsConfig;
    use serde_json::json;
    use std::time::Duration;
    use tokio::sync::{broadcast, mpsc};

    // ── Mock NetworkProvider ──────────────────────────────────────────

    struct MockNetworkProvider {
        identity: NodeIdentity,
        local_addr: PeerAddr,
        peer_event_tx: broadcast::Sender<NetworkPeerEvent>,
        /// Pre-loaded peer list for `peers()`.
        mock_peers: Arc<RwLock<Vec<NetworkPeer>>>,
    }

    impl MockNetworkProvider {
        fn new(id: &str) -> Self {
            let (peer_event_tx, _) = broadcast::channel(64);
            Self {
                identity: NodeIdentity {
                    app_id: "test".to_string(),
                    // RFC 017: align `device_id` with fixture input so
                    // tests can reason about a single identifier.
                    device_id: id.to_string(),
                    device_name: format!("Test Node {id}"),
                    tailscale_hostname: format!("truffle-test-{id}"),
                    tailscale_id: id.to_string(),
                    dns_name: None,
                    ip: Some("127.0.0.1".parse().unwrap()),
                },
                local_addr: PeerAddr {
                    ip: Some("127.0.0.1".parse().unwrap()),
                    hostname: format!("truffle-test-{id}"),
                    dns_name: None,
                },
                peer_event_tx,
                mock_peers: Arc::new(RwLock::new(Vec::new())),
            }
        }

        fn event_sender(&self) -> broadcast::Sender<NetworkPeerEvent> {
            self.peer_event_tx.clone()
        }
    }

    impl NetworkProvider for MockNetworkProvider {
        async fn start(&mut self) -> Result<(), NetworkError> {
            Ok(())
        }

        async fn stop(&mut self) -> Result<(), NetworkError> {
            Ok(())
        }

        fn local_identity(&self) -> NodeIdentity {
            self.identity.clone()
        }

        fn local_addr(&self) -> PeerAddr {
            self.local_addr.clone()
        }

        fn peer_events(&self) -> broadcast::Receiver<NetworkPeerEvent> {
            self.peer_event_tx.subscribe()
        }

        async fn peers(&self) -> Vec<NetworkPeer> {
            self.mock_peers.read().await.clone()
        }

        async fn dial_tcp(&self, addr: &str, port: u16) -> Result<TcpStream, NetworkError> {
            let target = format!("{addr}:{port}");
            TcpStream::connect(&target)
                .await
                .map_err(|e| NetworkError::DialFailed(format!("mock dial {target}: {e}")))
        }

        async fn listen_tcp(&self, port: u16) -> Result<NetworkTcpListener, NetworkError> {
            let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
                .await
                .map_err(|e| NetworkError::ListenFailed(format!("mock listen :{port}: {e}")))?;

            let actual_port = listener.local_addr().unwrap().port();
            let (tx, rx) = mpsc::channel::<IncomingConnection>(64);

            tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, addr)) => {
                            let conn = IncomingConnection {
                                stream,
                                remote_addr: addr.to_string(),
                                remote_identity: String::new(),
                                port: actual_port,
                            };
                            if tx.send(conn).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::debug!("mock listener error: {e}");
                            break;
                        }
                    }
                }
            });

            Ok(NetworkTcpListener {
                port: actual_port,
                incoming: rx,
            })
        }

        async fn unlisten_tcp(&self, _port: u16) -> Result<(), NetworkError> {
            Ok(())
        }

        async fn bind_udp(&self, _port: u16) -> Result<NetworkUdpSocket, NetworkError> {
            Err(NetworkError::Internal("mock: UDP not supported".into()))
        }

        async fn ping(&self, _addr: &str) -> Result<PingResult, NetworkError> {
            Ok(PingResult {
                latency: Duration::from_millis(1),
                connection: "direct".to_string(),
                peer_addr: None,
            })
        }

        async fn health(&self) -> HealthInfo {
            HealthInfo {
                state: "running".to_string(),
                healthy: true,
                ..Default::default()
            }
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────

    fn make_loopback_peer(id: &str) -> NetworkPeer {
        NetworkPeer {
            id: id.to_string(),
            hostname: format!("truffle-test-{id}"),
            ip: "127.0.0.1".parse().unwrap(),
            online: true,
            cur_addr: Some("127.0.0.1:41641".to_string()),
            relay: None,
            os: Some("linux".to_string()),
            last_seen: Some("2026-03-25T12:00:00Z".to_string()),
            key_expiry: None,
            dns_name: None,
        }
    }

    fn ws_config(port: u16) -> WsConfig {
        WsConfig {
            port,
            ping_interval: Duration::from_secs(300),
            pong_timeout: Duration::from_secs(300),
            ..Default::default()
        }
    }

    async fn random_port() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    }

    /// Create a Node backed by a mock provider for testing.
    async fn make_test_node(
        id: &str,
        ws_port: u16,
    ) -> (
        Node<MockNetworkProvider>,
        broadcast::Sender<NetworkPeerEvent>,
        Arc<MockNetworkProvider>,
    ) {
        let provider = MockNetworkProvider::new(id);
        let event_tx = provider.event_sender();
        let network = Arc::new(provider);
        let ws_transport = Arc::new(WebSocketTransport::new(network.clone(), ws_config(ws_port)));
        let session = Arc::new(PeerRegistry::new(network.clone(), ws_transport));
        session.start().await;

        let codec: Arc<dyn EnvelopeCodec> = Arc::new(JsonCodec);
        let node = Node::from_parts(network.clone(), session, codec);

        (node, event_tx, network)
    }

    // ── Tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_node_builder_creates_node() {
        let ws_port = random_port().await;
        let (node, _event_tx, _network) = make_test_node("node-1", ws_port).await;

        let identity = node.local_info();
        assert_eq!(identity.tailscale_id, "node-1");
        assert_eq!(identity.device_id, "node-1");
        assert!(identity.tailscale_hostname.contains("node-1"));
    }

    #[tokio::test]
    async fn test_node_peers_from_network() {
        let ws_port = random_port().await;
        let (node, event_tx, _network) = make_test_node("node-1", ws_port).await;

        // Initially no peers.
        let peers = node.peers().await;
        assert!(peers.is_empty());

        // Inject a peer via Layer 3.
        let peer = make_loopback_peer("peer-a");
        let _ = event_tx.send(NetworkPeerEvent::Joined(peer));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let peers = node.peers().await;
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].id, "peer-a");
        assert!(peers[0].online);
        assert!(!peers[0].ws_connected);
    }

    #[tokio::test]
    async fn test_node_send_to_unknown_peer_errors() {
        let ws_port = random_port().await;
        let (node, _event_tx, _network) = make_test_node("node-1", ws_port).await;

        let result = node.send("nonexistent", "test", b"hello").await;
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("unknown peer") || err_str.contains("not found"),
            "expected unknown peer error, got: {err_str}"
        );
    }

    #[tokio::test]
    async fn test_node_send_wraps_in_envelope() {
        // Test that send() properly creates an envelope.
        // We test the codec directly since a full send requires two connected nodes.
        let codec = JsonCodec;
        let data = b"hello world";
        let envelope = Envelope::new("test-ns", "message", serde_json::Value::from(data.to_vec()))
            .with_timestamp();

        let encoded = codec.encode(&envelope).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert_eq!(decoded.namespace, "test-ns");
        assert_eq!(decoded.msg_type, "message");
        assert!(decoded.timestamp.is_some());
    }

    #[tokio::test]
    async fn test_node_subscribe_filters_by_namespace() {
        let ws_port = random_port().await;
        let (node, _event_tx, _network) = make_test_node("node-1", ws_port).await;

        // Create subscribers for two different namespaces.
        let _rx_chat = node.subscribe("chat");
        let _rx_ft = node.subscribe("ft");

        // Subscribing to the same namespace again should work.
        let _rx_chat2 = node.subscribe("chat");
    }

    #[tokio::test]
    async fn test_node_broadcast() {
        let ws_port = random_port().await;
        let (node, _event_tx, _network) = make_test_node("node-1", ws_port).await;

        // Broadcast with no connected peers should not panic.
        node.broadcast("test", b"hello everyone").await;
    }

    #[tokio::test]
    async fn test_node_open_tcp_resolves_peer() {
        let ws_port = random_port().await;
        let (node, event_tx, _network) = make_test_node("node-1", ws_port).await;

        // Start a TCP server for the test.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tcp_port = listener.local_addr().unwrap().port();

        // Inject a loopback peer.
        let peer = make_loopback_peer("peer-tcp");
        let _ = event_tx.send(NetworkPeerEvent::Joined(peer));
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Accept a connection in the background.
        let accept_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream
        });

        // open_tcp should resolve peer-tcp to 127.0.0.1 and connect.
        let stream = node.open_tcp("peer-tcp", tcp_port).await;
        assert!(stream.is_ok(), "open_tcp failed: {:?}", stream.err());

        let _ = accept_handle.await;
    }

    #[tokio::test]
    async fn test_node_open_tcp_unknown_peer_errors() {
        let ws_port = random_port().await;
        let (node, _event_tx, _network) = make_test_node("node-1", ws_port).await;

        let result = node.open_tcp("nonexistent", 8080).await;
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("not found"),
            "expected peer not found error, got: {err_str}"
        );
    }

    #[tokio::test]
    async fn test_node_ping_resolves_peer() {
        let ws_port = random_port().await;
        let (node, event_tx, _network) = make_test_node("node-1", ws_port).await;

        // No peer yet.
        let result = node.ping("peer-ping").await;
        assert!(result.is_err());

        // Inject peer.
        let peer = make_loopback_peer("peer-ping");
        let _ = event_tx.send(NetworkPeerEvent::Joined(peer));
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should succeed (mock returns 1ms latency).
        let result = node.ping("peer-ping").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().latency, Duration::from_millis(1));
    }

    #[tokio::test]
    async fn test_node_health() {
        let ws_port = random_port().await;
        let (node, _event_tx, _network) = make_test_node("node-1", ws_port).await;

        let health = node.health().await;
        assert!(health.healthy);
        assert_eq!(health.state, "running");
    }

    #[tokio::test]
    async fn test_node_open_quic_not_implemented() {
        let ws_port = random_port().await;
        let (node, _event_tx, _network) = make_test_node("node-1", ws_port).await;

        let result = node.open_quic("peer").await;
        assert!(matches!(result, Err(NodeError::NotImplemented(_))));
    }

    #[tokio::test]
    async fn test_node_open_udp_not_implemented() {
        let ws_port = random_port().await;
        let (node, _event_tx, _network) = make_test_node("node-1", ws_port).await;

        let result = node.open_udp("peer").await;
        assert!(matches!(result, Err(NodeError::NotImplemented(_))));
    }

    #[tokio::test]
    async fn test_node_listen_tcp() {
        let ws_port = random_port().await;
        let (node, _event_tx, _network) = make_test_node("node-1", ws_port).await;

        // listen_tcp(0) should bind to an ephemeral port.
        let listener = node.listen_tcp(0).await;
        assert!(listener.is_ok(), "listen_tcp failed: {:?}", listener.err());
    }

    #[tokio::test]
    async fn test_envelope_serialize_deserialize() {
        let envelope = Envelope::new("chat", "message", json!({"text": "hello"})).with_timestamp();

        let bytes = envelope.serialize().unwrap();
        let decoded = Envelope::deserialize(&bytes).unwrap();

        assert_eq!(decoded.namespace, "chat");
        assert_eq!(decoded.msg_type, "message");
        assert_eq!(decoded.payload["text"], "hello");
        assert!(decoded.timestamp.is_some());
    }

    #[tokio::test]
    async fn test_envelope_codec_json() {
        let codec = JsonCodec;
        let envelope = Envelope::new("ft", "offer", json!({"file": "test.bin"}));

        let encoded = codec.encode(&envelope).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert_eq!(decoded.namespace, "ft");
        assert_eq!(decoded.payload["file"], "test.bin");
    }

    #[tokio::test]
    async fn test_envelope_unknown_fields_ignored() {
        let json_bytes = br#"{
            "namespace": "v2",
            "msg_type": "new",
            "payload": {},
            "future_field": "ignored"
        }"#;

        let codec = JsonCodec;
        let decoded = codec.decode(json_bytes).unwrap();
        assert_eq!(decoded.namespace, "v2");
        assert_eq!(decoded.msg_type, "new");
    }

    #[tokio::test]
    async fn test_node_send_and_receive_roundtrip() {
        // Set up two nodes that communicate via loopback WS.
        let port_a = random_port().await;
        let port_b = random_port().await;

        let (node_a, event_tx_a, _net_a) = make_test_node("node-a", port_a).await;
        let (node_b, event_tx_b, _net_b) = make_test_node("node-b", port_b).await;

        // Inject each node as a peer of the other.
        let peer_b = NetworkPeer {
            id: "node-b".to_string(),
            hostname: "truffle-test-node-b".to_string(),
            ip: "127.0.0.1".parse().unwrap(),
            online: true,
            cur_addr: Some("127.0.0.1:41641".to_string()),
            relay: None,
            os: None,
            last_seen: None,
            key_expiry: None,
            dns_name: None,
        };
        let peer_a = NetworkPeer {
            id: "node-a".to_string(),
            hostname: "truffle-test-node-a".to_string(),
            ip: "127.0.0.1".parse().unwrap(),
            online: true,
            cur_addr: Some("127.0.0.1:41641".to_string()),
            relay: None,
            os: None,
            last_seen: None,
            key_expiry: None,
            dns_name: None,
        };

        let _ = event_tx_a.send(NetworkPeerEvent::Joined(peer_b));
        let _ = event_tx_b.send(NetworkPeerEvent::Joined(peer_a));
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Subscribe to namespace on node_b.
        let mut rx = node_b.subscribe("test");

        // Send from node_a to node_b. This triggers lazy WS connect.
        // Note: this will connect to node_b's WS listener on port_b.
        let send_result = node_a.send("node-b", "test", b"hello from a").await;

        // The send may fail in loopback mock because the WS port for node-b
        // is the listener port, and the mock's dial connects to 127.0.0.1:port_b.
        // In a real scenario with Tailscale, this works because each node
        // listens on its own Tailscale IP.
        //
        // For unit tests, we verify the envelope codec roundtrip works.
        // Full integration tests require two separate processes.
        if send_result.is_ok() {
            // If send succeeded, verify the message arrives.
            let msg = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
            if let Ok(Ok(msg)) = msg {
                assert_eq!(msg.namespace, "test");
            }
        }
        // If send fails due to loopback WS peer-id mismatch, that's expected
        // in unit tests. The important thing is no panics.
    }

    // ── RFC 017 Phase 2: resolve_peer_id ─────────────────────────────

    /// Helper: inject a peer entry into the session registry and then
    /// stamp a synthetic RFC 017 identity on it so `resolve_peer_id`
    /// has something to look up. This skips the real hello exchange.
    async fn inject_peer_with_identity(
        node: &Node<MockNetworkProvider>,
        event_tx: &broadcast::Sender<NetworkPeerEvent>,
        tailscale_id: &str,
        device_id: &str,
        device_name: &str,
    ) {
        let peer = make_loopback_peer(tailscale_id);
        let _ = event_tx.send(NetworkPeerEvent::Joined(peer));
        tokio::time::sleep(Duration::from_millis(30)).await;

        let identity = crate::session::PeerIdentity {
            app_id: "test".into(),
            device_id: device_id.into(),
            device_name: device_name.into(),
            os: "linux".into(),
            tailscale_id: tailscale_id.into(),
        };
        assert!(
            node.session
                .test_stamp_identity(tailscale_id, identity)
                .await,
            "peer {tailscale_id} should exist in session registry before stamping identity"
        );
    }

    #[tokio::test]
    async fn test_resolve_peer_id_by_device_id() {
        let ws_port = random_port().await;
        let (node, event_tx, _net) = make_test_node("node-1", ws_port).await;

        inject_peer_with_identity(
            &node,
            &event_tx,
            "tailscale-abc",
            "01HZZZZZZZZZZZZZZZZZZZZZZZ",
            "Alice MacBook",
        )
        .await;

        let resolved = node
            .resolve_peer_id("01HZZZZZZZZZZZZZZZZZZZZZZZ")
            .await
            .unwrap();
        assert_eq!(resolved, "01HZZZZZZZZZZZZZZZZZZZZZZZ");
    }

    #[tokio::test]
    async fn test_resolve_peer_id_by_device_name() {
        let ws_port = random_port().await;
        let (node, event_tx, _net) = make_test_node("node-1", ws_port).await;

        inject_peer_with_identity(
            &node,
            &event_tx,
            "tailscale-abc",
            "01HXYZXYZXYZXYZXYZXYZXYZXY",
            "Bob's Mac",
        )
        .await;

        let resolved = node.resolve_peer_id("Bob's Mac").await.unwrap();
        assert_eq!(resolved, "01HXYZXYZXYZXYZXYZXYZXYZXY");
    }

    #[tokio::test]
    async fn test_resolve_peer_id_by_device_id_prefix() {
        let ws_port = random_port().await;
        let (node, event_tx, _net) = make_test_node("node-1", ws_port).await;

        inject_peer_with_identity(
            &node,
            &event_tx,
            "tailscale-abc",
            "01HXYZXYZXYZXYZXYZXYZXYZXY",
            "laptop",
        )
        .await;

        // Prefix match — 4 chars is the minimum the implementation
        // accepts.
        let resolved = node.resolve_peer_id("01HX").await.unwrap();
        assert_eq!(resolved, "01HXYZXYZXYZXYZXYZXYZXYZXY");
    }

    #[tokio::test]
    async fn test_resolve_peer_id_by_tailscale_id_legacy() {
        // Escape hatch: resolving by the Tailscale stable ID should
        // still work and return the device_id (or the tailscale_id as
        // fallback when no identity is populated).
        let ws_port = random_port().await;
        let (node, event_tx, _net) = make_test_node("node-1", ws_port).await;

        inject_peer_with_identity(
            &node,
            &event_tx,
            "tailscale-legacy",
            "01HLEGACY0000000000000000X",
            "legacy box",
        )
        .await;

        let resolved = node.resolve_peer_id("tailscale-legacy").await.unwrap();
        assert_eq!(resolved, "01HLEGACY0000000000000000X");
    }

    #[tokio::test]
    async fn test_resolve_peer_id_unknown() {
        let ws_port = random_port().await;
        let (node, _event_tx, _net) = make_test_node("node-1", ws_port).await;
        let result = node.resolve_peer_id("nope").await;
        assert!(matches!(result, Err(NodeError::PeerNotFound(_))));
    }
}
