//! JSON-RPC 2.0 protocol types for CLI-daemon communication.
//!
//! The daemon listens on a Unix socket and accepts JSON-RPC 2.0 requests.
//! Each request is a single line of JSON terminated by a newline character.
//! The daemon responds with a single line of JSON.

use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════════════
// JSON-RPC 2.0 types
// ═══════════════════════════════════════════════════════════════════════════

/// A JSON-RPC 2.0 request from CLI to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonRequest {
    /// Must be "2.0".
    pub jsonrpc: String,
    /// Request ID for correlating responses.
    pub id: u64,
    /// Method name (e.g., "status", "peers", "shutdown").
    pub method: String,
    /// Method parameters.
    #[serde(default = "default_params")]
    pub params: serde_json::Value,
}

fn default_params() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl DaemonRequest {
    /// Create a new JSON-RPC request.
    pub fn new(id: u64, method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 response from daemon to CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    /// Must be "2.0".
    pub jsonrpc: String,
    /// Matches the request ID.
    pub id: u64,
    /// Success result (mutually exclusive with `error`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error result (mutually exclusive with `result`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DaemonError>,
}

impl DaemonResponse {
    /// Create a success response.
    pub fn success(id: u64, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response.
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

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonError {
    /// Error code (negative numbers for protocol errors, positive for app errors).
    pub code: i32,
    /// Human-readable error message.
    pub message: String,
}

// ═══════════════════════════════════════════════════════════════════════════
// Method constants
// ═══════════════════════════════════════════════════════════════════════════

/// Well-known JSON-RPC method names.
pub mod method {
    /// Return node status (device ID, Tailscale IP, uptime, etc.).
    pub const STATUS: &str = "status";
    /// Return the list of discovered peers.
    pub const PEERS: &str = "peers";
    /// Gracefully shut down the daemon.
    pub const SHUTDOWN: &str = "shutdown";
    /// Open a TCP connection to a target.
    pub const TCP_CONNECT: &str = "tcp_connect";
    /// Open a WebSocket connection to a target.
    pub const WS_CONNECT: &str = "ws_connect";
    /// Start port forwarding (proxy).
    pub const PROXY_START: &str = "proxy_start";
    /// Stop port forwarding.
    pub const PROXY_STOP: &str = "proxy_stop";
    /// Expose a local port to the tailnet.
    pub const EXPOSE_START: &str = "expose_start";
    /// Stop exposing a local port.
    pub const EXPOSE_STOP: &str = "expose_stop";
    /// Ping a node for connectivity check.
    pub const PING: &str = "ping";
    /// Send a mesh message to a specific device.
    pub const SEND_MESSAGE: &str = "send_message";
    /// Push a file to a remote node via Taildrop.
    pub const PUSH_FILE: &str = "push_file";
    /// Get (download) a file from a remote node.
    pub const GET_FILE: &str = "get_file";
}

// ═══════════════════════════════════════════════════════════════════════════
// Error codes
// ═══════════════════════════════════════════════════════════════════════════

/// Standard JSON-RPC error codes.
pub mod error_code {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i32 = -32700;
    /// The JSON sent is not a valid Request object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist / is not available.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid method parameter(s).
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i32 = -32603;

    // Application-specific error codes (positive numbers).

    /// The daemon is not running.
    pub const DAEMON_NOT_RUNNING: i32 = 1;
    /// The target node was not found.
    pub const NODE_NOT_FOUND: i32 = 2;
    /// The operation timed out.
    pub const TIMEOUT: i32 = 3;
    /// The node is not online yet.
    pub const NOT_ONLINE: i32 = 4;
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_request_serialization() {
        let req = DaemonRequest::new(1, "status", serde_json::json!({}));
        let json = serde_json::to_string(&req).unwrap();
        let parsed: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.jsonrpc, "2.0");
        assert_eq!(parsed.id, 1);
        assert_eq!(parsed.method, "status");
    }

    #[test]
    fn test_daemon_request_with_params() {
        let req = DaemonRequest::new(
            42,
            "send_message",
            serde_json::json!({
                "device_id": "abc-123",
                "namespace": "app",
                "type": "hello",
                "payload": { "text": "hi" }
            }),
        );
        let json = serde_json::to_string(&req).unwrap();
        let parsed: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 42);
        assert_eq!(parsed.method, "send_message");
        assert_eq!(parsed.params["device_id"], "abc-123");
    }

    #[test]
    fn test_daemon_response_serialization() {
        let resp = DaemonResponse::success(
            1,
            serde_json::json!({
                "device_id": "abc-123",
                "ip": "100.64.0.5",
                "status": "online"
            }),
        );
        let json = serde_json::to_string(&resp).unwrap();

        // Success response should not have "error" field
        assert!(!json.contains("\"error\""));

        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.jsonrpc, "2.0");
        assert_eq!(parsed.id, 1);
        assert!(parsed.result.is_some());
        assert!(parsed.error.is_none());
        assert_eq!(parsed.result.unwrap()["status"], "online");
    }

    #[test]
    fn test_daemon_error_response() {
        let resp = DaemonResponse::error(
            5,
            error_code::METHOD_NOT_FOUND,
            "Method 'foobar' not found",
        );
        let json = serde_json::to_string(&resp).unwrap();

        // Error response should not have "result" field
        assert!(!json.contains("\"result\""));

        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 5);
        assert!(parsed.result.is_none());
        assert!(parsed.error.is_some());
        let err = parsed.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method 'foobar' not found");
    }

    #[test]
    fn test_daemon_request_default_params() {
        // Parsing a request without "params" should default to empty object
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"status"}"#;
        let req: DaemonRequest = serde_json::from_str(json).unwrap();
        assert!(req.params.is_object());
        assert!(req.params.as_object().unwrap().is_empty());
    }
}
