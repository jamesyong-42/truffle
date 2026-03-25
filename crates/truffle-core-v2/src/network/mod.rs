//! Layer 3: Network — Peer discovery, addressing, encrypted tunnels.
//!
//! This module defines the [`NetworkProvider`] trait, the public API for Layer 3.
//! The trait is generic — no Tailscale-specific types leak through.
//!
//! The [`tailscale`] submodule contains the [`TailscaleProvider`] implementation
//! that wraps the Go sidecar (tsnet) and bridge.

pub mod tailscale;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// NetworkProvider trait — the public API of Layer 3
// ---------------------------------------------------------------------------

/// Provides network-level peer discovery and raw connectivity.
///
/// The primary implementation is [`TailscaleProvider`](tailscale::TailscaleProvider)
/// which uses tsnet via a Go sidecar. The trait is designed to be swappable —
/// future providers could use mDNS (LAN), STUN/TURN (internet), or Bluetooth.
///
/// # Layer rules
///
/// - Layer 3 does NOT know about WebSocket, QUIC, or any Layer 4 protocol
/// - Layer 3 does NOT know about envelopes, namespaces, or messages
/// - Layer 3 provides raw `TcpStream` — not framed connections
/// - `peer_events()` is the ONLY source of peer events — no polling, no announce
#[allow(async_fn_in_trait)]
pub trait NetworkProvider: Send + Sync {
    /// Start the network provider.
    ///
    /// This spawns child processes, binds ports, and performs authentication.
    /// If authentication is required (e.g., Tailscale browser auth), the provider
    /// emits `NetworkPeerEvent::AuthRequired { url }` via `peer_events()` and
    /// **keeps waiting** until auth completes or the timeout is reached.
    ///
    /// Callers should subscribe to `peer_events()` BEFORE calling `start()` to
    /// receive auth URLs and display them to the user.
    ///
    /// Returns `Ok(())` when the provider is fully online, or `Err` if auth
    /// times out or a fatal error occurs.
    async fn start(&mut self) -> Result<(), NetworkError>;

    /// Stop the network provider and clean up all resources.
    async fn stop(&mut self) -> Result<(), NetworkError>;

    /// Local node's identity (stable ID, hostname, display name).
    ///
    /// Returns a clone of the current cached identity. The identity is
    /// populated after [`start()`](Self::start) completes and may be
    /// updated when the sidecar reports `tsnet:status`.
    fn local_identity(&self) -> NodeIdentity;

    /// Local node's network address.
    ///
    /// Returns a clone of the current cached address. The address is
    /// populated after [`start()`](Self::start) completes and may be
    /// updated when the sidecar reports `tsnet:status`.
    fn local_addr(&self) -> PeerAddr;

    // ── Discovery (event-driven, NOT polling) ──

    /// Subscribe to peer events. Fires immediately when peers join/leave/update.
    ///
    /// Uses `WatchIPNBus` for real-time notifications instead of polling.
    fn peer_events(&self) -> broadcast::Receiver<NetworkPeerEvent>;

    /// Snapshot of all currently known peers.
    async fn peers(&self) -> Vec<NetworkPeer>;

    // ── Connectivity primitives for Layer 4 ──

    /// Dial a TCP connection to a peer via the encrypted Tailscale tunnel.
    ///
    /// Returns a plain `TcpStream` — all bridge internals (pending_dials,
    /// session token, binary headers) are hidden inside the provider.
    async fn dial_tcp(&self, addr: &str, port: u16) -> Result<TcpStream, NetworkError>;

    /// Listen for incoming TCP connections on a port via the Tailscale tunnel.
    ///
    /// The returned receiver yields `TcpStream`s for each accepted connection.
    async fn listen_tcp(
        &self,
        port: u16,
    ) -> Result<NetworkTcpListener, NetworkError>;

    /// Stop listening on a previously opened port.
    async fn unlisten_tcp(&self, port: u16) -> Result<(), NetworkError>;

    /// Bind a UDP socket on a port via the network tunnel.
    ///
    /// Returns a [`NetworkUdpSocket`] that transparently relays datagrams
    /// through the network provider. The socket supports `send_to` / `recv_from`
    /// with full remote address information.
    ///
    /// Not all providers support UDP. Returns [`NetworkError::Internal`] if
    /// the provider has not implemented UDP transport yet.
    async fn bind_udp(&self, port: u16) -> Result<NetworkUdpSocket, NetworkError>;

    // ── Diagnostics ──

    /// Ping a peer via the network layer (Tailscale TSMP).
    async fn ping(&self, addr: &str) -> Result<PingResult, NetworkError>;

