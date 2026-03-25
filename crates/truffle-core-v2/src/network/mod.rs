//! Layer 3: Network — Peer discovery, addressing, encrypted tunnels.
//!
//! This module defines the [`NetworkProvider`] trait, the public API for Layer 3.
//! The trait is generic — no Tailscale-specific types leak through.
//!
//! The [`tailscale`] submodule contains the [`TailscaleProvider`] implementation
//! that wraps the Go sidecar (tsnet) and bridge.

pub mod tailscale;

use std::net::IpAddr;
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
    /// This may spawn child processes, bind ports, and perform authentication.
    /// Returns when the provider is ready to accept connections.
    async fn start(&mut self) -> Result<(), NetworkError>;

    /// Stop the network provider and clean up all resources.
    async fn stop(&mut self) -> Result<(), NetworkError>;

    /// Local node's identity (stable ID, hostname, display name).
    fn local_identity(&self) -> &NodeIdentity;

    /// Local node's network address.
    fn local_addr(&self) -> &PeerAddr;

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
