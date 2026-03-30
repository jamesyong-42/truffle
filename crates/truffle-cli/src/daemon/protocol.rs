//! JSON-RPC 2.0 protocol types for CLI-daemon communication.

use serde::{Deserialize, Serialize};

// ==========================================================================
// JSON-RPC 2.0 types
// ==========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default = "default_params")]
    pub params: serde_json::Value,
}

fn default_params() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl DaemonRequest {
    pub fn new(id: u64, method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DaemonError>,
}

impl DaemonResponse {
    pub fn success(id: u64, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: u64, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(DaemonError {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonError {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: serde_json::Value,
}

impl DaemonNotification {
    pub fn new(method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
        }
    }
}

// ==========================================================================
// Method constants
// ==========================================================================

pub mod method {
    pub const STATUS: &str = "status";
    pub const PEERS: &str = "peers";
    pub const SHUTDOWN: &str = "shutdown";
    pub const PING: &str = "ping";
    pub const SEND_MESSAGE: &str = "send_message";
    pub const PUSH_FILE: &str = "push_file";
    pub const GET_FILE: &str = "get_file";
    pub const TCP_CONNECT: &str = "tcp_connect";
    pub const DOCTOR: &str = "doctor";
    pub const SUBSCRIBE: &str = "subscribe";
}

pub mod notification {
    pub const CP_PROGRESS: &str = "cp.progress";
    pub const PEER_JOINED: &str = "peer.joined";
    pub const PEER_LEFT: &str = "peer.left";
    pub const PEER_UPDATED: &str = "peer.updated";
    pub const PEER_WS_CONNECTED: &str = "peer.ws_connected";
    pub const PEER_WS_DISCONNECTED: &str = "peer.ws_disconnected";
    pub const MESSAGE_RECEIVED: &str = "message.received";
}

pub mod error_code {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;

    pub const DAEMON_NOT_RUNNING: i32 = 1;
    pub const NODE_NOT_FOUND: i32 = 2;
    pub const TIMEOUT: i32 = 3;
    pub const NOT_ONLINE: i32 = 4;
}
