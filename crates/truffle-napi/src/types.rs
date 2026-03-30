//! JS-facing types for the NAPI bindings.
//!
//! All structs here use `#[napi(object)]` so they are passed as plain JS
//! objects (not class instances). They mirror the public types from
//! truffle-core but flatten/simplify for JS ergonomics.

use napi_derive::napi;

// ---------------------------------------------------------------------------
// Node configuration
// ---------------------------------------------------------------------------

/// Configuration for starting a Node.
#[napi(object)]
pub struct NapiNodeConfig {
    /// Human-readable node name (used as Tailscale hostname).
    pub name: String,
    /// Path to the Go sidecar binary.
    pub sidecar_path: String,
    /// Tailscale state directory. Defaults to `/tmp/truffle-{name}`.
    pub state_dir: Option<String>,
    /// Tailscale auth key for headless authentication.
    pub auth_key: Option<String>,
    /// Whether the node is ephemeral (auto-removed from tailnet on shutdown).
    pub ephemeral: Option<bool>,
    /// WebSocket listen port. Defaults to 9417.
    pub ws_port: Option<u16>,
}

// ---------------------------------------------------------------------------
// Node identity
// ---------------------------------------------------------------------------

/// Identity of the local node.
#[napi(object)]
pub struct NapiNodeIdentity {
    /// Stable node ID.
    pub id: String,
    /// Hostname on the network.
    pub hostname: String,
    /// Human-readable display name.
    pub name: String,
    /// DNS name on the tailnet, if available.
    pub dns_name: Option<String>,
    /// Tailscale IP address as a string, if available.
    pub ip: Option<String>,
}

// ---------------------------------------------------------------------------
// Peer
// ---------------------------------------------------------------------------

/// A peer as seen by application code.
#[napi(object)]
pub struct NapiPeer {
    /// Stable node ID.
    pub id: String,
    /// Human-readable name (hostname).
    pub name: String,
    /// Network IP address as a string.
    pub ip: String,
    /// Whether the peer is online (from Layer 3).
    pub online: bool,
    /// Whether there is an active WebSocket connection.
    pub connected: bool,
    /// Connection type description (e.g., "direct" or "relay:ord").
    pub connection_type: String,
    /// Operating system, if known.
    pub os: Option<String>,
    /// Last time the peer was seen online (RFC 3339 string).
    pub last_seen: Option<String>,
}

// ---------------------------------------------------------------------------
// Ping result
// ---------------------------------------------------------------------------

/// Result of a network-level ping.
#[napi(object)]
pub struct NapiPingResult {
    /// Round-trip latency in milliseconds.
    pub latency_ms: f64,
    /// Connection type description (e.g., "direct" or "relay:sfo").
    pub connection: String,
    /// Direct peer endpoint address, if available.
    pub peer_addr: Option<String>,
}

// ---------------------------------------------------------------------------
// Health info
// ---------------------------------------------------------------------------

/// Health information from the network layer.
#[napi(object)]
pub struct NapiHealthInfo {
    /// Current backend state (e.g., "Running", "NeedsLogin").
    pub state: String,
    /// Key expiry timestamp (RFC 3339), if applicable.
    pub key_expiry: Option<String>,
    /// Active health warnings.
    pub warnings: Vec<String>,
    /// Whether the network is fully operational.
    pub healthy: bool,
}

// ---------------------------------------------------------------------------
// Peer event
// ---------------------------------------------------------------------------

/// A peer change event delivered to JS.
#[napi(object)]
pub struct NapiPeerEvent {
    /// Event type: "joined", "left", "updated", "connected", "disconnected", "auth_required".
    pub event_type: String,
    /// Peer ID (present for peer events, empty for auth_required).
    pub peer_id: String,
    /// Full peer info (present for joined/updated events).
    pub peer: Option<NapiPeer>,
    /// Auth URL (present only for auth_required events).
    pub auth_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Namespaced message
// ---------------------------------------------------------------------------

/// A message received on a specific namespace.
#[napi(object)]
pub struct NapiNamespacedMessage {
    /// Stable node ID of the sender.
    pub from: String,
    /// Namespace the message was sent on.
    pub namespace: String,
    /// Application-defined message type.
    pub msg_type: String,
    /// Opaque JSON payload (as a JS value via serde).
    pub payload: serde_json::Value,
    /// Millisecond Unix timestamp from the sender, if set.
    pub timestamp: Option<f64>,
}

// ---------------------------------------------------------------------------
// File transfer types
// ---------------------------------------------------------------------------

/// An incoming file offer from a remote peer.
#[napi(object)]
pub struct NapiFileOffer {
    /// Stable node ID of the sending peer.
    pub from_peer: String,
    /// Human-readable name of the sending peer.
    pub from_name: String,
    /// File name being offered.
    pub file_name: String,
    /// File size in bytes.
    pub size: f64,
    /// Expected SHA-256 hash (hex).
    pub sha256: String,
    /// Suggested save path from the sender.
    pub suggested_path: String,
    /// Unique token for this transfer.
    pub token: String,
}

/// Result of a completed file transfer.
#[napi(object)]
pub struct NapiTransferResult {
    /// Number of bytes transferred.
    pub bytes_transferred: f64,
    /// SHA-256 hash of the transferred file.
    pub sha256: String,
    /// Elapsed time in seconds.
    pub elapsed_secs: f64,
}

/// Progress update for an in-flight file transfer.
#[napi(object)]
pub struct NapiTransferProgress {
    /// Unique token for this transfer.
    pub token: String,
    /// Direction: "send" or "receive".
    pub direction: String,
    /// File name being transferred.
    pub file_name: String,
    /// Bytes transferred so far.
    pub bytes_transferred: f64,
    /// Total file size in bytes.
    pub total_bytes: f64,
    /// Current transfer speed in bytes per second.
    pub speed_bps: f64,
}

/// Events emitted by the file transfer subsystem.
#[napi(object)]
pub struct NapiFileTransferEvent {
    /// Event type: "offer_received", "hashing", "waiting_for_accept",
    /// "progress", "completed", "rejected", "failed".
    pub event_type: String,
    /// Transfer token (if applicable).
    pub token: Option<String>,
    /// File name (if applicable).
    pub file_name: Option<String>,
    /// Direction: "send" or "receive" (if applicable).
    pub direction: Option<String>,
    /// Progress info (present for "progress" events).
    pub progress: Option<NapiTransferProgress>,
    /// Offer info (present for "offer_received" events).
    pub offer: Option<NapiFileOffer>,
    /// Bytes transferred (present for "completed" events).
    pub bytes_transferred: Option<f64>,
    /// SHA-256 hash (present for "completed" events).
    pub sha256: Option<String>,
    /// Elapsed seconds (present for "completed" events).
    pub elapsed_secs: Option<f64>,
    /// Reason for rejection or failure.
    pub reason: Option<String>,
    /// Bytes hashed so far (present for "hashing" events).
    pub bytes_hashed: Option<f64>,
    /// Total bytes to hash (present for "hashing" events).
    pub total_bytes: Option<f64>,
}

