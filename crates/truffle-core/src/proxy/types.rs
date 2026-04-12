//! Public types for the reverse proxy subsystem.

use serde::{Deserialize, Serialize};

/// Target backend for the reverse proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyTarget {
    /// Target host (e.g. "localhost", "127.0.0.1")
    pub host: String,
    /// Target port (e.g. 3000)
    pub port: u16,
    /// Target scheme ("http" or "https")
    pub scheme: String,
}

impl Default for ProxyTarget {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 0,
            scheme: "http".to_string(),
        }
    }
}

/// Configuration for adding a reverse proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Unique identifier (user-chosen or auto-generated).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Port on which the proxy listens on the tailnet.
    pub listen_port: u16,
    /// Backend target to forward requests to.
    pub target: ProxyTarget,
    /// Whether to announce this proxy on the mesh for discovery.
    #[serde(default = "default_true")]
    pub announce: bool,
}

fn default_true() -> bool {
    true
}

/// Information about a running or configured proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyInfo {
    pub id: String,
    pub name: String,
    pub listen_port: u16,
    pub target: ProxyTarget,
    /// Fully qualified URL (e.g. "https://hostname.ts.net:3001")
    pub url: String,
    pub status: ProxyStatus,
}

/// Status of a proxy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProxyStatus {
    Starting,
    Running,
    Stopped,
    Error(String),
}

/// Events emitted by the proxy subsystem.
#[derive(Debug, Clone)]
pub enum ProxyEvent {
    Started {
        id: String,
        url: String,
        listen_port: u16,
    },
    Stopped {
        id: String,
    },
    Error {
        id: String,
        code: String,
        message: String,
    },
}

/// A proxy announced by a remote peer (used for mesh discovery).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyAnnouncement {
    pub id: String,
    pub name: String,
    pub listen_port: u16,
    pub url: String,
}

/// A remote proxy discovered via the mesh.
#[derive(Debug, Clone)]
pub struct RemoteProxy {
    /// Which peer is hosting this proxy.
    pub peer_id: String,
    /// Human-readable peer name.
    pub peer_name: String,
    /// Proxy ID on the remote peer.
    pub id: String,
    /// Human-readable proxy name.
    pub name: String,
    /// Fully qualified URL.
    pub url: String,
    /// Listen port on the remote peer's tailnet address.
    pub listen_port: u16,
}