    /// Node health info (key expiry, connection quality, warnings).
    async fn health(&self) -> HealthInfo;
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A peer as seen by the network layer (Layer 3).
///
/// Contains only information available from the network provider itself
/// (e.g., Tailscale status). No transport or session state.
#[derive(Debug, Clone)]
pub struct NetworkPeer {
    /// Stable node ID from the network provider.
    pub id: String,
    /// Hostname on the network (e.g., "truffle-cli-abc123").
    pub hostname: String,
    /// Network IP address (e.g., 100.x.x.x for Tailscale).
    pub ip: IpAddr,
    /// Whether the peer is currently online.
    pub online: bool,
    /// Direct endpoint address, if connected directly.
    pub cur_addr: Option<String>,
    /// DERP relay name if connection is relayed.
    pub relay: Option<String>,
    /// Operating system of the peer.
    pub os: Option<String>,
    /// Last time the peer was seen online (RFC 3339 string).
    pub last_seen: Option<String>,
    /// Key expiry timestamp (RFC 3339 string).
    pub key_expiry: Option<String>,
    /// DNS name on the tailnet (e.g., "truffle-cli-abc123.tailnet.ts.net").
    pub dns_name: Option<String>,
}

/// Events emitted when network peers change state.
#[derive(Debug, Clone)]
pub enum NetworkPeerEvent {
    /// A new peer appeared on the network.
    Joined(NetworkPeer),
    /// A peer left the network (by stable node ID).
    Left(String),
    /// A peer's metadata changed (IP, relay, online status, etc.).
    Updated(NetworkPeer),
    /// Authentication is required. The URL should be shown to the user.
    /// Emitted during `start()` — the provider keeps waiting for auth to complete.
    /// May be emitted multiple times if the URL expires and is refreshed.
    AuthRequired {
        /// URL the user should open in a browser.
        url: String,
    },
}

/// Network address of a peer.
#[derive(Debug, Clone, Default)]
pub struct PeerAddr {
    /// IP address (100.x.x.x for Tailscale).
    pub ip: Option<IpAddr>,
    /// Hostname on the network.
    pub hostname: String,
    /// DNS name on the tailnet.
    pub dns_name: Option<String>,
}

/// Identity of the local node on the network.
#[derive(Debug, Clone, Default)]
pub struct NodeIdentity {
    /// Stable node ID from the network provider.
    pub id: String,
    /// Hostname on the network.
    pub hostname: String,
    /// Human-readable display name.
    pub name: String,
    /// DNS name on the tailnet.
    pub dns_name: Option<String>,
    /// Tailscale IP address.
    pub ip: Option<IpAddr>,
}

/// Result of a network-level ping.
#[derive(Debug, Clone)]
pub struct PingResult {
    /// Round-trip latency.
    pub latency: Duration,
    /// Connection type description (e.g., "direct" or "relay:sfo").
    pub connection: String,
    /// Direct peer endpoint address, if available.
    pub peer_addr: Option<String>,
}

/// Health information from the network provider.
#[derive(Debug, Clone, Default)]
pub struct HealthInfo {
    /// Current backend state (e.g., "Running", "NeedsLogin").
    pub state: String,
    /// Key expiry timestamp (RFC 3339), if applicable.
    pub key_expiry: Option<String>,
    /// Active health warnings.
    pub warnings: Vec<String>,
    /// Whether the network is fully operational.
    pub healthy: bool,
}

/// A listener for incoming TCP connections via the network provider.
///
/// Wraps a channel that receives `TcpStream`s from the bridge. The bridge
/// internals (binary headers, session tokens) are completely hidden.
pub struct NetworkTcpListener {
    /// Port this listener is bound to.
    pub port: u16,
    /// Receiver for incoming connections.
    pub incoming: tokio::sync::mpsc::Receiver<IncomingConnection>,
}

/// An incoming TCP connection with metadata.
#[derive(Debug)]
pub struct IncomingConnection {
    /// The raw TCP stream (bridge headers already consumed).
    pub stream: TcpStream,
    /// Remote address of the connecting peer.
    pub remote_addr: String,
    /// Remote DNS name or peer identity JSON.
    pub remote_identity: String,
    /// Port the connection arrived on.
    pub port: u16,
}

// ---------------------------------------------------------------------------
// NetworkUdpSocket — address-framed UDP relay wrapper
// ---------------------------------------------------------------------------

/// A UDP socket that relays datagrams through the network provider.
///
/// Under the hood, the Rust side talks to a local relay socket. Each outbound
/// datagram is prefixed with a 6-byte address header (`[4-byte IPv4][2-byte port BE]`)
/// so the relay (Go sidecar) knows where to forward the packet on the tsnet
/// network. Inbound datagrams arrive with the same header prepended by the relay.
///
/// This struct hides the framing — callers use `send_to` / `recv_from` with
/// normal `SocketAddr` values.
pub struct NetworkUdpSocket {
    /// The underlying tokio UDP socket connected to the local relay.
    inner: tokio::net::UdpSocket,
    /// The tsnet-bound port (the logical port on the Tailscale network).
    tsnet_port: u16,
}

/// Address header size: 4 bytes IPv4 + 2 bytes port (big-endian).
const UDP_ADDR_HEADER_SIZE: usize = 6;

impl NetworkUdpSocket {
    /// Create a new `NetworkUdpSocket` from a tokio UdpSocket and the tsnet port.
    pub(crate) fn new(inner: tokio::net::UdpSocket, tsnet_port: u16) -> Self {
        Self { inner, tsnet_port }
    }

