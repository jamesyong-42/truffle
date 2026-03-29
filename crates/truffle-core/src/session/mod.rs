//! Layer 5: Session — Peer identity, connection lifecycle, message routing.
//!
//! The [`PeerRegistry`] is the central component. It consumes peer discovery
//! events from Layer 3 ([`NetworkProvider`]) and manages transport connections
//! from Layer 4 ([`StreamTransport`], [`RawTransport`]).
//!
//! # Layer rules
//!
//! - Layer 5 does NOT know what the data means (no namespaces, no envelopes)
//! - Layer 5 does NOT inspect payloads
//! - Layer 5 does NOT do peer discovery — it consumes Layer 3 events
//! - Peers exist because Layer 3 says they exist, NOT because of connections
//! - Connections are lazy — established on first `send()`
//! - Layer 5 does NOT implement any transport protocol — it delegates to Layer 4

pub mod reconnect;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, RwLock};

use self::reconnect::ReconnectBackoff;

use crate::network::{NetworkPeer, NetworkPeerEvent, NetworkProvider, PeerAddr};
use crate::transport::websocket::{WebSocketTransport, WsFramedStream};
use crate::transport::{FramedStream, StreamTransport};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A peer's state in the session registry.
///
/// Combines Layer 3 network information (discovery, addressing) with
/// Layer 5 session state (connection status). Peers are added to the
/// registry when Layer 3 reports them, NOT when transport connections
/// are established.
#[derive(Debug, Clone)]
pub struct PeerState {
    /// Stable node ID from the network provider.
    pub id: String,
    /// Human-readable name (hostname from Layer 3).
    pub name: String,
    /// Network IP address.
    pub ip: IpAddr,
    /// Whether the peer is currently online (from Layer 3).
    pub online: bool,
    /// Whether the peer has an active WebSocket connection.
    pub connected: bool,
    /// Connection type description (e.g., "direct" or "relay:ord").
    pub connection_type: String,
    /// Operating system of the peer, if known.
    pub os: Option<String>,
    /// Last time the peer was seen online (RFC 3339 string).
    pub last_seen: Option<String>,
}

/// Events emitted by the session layer when peer state changes.
///
/// Subscribers receive these via [`PeerRegistry::on_peer_change`].
/// Events cover both Layer 3 discovery changes and Layer 5 connection
/// lifecycle changes.
#[derive(Debug, Clone)]
pub enum PeerEvent {
    /// A new peer appeared on the network (from Layer 3).
    Joined(PeerState),
    /// A peer left the network (by stable node ID, from Layer 3).
    Left(String),
    /// A peer's metadata changed (IP, relay, online status, from Layer 3).
    Updated(PeerState),
    /// A WebSocket connection was established to a peer (Layer 5).
    Connected(String),
    /// A WebSocket connection was lost to a peer (Layer 5).
    Disconnected(String),
    /// Authentication is required — the URL should be shown to the user.
    AuthRequired { url: String },
}

/// An incoming message received from a peer via WebSocket.
///
/// Layer 5 does not inspect or interpret the data — it simply delivers
/// raw bytes along with the sender's identity and a timestamp.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// Stable node ID of the sender.
    pub from: String,
    /// Raw bytes received (Layer 6 will interpret this).
    pub data: Vec<u8>,
    /// When this message was received.
    pub received_at: Instant,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from Layer 5 session operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The specified peer is not known to the registry.
    #[error("unknown peer: {0}")]
    UnknownPeer(String),

    /// The specified peer is offline (Layer 3 reports not online).
    #[error("peer offline: {0}")]
    PeerOffline(String),

    /// Failed to establish a transport connection.
    #[error("connect failed: {0}")]
    ConnectFailed(String),

    /// Failed to send data on a transport connection.
    #[error("send failed: {0}")]
    SendFailed(String),

    /// Reconnect backoff is active — wait before retrying.
    #[error("reconnect backoff: retry after {retry_after:?}")]
    ReconnectBackoff {
        /// How long the caller must wait before retrying.
        retry_after: Duration,
    },

    /// A transport layer error.
    #[error("transport error: {0}")]
    Transport(#[from] crate::transport::TransportError),
}

