//! Serializable types for the Tauri frontend.
//!
//! These types wrap truffle-core's internal types with `#[derive(Serialize)]`
//! so they can be returned from Tauri commands as JSON. Core types intentionally
//! do not derive Serialize, so we map them here at the plugin boundary.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// StartConfig — input from frontend
// ---------------------------------------------------------------------------

/// Configuration for starting a truffle node, received from the frontend.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartConfig {
    /// Node display name (used as Tailscale hostname).
    pub name: String,
    /// Path to the Go sidecar binary.
    pub sidecar_path: String,
    /// Optional Tailscale state directory.
    pub state_dir: Option<String>,
    /// Optional Tailscale auth key for headless authentication.
    pub auth_key: Option<String>,
    /// Whether the node is ephemeral (auto-removed on shutdown).
    #[serde(default)]
    pub ephemeral: bool,
    /// WebSocket listen port (defaults to 9417 if not set).
    pub ws_port: Option<u16>,
}

// ---------------------------------------------------------------------------
// NodeIdentityJs
// ---------------------------------------------------------------------------

/// Local node identity, serialized for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeIdentityJs {
    pub id: String,
    pub hostname: String,
    pub name: String,
    pub dns_name: Option<String>,
    pub ip: Option<String>,
}

impl From<truffle_core::network::NodeIdentity> for NodeIdentityJs {
    fn from(i: truffle_core::network::NodeIdentity) -> Self {
        Self {
            id: i.id,
            hostname: i.hostname,
            name: i.name,
            dns_name: i.dns_name,
            ip: i.ip.map(|a| a.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// PeerJs
// ---------------------------------------------------------------------------

/// A peer as seen by the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerJs {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub online: bool,
    pub ws_connected: bool,
    pub connection_type: String,
    pub os: Option<String>,
    pub last_seen: Option<String>,
}

impl From<truffle_core::Peer> for PeerJs {
    fn from(p: truffle_core::Peer) -> Self {
        Self {
            id: p.id,
            name: p.name,
            ip: p.ip.to_string(),
            online: p.online,
            ws_connected: p.ws_connected,
            connection_type: p.connection_type,
            os: p.os,
            last_seen: p.last_seen,
        }
    }
}

// ---------------------------------------------------------------------------
// PingResultJs
// ---------------------------------------------------------------------------

/// Ping result, serialized for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResultJs {
    pub latency_ms: f64,
    pub connection: String,
    pub peer_addr: Option<String>,
}

impl From<truffle_core::network::PingResult> for PingResultJs {
    fn from(r: truffle_core::network::PingResult) -> Self {
        Self {
            latency_ms: r.latency.as_secs_f64() * 1000.0,
            connection: r.connection,
            peer_addr: r.peer_addr,
        }
    }
}

// ---------------------------------------------------------------------------
// HealthInfoJs
// ---------------------------------------------------------------------------

/// Health info, serialized for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthInfoJs {
    pub state: String,
    pub key_expiry: Option<String>,
    pub warnings: Vec<String>,
    pub healthy: bool,
}

impl From<truffle_core::network::HealthInfo> for HealthInfoJs {
    fn from(h: truffle_core::network::HealthInfo) -> Self {
        Self {
            state: h.state,
            key_expiry: h.key_expiry,
            warnings: h.warnings,
            healthy: h.healthy,
        }
    }
}

// ---------------------------------------------------------------------------
// TransferResultJs
// ---------------------------------------------------------------------------

/// File transfer result, serialized for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferResultJs {
    pub bytes_transferred: u64,
    pub sha256: String,
    pub elapsed_secs: f64,
}

impl From<truffle_core::TransferResult> for TransferResultJs {
    fn from(r: truffle_core::TransferResult) -> Self {
        Self {
            bytes_transferred: r.bytes_transferred,
            sha256: r.sha256,
            elapsed_secs: r.elapsed_secs,
        }
    }
}

// ---------------------------------------------------------------------------
// FileOfferJs
// ---------------------------------------------------------------------------

/// An incoming file offer, serialized for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOfferJs {
    pub from_peer: String,
    pub from_name: String,
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    pub suggested_path: String,
    pub token: String,
}

impl From<truffle_core::FileOffer> for FileOfferJs {
    fn from(o: truffle_core::FileOffer) -> Self {
        Self {
            from_peer: o.from_peer,
            from_name: o.from_name,
            file_name: o.file_name,
            size: o.size,
            sha256: o.sha256,
            suggested_path: o.suggested_path,
            token: o.token,
        }
    }
}

// ---------------------------------------------------------------------------
// PeerEventJs
// ---------------------------------------------------------------------------

