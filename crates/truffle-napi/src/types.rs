//! JS-facing types for the NAPI bindings.
//!
//! All structs here use `#[napi(object)]` so they are passed as plain JS
//! objects (not class instances). They mirror the public types from
//! truffle-core but flatten/simplify for JS ergonomics.
//!
//! RFC 017 identity model: application code sees peers by `device_id`
//! (stable ULID from the hello envelope) and `device_name` (human-readable
//! Unicode). The Tailscale stable ID and hostname are still available as
//! escape hatches for diagnostics.

use napi_derive::napi;

// ---------------------------------------------------------------------------
// Node configuration
// ---------------------------------------------------------------------------

/// Configuration for starting a Node (RFC 017 §7 shape).
#[napi(object)]
pub struct NapiNodeConfig {
    /// Application namespace identifier. Required. Must match
    /// `^[a-z][a-z0-9-]{1,31}$`; validated on the Rust side.
    pub app_id: String,
    /// Optional human-readable device name. Defaults to the OS hostname.
    /// May contain any Unicode characters; the Tailscale hostname is
    /// derived automatically.
    pub device_name: Option<String>,
    /// Optional stable ULID override. Auto-generated (and persisted) when
    /// omitted.
    pub device_id: Option<String>,
    /// Tailscale state directory. Defaults to
    /// `{userDataDir}/truffle/{app_id}/{slug(device_name)}`.
    pub state_dir: Option<String>,
    /// Path to the Go sidecar binary.
    pub sidecar_path: String,
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

/// Identity of the local node (RFC 017 §7.2).
#[napi(object)]
pub struct NapiNodeIdentity {
    /// Application namespace identifier.
    pub app_id: String,
    /// Stable per-device ULID. Primary key for device identity.
    pub device_id: String,
    /// Original (unsanitised) device name, as passed by the application.
    pub device_name: String,
    /// The Tailscale hostname (`truffle-{app_id}-{slug}`). Debug use only.
    pub tailscale_hostname: String,
    /// Tailscale stable node ID. Escape hatch for diagnostics.
    pub tailscale_id: String,
    /// DNS name on the tailnet, if available.
    pub dns_name: Option<String>,
    /// Tailscale IP address as a string, if available.
    pub ip: Option<String>,
}

// ---------------------------------------------------------------------------
// Peer
// ---------------------------------------------------------------------------

/// A peer as seen by application code (RFC 017 §7.3).
#[napi(object)]
pub struct NapiPeer {
    /// Stable per-device ULID from the remote node. Primary key.
    pub device_id: String,
    /// Human-readable device name from the remote node.
    pub device_name: String,
    /// Network IP address as a string.
    pub ip: String,
    /// Whether the peer is online (from Layer 3).
    pub online: bool,
    /// Whether there is an active WebSocket connection.
    pub ws_connected: bool,
    /// Connection type description (e.g., "direct" or "relay:ord").
    pub connection_type: String,
    /// Operating system, if known.
    pub os: Option<String>,
    /// Last time the peer was seen online (RFC 3339 string).
    pub last_seen: Option<String>,
    /// Tailscale stable ID. Escape hatch; most code should use `deviceId`.
    pub tailscale_id: String,
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
    /// Event type: "joined", "left", "updated", "ws_connected", "ws_disconnected", "auth_required".
    pub event_type: String,
    /// Stable `device_id` (ULID) of the affected peer. Empty string for
    /// `auth_required` events (no associated peer).
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
    /// Sender's stable `device_id` (ULID) from the RFC 017 hello envelope.
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
    /// Sender's stable `device_id` (ULID) from the RFC 017 hello envelope.
    pub from_peer: String,
    /// Human-readable device name of the sending peer.
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

// ---------------------------------------------------------------------------
// Synced store types
// ---------------------------------------------------------------------------

/// A versioned slice of data owned by a single device.
#[napi(object)]
pub struct NapiSlice {
    /// Owning device's stable `device_id` (ULID) from the RFC 017 hello
    /// envelope.
    pub device_id: String,
    /// The data (JSON value).
    pub data: serde_json::Value,
    /// Monotonically increasing version (per-device).
    pub version: f64,
    /// When this version was created (Unix milliseconds).
    pub updated_at: f64,
}

/// A store change event delivered to JS.
#[napi(object)]
pub struct NapiStoreEvent {
    /// Event type: "local_changed", "peer_updated", "peer_removed".
    pub event_type: String,
    /// Stable `device_id` (ULID) of the affected device (present for
    /// peer_updated / peer_removed events).
    pub device_id: Option<String>,
    /// Data payload (present for local_changed/peer_updated events).
    pub data: Option<serde_json::Value>,
    /// Version number (present for peer_updated events).
    pub version: Option<f64>,
}

// ---------------------------------------------------------------------------
// File transfer types
// ---------------------------------------------------------------------------

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