// ---------------------------------------------------------------------------
// WsConnectionHandle — channel-based connection control
// ---------------------------------------------------------------------------

/// A handle to an active WebSocket connection.
///
/// Instead of sharing a `Mutex<WsFramedStream>` (which would deadlock
/// because recv holds the lock across awaits), we use a channel pair:
/// - `send_tx`: Send data to the connection task, which writes to the WS
/// - `close_tx`: Signal the connection task to close and exit
///
/// The connection task exclusively owns the `WsFramedStream` and uses
/// `tokio::select!` to multiplex between sending, receiving, and close
/// signals. This avoids lock contention entirely.
struct WsConnectionHandle {
    /// Channel to send outgoing data to the connection task.
    send_tx: mpsc::Sender<Vec<u8>>,
    /// One-shot close signal. Dropping this also signals close.
    close_tx: mpsc::Sender<()>,
    /// Stable node ID of the connected peer.
    #[allow(dead_code)]
    peer_id: String,
    /// When this connection was established.
    #[allow(dead_code)]
    connected_at: Instant,
}

// ---------------------------------------------------------------------------
// PeerRegistry
// ---------------------------------------------------------------------------

/// Manages peer state and WebSocket connections.
///
/// The `PeerRegistry` is the heart of Layer 5. It:
///
/// 1. **Tracks peers** from Layer 3 discovery events — peers exist in the
///    registry even with zero transport connections.
/// 2. **Manages lazy connections** — the first [`send()`](Self::send) to a
///    peer triggers a WebSocket connection via Layer 4. Subsequent sends
///    reuse the cached connection.
/// 3. **Routes messages** — incoming messages from any peer are forwarded
///    to subscribers via a broadcast channel.
/// 4. **Emits lifecycle events** — [`PeerEvent`]s for peer discovery changes
///    and connection state changes.
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
/// use truffle_core::session::PeerRegistry;
///
/// let registry = PeerRegistry::new(network, ws_transport);
/// registry.start().await;
///
/// // Peers appear from Layer 3 discovery
/// let peers = registry.peers().await;
///
/// // First send lazily connects
/// registry.send("peer-id", b"hello").await?;
/// ```
pub struct PeerRegistry<N: NetworkProvider + 'static> {
    /// Layer 3 network provider (for peer events and addressing).
    network: Arc<N>,
    /// Layer 4 WebSocket transport (for framed connections).
    ws_transport: Arc<WebSocketTransport<N>>,

    /// All known peers from Layer 3. Peers exist here even with zero connections.
    peers: Arc<RwLock<HashMap<String, PeerState>>>,

    /// Active WebSocket connection handles indexed by peer_id.
    ws_connections: Arc<RwLock<HashMap<String, WsConnectionHandle>>>,

    /// Reconnect backoff trackers per peer.
    peer_backoffs: Arc<RwLock<HashMap<String, ReconnectBackoff>>>,

    /// Set of peer IDs currently being connected to (prevents duplicate dials).
    connecting: Arc<RwLock<HashSet<String>>>,

    /// Event channel for peer changes (discovery + connection lifecycle).
    event_tx: broadcast::Sender<PeerEvent>,

    /// Channel for incoming messages from any connected peer.
    incoming_tx: broadcast::Sender<IncomingMessage>,
}