/// Peer event, serialized for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum PeerEventJs {
    Joined { peer: PeerStateJs },
    Left { id: String },
    Updated { peer: PeerStateJs },
    WsConnected { id: String },
    WsDisconnected { id: String },
    AuthRequired { url: String },
}

/// Internal peer state, serialized for the frontend (used in PeerEventJs).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStateJs {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub online: bool,
    pub ws_connected: bool,
    pub connection_type: String,
    pub os: Option<String>,
    pub last_seen: Option<String>,
}

impl From<truffle_core::session::PeerState> for PeerStateJs {
    fn from(s: truffle_core::session::PeerState) -> Self {
        Self {
            id: s.id,
            name: s.name,
            ip: s.ip.to_string(),
            online: s.online,
            ws_connected: s.ws_connected,
            connection_type: s.connection_type,
            os: s.os,
            last_seen: s.last_seen,
        }
    }
}

impl From<truffle_core::session::PeerEvent> for PeerEventJs {
    fn from(e: truffle_core::session::PeerEvent) -> Self {
        use truffle_core::session::PeerEvent;
        match e {
            PeerEvent::Joined(state) => PeerEventJs::Joined {
                peer: state.into(),
            },
            PeerEvent::Left(id) => PeerEventJs::Left { id },
            PeerEvent::Updated(state) => PeerEventJs::Updated {
                peer: state.into(),
            },
            PeerEvent::WsConnected(id) => PeerEventJs::WsConnected { id },
            PeerEvent::WsDisconnected(id) => PeerEventJs::WsDisconnected { id },
            PeerEvent::AuthRequired { url } => PeerEventJs::AuthRequired { url },
        }
    }
}

// ---------------------------------------------------------------------------
// FileTransferEventJs
// ---------------------------------------------------------------------------

/// File transfer event, serialized for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum FileTransferEventJs {
    OfferReceived {
        offer: FileOfferJs,
    },
    Hashing {
        token: String,
        file_name: String,
        bytes_hashed: u64,
        total_bytes: u64,
    },
    WaitingForAccept {
        token: String,
        file_name: String,
    },
    Progress {
        token: String,
        direction: String,
        file_name: String,
        bytes_transferred: u64,
        total_bytes: u64,
        speed_bps: f64,
    },
    Completed {
        token: String,
        direction: String,
        file_name: String,
        bytes_transferred: u64,
        sha256: String,
        elapsed_secs: f64,
    },
    Rejected {
        token: String,
        file_name: String,
        reason: String,
    },
    Failed {
        token: String,
        direction: String,
        file_name: String,
        reason: String,
    },
}

fn direction_str(d: truffle_core::TransferDirection) -> String {
    match d {
        truffle_core::TransferDirection::Send => "send".to_string(),
        truffle_core::TransferDirection::Receive => "receive".to_string(),
    }
}

impl From<truffle_core::FileTransferEvent> for FileTransferEventJs {
    fn from(e: truffle_core::FileTransferEvent) -> Self {
        use truffle_core::FileTransferEvent;
        match e {
            FileTransferEvent::OfferReceived(offer) => FileTransferEventJs::OfferReceived {
                offer: offer.into(),
            },
            FileTransferEvent::Hashing {
                token,
                file_name,
                bytes_hashed,
                total_bytes,
            } => FileTransferEventJs::Hashing {
                token,
                file_name,
                bytes_hashed,
                total_bytes,
            },
            FileTransferEvent::WaitingForAccept { token, file_name } => {
                FileTransferEventJs::WaitingForAccept { token, file_name }
            }
            FileTransferEvent::Progress(p) => FileTransferEventJs::Progress {
                token: p.token,
                direction: direction_str(p.direction),
                file_name: p.file_name,
                bytes_transferred: p.bytes_transferred,
                total_bytes: p.total_bytes,
                speed_bps: p.speed_bps,
            },
            FileTransferEvent::Completed {
                token,
                direction,
                file_name,
                bytes_transferred,
                sha256,
                elapsed_secs,
            } => FileTransferEventJs::Completed {
                token,
                direction: direction_str(direction),
                file_name,
                bytes_transferred,
                sha256,
                elapsed_secs,
            },
            FileTransferEvent::Rejected {
                token,
                file_name,
                reason,
            } => FileTransferEventJs::Rejected {
                token,
                file_name,
                reason,
            },
            FileTransferEvent::Failed {
                token,
                direction,
                file_name,
                reason,
            } => FileTransferEventJs::Failed {
                token,
                direction: direction_str(direction),
                file_name,
                reason,
            },
        }
    }
}
