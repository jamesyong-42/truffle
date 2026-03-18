use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Commands: Rust → Go shim (sent as JSON lines on stdin)
// ---------------------------------------------------------------------------

/// Envelope for all commands sent to the Go shim.
#[derive(Debug, Clone, Serialize)]
pub struct ShimCommand {
    pub command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Data payload for `tsnet:start`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartCommandData {
    pub hostname: String,
    pub state_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_key: Option<String>,
    /// Ephemeral port Rust is listening on for bridge connections.
    pub bridge_port: u16,
    /// Random 32-byte hex token for bridge authentication.
    pub session_token: String,
}

/// Data payload for `bridge:dial`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DialCommandData {
    /// UUID v4 to correlate the dial result back to the caller.
    pub request_id: String,
    /// Tailscale DNS name or IP of the target.
    pub target: String,
    /// Port to connect to (443 or 9417).
    pub port: u16,
}

// ---------------------------------------------------------------------------
// Events: Go shim → Rust (received as JSON lines on stdout)
// ---------------------------------------------------------------------------

/// Envelope for all events received from the Go shim.
#[derive(Debug, Clone, Deserialize)]
pub struct ShimEvent {
    pub event: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

/// Well-known event type strings.
pub mod event_type {
    pub const STARTED: &str = "tsnet:started";
    pub const STOPPED: &str = "tsnet:stopped";
    pub const STATUS: &str = "tsnet:status";
    pub const AUTH_REQUIRED: &str = "tsnet:authRequired";
    pub const PEERS: &str = "tsnet:peers";
    pub const DIAL_RESULT: &str = "bridge:dialResult";
    pub const ERROR: &str = "tsnet:error";
}

/// Well-known command type strings.
pub mod command_type {
    pub const START: &str = "tsnet:start";
    pub const STOP: &str = "tsnet:stop";
    pub const GET_PEERS: &str = "tsnet:getPeers";
    pub const DIAL: &str = "bridge:dial";
}

/// Data from `tsnet:status` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusEventData {
    pub state: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub dns_name: String,
    #[serde(default, alias = "tailscaleIP")]
    pub tailscale_ip: String,
    #[serde(default)]
    pub error: String,
}

/// Data from `tsnet:authRequired` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthRequiredEventData {
    pub auth_url: String,
}

use crate::types::TailnetPeer as CanonicalTailnetPeer;

/// Wire-format peer from Go sidecar. Converted to canonical TailnetPeer by the shim.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeTailnetPeer {
    pub id: String,
    pub hostname: String,
    pub dns_name: String,
    #[serde(rename = "tailscaleIPs")]
    pub tailscale_ips: Vec<String>,
    pub online: bool,
    #[serde(default)]
    pub os: String,
}

impl BridgeTailnetPeer {
    pub fn to_canonical(&self) -> CanonicalTailnetPeer {
        CanonicalTailnetPeer {
            id: self.id.clone(),
            hostname: self.hostname.clone(),
            dns_name: self.dns_name.clone(),
            tailscale_ips: self.tailscale_ips.clone(),
            online: self.online,
            os: if self.os.is_empty() { None } else { Some(self.os.clone()) },
        }
    }
}

/// Data from `tsnet:peers` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeersEventData {
    pub peers: Vec<BridgeTailnetPeer>,
}

/// Data from `bridge:dialResult` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DialResultEventData {
    pub request_id: String,
    pub success: bool,
    #[serde(default)]
    pub error: String,
}

/// Data from `tsnet:error` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEventData {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_start_command() {
        let data = StartCommandData {
            hostname: "my-node".to_string(),
            state_dir: "/tmp/tsnet".to_string(),
            auth_key: None,
            bridge_port: 12345,
            session_token: "aa".repeat(32),
        };
        let cmd = ShimCommand {
            command: command_type::START,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"tsnet:start\""));
        assert!(json.contains("\"bridgePort\":12345"));
        assert!(json.contains("\"sessionToken\""));
    }

    #[test]
    fn serialize_dial_command() {
        let data = DialCommandData {
            request_id: "test-uuid".to_string(),
            target: "peer.tailnet.ts.net".to_string(),
            port: 443,
        };
        let cmd = ShimCommand {
            command: command_type::DIAL,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"bridge:dial\""));
        assert!(json.contains("\"requestId\":\"test-uuid\""));
    }

    #[test]
    fn serialize_stop_command() {
        let cmd = ShimCommand {
            command: command_type::STOP,
            data: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"command":"tsnet:stop"}"#);
    }

    #[test]
    fn deserialize_status_event() {
        let json = r#"{"event":"tsnet:status","data":{"state":"running","hostname":"node1","dnsName":"node1.tailnet.ts.net","tailscaleIP":"100.64.0.1"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::STATUS);

        let data: StatusEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.state, "running");
        assert_eq!(data.dns_name, "node1.tailnet.ts.net");
    }

    #[test]
    fn deserialize_auth_required_event() {
        let json = r#"{"event":"tsnet:authRequired","data":{"authUrl":"https://login.tailscale.com/a/abc123"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::AUTH_REQUIRED);

        let data: AuthRequiredEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.auth_url, "https://login.tailscale.com/a/abc123");
    }

    #[test]
    fn deserialize_peers_event() {
        let json = r#"{"event":"tsnet:peers","data":{"peers":[{"id":"1","hostname":"peer1","dnsName":"peer1.ts.net","tailscaleIPs":["100.64.0.2"],"online":true}]}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        let data: PeersEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.peers.len(), 1);
        assert_eq!(data.peers[0].hostname, "peer1");
        assert!(data.peers[0].online);
    }

    #[test]
    fn deserialize_dial_result_success() {
        let json = r#"{"event":"bridge:dialResult","data":{"requestId":"uuid-123","success":true}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        let data: DialResultEventData = serde_json::from_value(event.data).unwrap();
        assert!(data.success);
        assert_eq!(data.request_id, "uuid-123");
    }

    #[test]
    fn deserialize_bridge_peer_to_canonical() {
        let json = r#"{"id":"1","hostname":"h","dnsName":"h.ts.net","tailscaleIPs":["100.64.0.2"],"online":true,"os":"linux"}"#;
        let peer: BridgeTailnetPeer = serde_json::from_str(json).unwrap();
        let canonical = peer.to_canonical();
        assert_eq!(canonical.os, Some("linux".into()));
        assert_eq!(canonical.tailscale_ips, vec!["100.64.0.2"]);
    }

    #[test]
    fn deserialize_bridge_peer_empty_os() {
        let json = r#"{"id":"1","hostname":"h","dnsName":"h.ts.net","tailscaleIPs":["100.64.0.2"],"online":true}"#;
        let peer: BridgeTailnetPeer = serde_json::from_str(json).unwrap();
        let canonical = peer.to_canonical();
        assert_eq!(canonical.os, None);
    }

    #[test]
    fn deserialize_dial_result_failure() {
        let json = r#"{"event":"bridge:dialResult","data":{"requestId":"uuid-456","success":false,"error":"connection refused"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        let data: DialResultEventData = serde_json::from_value(event.data).unwrap();
        assert!(!data.success);
        assert_eq!(data.error, "connection refused");
    }
}