impl<N: NetworkProvider + 'static> PeerRegistry<N> {
    /// Create a new peer registry.
    ///
    /// - `network`: The Layer 3 network provider for peer discovery.
    /// - `ws_transport`: The Layer 4 WebSocket transport for connections.
    ///
    /// Call [`start()`](Self::start) after creation to begin processing
    /// peer events and accepting incoming connections.
    pub fn new(network: Arc<N>, ws_transport: Arc<WebSocketTransport<N>>) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let (incoming_tx, _) = broadcast::channel(1024);

        Self {
            network,
            ws_transport,
            peers: Arc::new(RwLock::new(HashMap::new())),
            ws_connections: Arc::new(RwLock::new(HashMap::new())),
            peer_backoffs: Arc::new(RwLock::new(HashMap::new())),
            connecting: Arc::new(RwLock::new(HashSet::new())),
            event_tx,
            incoming_tx,
        }
    }

    /// Start the peer registry.
    ///
    /// This spawns two background tasks:
    /// 1. A task that subscribes to Layer 3 peer events and maintains the
    ///    peer list (Joined/Left/Updated).
    /// 2. A task that listens for incoming WebSocket connections from peers
    ///    and spawns connection tasks for each.
    ///
    /// Call this once after constructing the registry.
    pub async fn start(&self) {
        // Task 1: Subscribe to Layer 3 peer events
        self.spawn_peer_event_loop();

        // Task 2: Accept incoming WS connections
        self.spawn_accept_loop().await;
    }

    /// Spawn a task that subscribes to Layer 3 peer events and updates the
    /// internal peer list.
    fn spawn_peer_event_loop(&self) {
        let mut events = self.network.peer_events();
        let peers = self.peers.clone();
        let ws_connections = self.ws_connections.clone();
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(NetworkPeerEvent::Joined(network_peer)) => {
                        let state = network_peer_to_state(&network_peer);
                        let peer_event = PeerEvent::Joined(state.clone());

                        {
                            let mut map = peers.write().await;
                            map.insert(network_peer.id.clone(), state);
                        }

                        let _ = event_tx.send(peer_event);
                        tracing::info!(
                            peer_id = %network_peer.id,
                            peer_name = %network_peer.hostname,
                            "session: peer joined"
                        );
                    }
                    Ok(NetworkPeerEvent::Left(peer_id)) => {
                        // Close any active WS connection for this peer
                        let handle = {
                            let mut conns = ws_connections.write().await;
                            conns.remove(&peer_id)
                        };
                        if let Some(handle) = handle {
                            let _ = handle.close_tx.send(()).await;
                            // Emit Disconnected before Left
                            let _ = event_tx.send(PeerEvent::Disconnected(peer_id.clone()));
                            tracing::info!(
                                peer_id = %peer_id,
                                "session: closed WS connection for departing peer"
                            );
                        }

                        {
                            let mut map = peers.write().await;
                            map.remove(&peer_id);
                        }

                        let _ = event_tx.send(PeerEvent::Left(peer_id.clone()));
                        tracing::info!(peer_id = %peer_id, "session: peer left");
                    }
                    Ok(NetworkPeerEvent::Updated(network_peer)) => {
                        let mut state = network_peer_to_state(&network_peer);

                        // Preserve the `connected` flag from existing state
                        {
                            let mut map = peers.write().await;
                            if let Some(existing) = map.get(&network_peer.id) {
                                state.connected = existing.connected;
                            }
                            map.insert(network_peer.id.clone(), state.clone());
                        }

                        let _ = event_tx.send(PeerEvent::Updated(state));
                        tracing::debug!(
                            peer_id = %network_peer.id,
                            "session: peer updated"
                        );
                    }
                    Ok(NetworkPeerEvent::AuthRequired { url }) => {
                        let _ = event_tx.send(PeerEvent::AuthRequired { url });
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            missed = n,
                            "session: peer event receiver lagged, missed {n} events"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::debug!("session: peer event channel closed");
                        break;
                    }
                }
            }
        });
    }

    /// Spawn a task that accepts incoming WebSocket connections from peers.
    async fn spawn_accept_loop(&self) {
        let ws_transport = self.ws_transport.clone();
        let ws_connections = self.ws_connections.clone();
        let peers = self.peers.clone();
        let event_tx = self.event_tx.clone();
        let incoming_tx = self.incoming_tx.clone();

        // Try to start the WS listener. If it fails, log and return.
        let mut listener = match ws_transport.listen().await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("session: failed to start WS listener: {e}");
                return;
            }
        };

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Some(stream) => {
                        let peer_id = stream.remote_peer_id().to_string();
                        tracing::info!(
                            peer_id = %peer_id,
                            "session: accepted incoming WS connection"
                        );

                        // Create connection handle and spawn connection task
                        let handle = spawn_connection_task(
                            stream,
                            peer_id.clone(),
                            ws_connections.clone(),
                            peers.clone(),
                            event_tx.clone(),
                            incoming_tx.clone(),
                        );

                        {
                            let mut conns = ws_connections.write().await;
                            conns.insert(peer_id.clone(), handle);
                        }

                        // Mark peer as connected
                        {
                            let mut map = peers.write().await;
                            if let Some(state) = map.get_mut(&peer_id) {
                                state.connected = true;
                            }
                        }

                        let _ = event_tx.send(PeerEvent::Connected(peer_id));
                    }
                    None => {
                        tracing::debug!("session: WS listener closed");
                        break;
                    }
                }
            }
        });
    }

    /// Return all known peers.
    ///
    /// This returns peers discovered by Layer 3, including those with
    /// no active transport connections (`connected: false`).
    pub async fn peers(&self) -> Vec<PeerState> {
        let map = self.peers.read().await;
        map.values().cloned().collect()
    }

    /// Subscribe to peer change events.
    ///
    /// Returns a broadcast receiver that yields [`PeerEvent`]s for peer
    /// discovery changes (Joined/Left/Updated) and connection lifecycle
    /// changes (Connected/Disconnected).
    pub fn on_peer_change(&self) -> broadcast::Receiver<PeerEvent> {
        self.event_tx.subscribe()
    }

    /// Send data to a specific peer.
    ///
    /// If no WebSocket connection exists to the peer, one is lazily
    /// established via Layer 4. The connection is cached for subsequent
    /// sends. If the peer is unknown or offline, an error is returned.
    ///
    /// # Errors
    ///
    /// - [`SessionError::UnknownPeer`] if the peer is not in the registry
    /// - [`SessionError::PeerOffline`] if Layer 3 reports the peer as offline
    /// - [`SessionError::ConnectFailed`] if the WS connection cannot be established
    /// - [`SessionError::SendFailed`] if the send operation fails
    pub async fn send(&self, peer_id: &str, data: &[u8]) -> Result<(), SessionError> {
        // 0. Resolve peer_id: accept either the Tailscale node ID or the
        //    peer name (hostname).  This mirrors Node::ping() which also
        //    searches by both id and name.
        let peer_id = {
            let map = self.peers.read().await;
            if map.contains_key(peer_id) {
                peer_id.to_string()
            } else {
                // Fall back to searching by name
                map.values()
                    .find(|p| p.name == peer_id)
                    .map(|p| p.id.clone())
                    .ok_or_else(|| SessionError::UnknownPeer(peer_id.to_string()))?
            }
        };
        let peer_id = peer_id.as_str();

        // 1. Look up peer in the registry
        let peer_addr = {
            let map = self.peers.read().await;
            let state = map
                .get(peer_id)
                .ok_or_else(|| SessionError::UnknownPeer(peer_id.to_string()))?;

            if !state.online {
                return Err(SessionError::PeerOffline(peer_id.to_string()));
            }

            PeerAddr {
                ip: Some(state.ip),
                hostname: state.name.clone(),
                dns_name: None,
            }
        };

        // 2. Check for existing WS connection
        {
            let conns = self.ws_connections.read().await;
            if let Some(handle) = conns.get(peer_id) {
                return handle
                    .send_tx
                    .send(data.to_vec())
                    .await
                    .map_err(|_| SessionError::SendFailed("connection task closed".to_string()));
            }
        }

        // 3. Check reconnect backoff before attempting a new connection
        {
            let backoffs = self.peer_backoffs.read().await;
            if let Some(backoff) = backoffs.get(peer_id) {
                if backoff.should_retry().is_none() {
                    let retry_after = backoff.retry_after();
                    return Err(SessionError::ReconnectBackoff { retry_after });
                }
            }
        }

        // 4. Concurrent send protection — check if another task is already connecting
        {
            let mut connecting = self.connecting.write().await;
            if connecting.contains(peer_id) {
                // Another send() is already dialing this peer. Fail fast rather
                // than creating a duplicate connection.
                return Err(SessionError::ConnectFailed(
                    "connection already in progress".to_string(),
                ));
            }
            connecting.insert(peer_id.to_string());
        }

        // 5. No existing connection — lazily connect via Layer 4
        tracing::info!(peer_id = %peer_id, "session: lazy connecting WS");

        let connect_result = self.ws_transport.connect(&peer_addr).await;

        // Remove from connecting set regardless of outcome
        {
            let mut connecting = self.connecting.write().await;
            connecting.remove(peer_id);
        }

        let ws_stream = match connect_result {
            Ok(stream) => {
                // Successful connect — reset backoff
                let mut backoffs = self.peer_backoffs.write().await;
                backoffs
                    .entry(peer_id.to_string())
                    .or_insert_with(ReconnectBackoff::new)
                    .success();
                stream
            }
            Err(e) => {
                // Failed connect — increase backoff
                let mut backoffs = self.peer_backoffs.write().await;
                backoffs
                    .entry(peer_id.to_string())
                    .or_insert_with(ReconnectBackoff::new)
                    .failure();
                return Err(SessionError::ConnectFailed(e.to_string()));
            }
        };

        // 6. Create connection handle and spawn connection task
        let handle = spawn_connection_task(
            ws_stream,
            peer_id.to_string(),
            self.ws_connections.clone(),
            self.peers.clone(),
            self.event_tx.clone(),
            self.incoming_tx.clone(),
        );

        // Send data before inserting (so we don't lose the race)
        let send_result = handle
            .send_tx
            .send(data.to_vec())
            .await
            .map_err(|_| SessionError::SendFailed("connection task closed".to_string()));

        {
            let mut conns = self.ws_connections.write().await;
            conns.insert(peer_id.to_string(), handle);
        }

        // Mark peer as connected
        {
            let mut map = self.peers.write().await;
            if let Some(state) = map.get_mut(peer_id) {
                state.connected = true;
            }
        }

        let _ = self
            .event_tx
            .send(PeerEvent::Connected(peer_id.to_string()));

        send_result
    }

    /// Broadcast data to all peers with active WebSocket connections.
    ///
    /// Sends to all currently connected peers. Peers with no active
    /// connection are skipped (no lazy connect on broadcast).
    /// Errors from individual sends are logged but do not fail the broadcast.
    pub async fn broadcast(&self, data: &[u8]) {
        let conns = self.ws_connections.read().await;

        for (peer_id, handle) in conns.iter() {
            if let Err(_) = handle.send_tx.send(data.to_vec()).await {
                tracing::warn!(
                    peer_id = %peer_id,
                    "session: broadcast send failed (connection task closed)"
                );
            }
        }
    }

    /// Subscribe to incoming messages from any connected peer.
    ///
    /// Returns a broadcast receiver that yields [`IncomingMessage`]s.
    /// Messages include the sender's peer ID and raw bytes — Layer 5
    /// does not interpret the payload.
    pub fn subscribe(&self) -> broadcast::Receiver<IncomingMessage> {
        self.incoming_tx.subscribe()
    }

    /// Disconnect a specific peer's WebSocket connection.
    ///
    /// Removes the cached connection and marks the peer as disconnected.
    /// Does not remove the peer from the registry (that only happens when
    /// Layer 3 emits a `Left` event).
    pub async fn disconnect(&self, peer_id: &str) {
        let handle = {
            let mut conns = self.ws_connections.write().await;
            conns.remove(peer_id)
        };

        if let Some(handle) = handle {
            // Signal the connection task to close. If the channel is already
            // closed (task exited), that's fine.
            let _ = handle.close_tx.send(()).await;
        }

        // Mark peer as disconnected
        {
            let mut map = self.peers.write().await;
            if let Some(state) = map.get_mut(peer_id) {
                state.connected = false;
            }
        }

        let _ = self
            .event_tx
            .send(PeerEvent::Disconnected(peer_id.to_string()));
    }
}