    /// Send a datagram to the specified address via the relay.
    ///
    /// The relay will forward the datagram to the target on the tsnet network.
    pub async fn send_to(&self, data: &[u8], addr: SocketAddr) -> Result<usize, NetworkError> {
        let ip = match addr.ip() {
            IpAddr::V4(v4) => v4,
            IpAddr::V6(_) => {
                return Err(NetworkError::Internal(
                    "NetworkUdpSocket: IPv6 not supported in relay framing".into(),
                ));
            }
        };
        let port = addr.port();

        // Build framed packet: [4-byte IPv4][2-byte port BE][payload]
        let mut framed = Vec::with_capacity(UDP_ADDR_HEADER_SIZE + data.len());
        framed.extend_from_slice(&ip.octets());
        framed.extend_from_slice(&port.to_be_bytes());
        framed.extend_from_slice(data);

        let n = self
            .inner
            .send(&framed)
            .await
            .map_err(NetworkError::Io)?;

        // Return the number of payload bytes sent (subtract header)
        Ok(n.saturating_sub(UDP_ADDR_HEADER_SIZE))
    }

    /// Receive a datagram from the relay, returning the payload and sender address.
    ///
    /// The relay prepends a 6-byte address header to each inbound datagram.
    pub async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), NetworkError> {
        // Read into a temporary buffer that includes space for the header
        let mut tmp = vec![0u8; UDP_ADDR_HEADER_SIZE + buf.len()];
        let n = self
            .inner
            .recv(&mut tmp)
            .await
            .map_err(NetworkError::Io)?;

        if n < UDP_ADDR_HEADER_SIZE {
            return Err(NetworkError::Internal(
                "NetworkUdpSocket: received packet too short for address header".into(),
            ));
        }

        // Parse address header
        let ip = Ipv4Addr::new(tmp[0], tmp[1], tmp[2], tmp[3]);
        let port = u16::from_be_bytes([tmp[4], tmp[5]]);
        let addr = SocketAddr::new(IpAddr::V4(ip), port);

        // Copy payload to caller's buffer
        let payload_len = n - UDP_ADDR_HEADER_SIZE;
        buf[..payload_len].copy_from_slice(&tmp[UDP_ADDR_HEADER_SIZE..n]);

        Ok((payload_len, addr))
    }

    /// Return the local address of the underlying relay socket.
    pub fn local_addr(&self) -> Result<SocketAddr, NetworkError> {
        self.inner.local_addr().map_err(NetworkError::Io)
    }

    /// Return the tsnet-bound port (the logical port on the Tailscale network).
    pub fn tsnet_port(&self) -> u16 {
        self.tsnet_port
    }

    /// Get a reference to the inner tokio UdpSocket.
    ///
    /// This is the socket connected to the local relay. Direct reads/writes
    /// bypass the address framing — prefer `send_to` / `recv_from` instead.
    pub fn inner(&self) -> &tokio::net::UdpSocket {
        &self.inner
    }
}

impl std::fmt::Debug for NetworkUdpSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkUdpSocket")
            .field("tsnet_port", &self.tsnet_port)
            .field("local_addr", &self.inner.local_addr().ok())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from Layer 3 network operations.
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    /// The network provider is not running.
    #[error("network provider not running")]
    NotRunning,

    /// The network provider is already running.
    #[error("network provider already running")]
    AlreadyRunning,

    /// Failed to start the network provider.
    #[error("start failed: {0}")]
    StartFailed(String),

    /// Failed to stop the network provider.
    #[error("stop failed: {0}")]
    StopFailed(String),

    /// Authentication is required.
    #[error("authentication required: {url}")]
    AuthRequired { url: String },

    /// A dial operation failed.
    #[error("dial failed: {0}")]
    DialFailed(String),

    /// A dial operation timed out.
    #[error("dial timed out after {0:?}")]
    DialTimeout(Duration),

    /// A listen operation failed.
    #[error("listen failed: {0}")]
    ListenFailed(String),

    /// A ping operation failed.
    #[error("ping failed: {0}")]
    PingFailed(String),

    /// The sidecar process crashed or is unavailable.
    #[error("sidecar error: {0}")]
    SidecarError(String),

    /// Bridge communication error.
    #[error("bridge error: {0}")]
    BridgeError(String),

    /// I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),

    /// Generic internal error.
    #[error("internal error: {0}")]
    Internal(String),
}