// ---------------------------------------------------------------------------
// Connection task — exclusively owns the WsFramedStream
// ---------------------------------------------------------------------------

/// Spawn a background task that exclusively owns a `WsFramedStream`.
///
/// The task uses `tokio::select!` to multiplex between:
/// - Receiving outgoing data from the `send_rx` channel and writing to the WS
/// - Reading incoming data from the WS and forwarding to `incoming_tx`
/// - Receiving a close signal from `close_rx`
///
/// When the task exits (stream closed, error, or close signal), it cleans up
/// the connection from the registry and emits a `Disconnected` event.
///
/// Returns a [`WsConnectionHandle`] for the caller to send data and close.
fn spawn_connection_task(
    stream: WsFramedStream,
    peer_id: String,
    ws_connections: Arc<RwLock<HashMap<String, WsConnectionHandle>>>,
    peers: Arc<RwLock<HashMap<String, PeerState>>>,
    event_tx: broadcast::Sender<PeerEvent>,
    incoming_tx: broadcast::Sender<IncomingMessage>,
) -> WsConnectionHandle {
    let (send_tx, mut send_rx) = mpsc::channel::<Vec<u8>>(256);
    let (close_tx, mut close_rx) = mpsc::channel::<()>(1);

    let handle = WsConnectionHandle {
        send_tx: send_tx.clone(),
        close_tx: close_tx.clone(),
        peer_id: peer_id.clone(),
        connected_at: Instant::now(),
    };

    tokio::spawn(async move {
        let mut stream = stream;
        let mut closed = false;

        loop {
            tokio::select! {
                // Outgoing: data from send channel → write to WS
                Some(data) = send_rx.recv() => {
                    if let Err(e) = stream.send(&data).await {
                        tracing::warn!(
                            peer_id = %peer_id,
                            error = %e,
                            "session: WS send error"
                        );
                        break;
                    }
                }

                // Incoming: data from WS → forward to incoming channel
                result = stream.recv() => {
                    match result {
                        Ok(Some(data)) => {
                            let msg = IncomingMessage {
                                from: peer_id.clone(),
                                data,
                                received_at: Instant::now(),
                            };
                            let _ = incoming_tx.send(msg);
                        }
                        Ok(None) => {
                            tracing::info!(
                                peer_id = %peer_id,
                                "session: WS stream closed"
                            );
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(
                                peer_id = %peer_id,
                                error = %e,
                                "session: WS recv error"
                            );
                            break;
                        }
                    }
                }

                // Close signal
                _ = close_rx.recv() => {
                    tracing::info!(
                        peer_id = %peer_id,
                        "session: connection close requested"
                    );
                    closed = true;
                    let _ = stream.close().await;
                    break;
                }
            }
        }

        // Clean up: remove connection from registry, mark peer as disconnected
        // Only clean up if we weren't explicitly closed (disconnect() handles
        // its own cleanup to avoid racing).
        if !closed {
            {
                let mut conns = ws_connections.write().await;
                conns.remove(&peer_id);
            }
            {
                let mut map = peers.write().await;
                if let Some(state) = map.get_mut(&peer_id) {
                    state.connected = false;
                }
            }
            let _ = event_tx.send(PeerEvent::Disconnected(peer_id));
        }
    });

    handle
}

// ---------------------------------------------------------------------------
// Helper: convert NetworkPeer to PeerState
// ---------------------------------------------------------------------------

/// Convert a Layer 3 `NetworkPeer` to a Layer 5 `PeerState`.
///
/// Sets `connected: false` by default — connections are managed by Layer 5,
/// not by Layer 3 discovery.
fn network_peer_to_state(peer: &NetworkPeer) -> PeerState {
    let connection_type = if let Some(ref relay) = peer.relay {
        format!("relay:{relay}")
    } else if peer.cur_addr.is_some() {
        "direct".to_string()
    } else {
        "unknown".to_string()
    };

    PeerState {
        id: peer.id.clone(),
        name: peer.hostname.clone(),
        ip: peer.ip,
        online: peer.online,
        connected: false,
        connection_type,
        os: peer.os.clone(),
        last_seen: peer.last_seen.clone(),
    }
}
