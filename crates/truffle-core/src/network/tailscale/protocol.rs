//! JSON command/event protocol types for Go sidecar communication.
//!
//! Commands are sent Rust -> Go via stdin (JSON lines).
//! Events are received Go -> Rust via stdout (JSON lines).
//!
//! These types match the wire format defined in `packages/sidecar-slim/main.go`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Commands: Rust → Go sidecar (JSON lines on stdin)
// ---------------------------------------------------------------------------

/// Envelope for all commands sent to the Go sidecar.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SidecarCommand {
    pub command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Data payload for `tsnet:start`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartCommandData {
    pub hostname: String,
    pub state_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_key: Option<String>,
    pub bridge_port: u16,
    pub session_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// Data payload for `bridge:dial`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DialCommandData {
    pub request_id: String,
    pub target: String,
    pub port: u16,
}

/// Data payload for `tsnet:listen`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListenCommandData {
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<bool>,
}

/// Data payload for `tsnet:unlisten`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UnlistenCommandData {
    pub port: u16,
}

/// Data payload for `tsnet:ping`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PingCommandData {
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ping_type: Option<String>,
}

/// Data payload for `tsnet:watchPeers`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WatchPeersCommandData {
    /// If true, also include non-truffle peers in events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_all: Option<bool>,
}

/// Data payload for `tsnet:listenPacket`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListenPacketCommandData {
    pub port: u16,
}

// ---------------------------------------------------------------------------
// Well-known command type strings
// ---------------------------------------------------------------------------

pub(crate) mod command_type {
    pub const START: &str = "tsnet:start";
    pub const STOP: &str = "tsnet:stop";
    pub const GET_PEERS: &str = "tsnet:getPeers";
    pub const DIAL: &str = "bridge:dial";
    pub const LISTEN: &str = "tsnet:listen";
    pub const UNLISTEN: &str = "tsnet:unlisten";
    pub const PING: &str = "tsnet:ping";
    pub const WATCH_PEERS: &str = "tsnet:watchPeers";
    pub const LISTEN_PACKET: &str = "tsnet:listenPacket";
}

// ---------------------------------------------------------------------------
// Events: Go sidecar → Rust (JSON lines on stdout)
// ---------------------------------------------------------------------------

/// Envelope for all events received from the Go sidecar.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SidecarEvent {
    pub event: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Well-known event type strings
// ---------------------------------------------------------------------------

pub(crate) mod event_type {
    pub const STARTED: &str = "tsnet:started";
    pub const STOPPED: &str = "tsnet:stopped";
    pub const STATUS: &str = "tsnet:status";
    pub const AUTH_REQUIRED: &str = "tsnet:authRequired";
    pub const NEEDS_APPROVAL: &str = "tsnet:needsApproval";
    pub const STATE_CHANGE: &str = "tsnet:stateChange";
    pub const KEY_EXPIRING: &str = "tsnet:keyExpiring";
    pub const HEALTH_WARNING: &str = "tsnet:healthWarning";
    pub const PEERS: &str = "tsnet:peers";
    pub const DIAL_RESULT: &str = "bridge:dialResult";
    pub const ERROR: &str = "tsnet:error";
    pub const LISTENING: &str = "tsnet:listening";
    pub const UNLISTENED: &str = "tsnet:unlistened";
    pub const PING_RESULT: &str = "tsnet:pingResult";
    pub const PEER_CHANGED: &str = "tsnet:peerChanged";
    pub const LISTENING_PACKET: &str = "tsnet:listeningPacket";
}

// ---------------------------------------------------------------------------
// Event data payloads
// ---------------------------------------------------------------------------

/// Data from `tsnet:status` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusEventData {
    pub state: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub dns_name: String,
    #[serde(default, alias = "tailscaleIP")]
    pub tailscale_ip: String,
    /// Tailscale stable node ID (e.g. "nv8m2uw7se11CNTRL").
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub error: String,
}

/// Data from `tsnet:authRequired` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthRequiredEventData {
    pub auth_url: String,
}

/// Data from `tsnet:stateChange` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StateChangeEventData {
    pub state: String,
}

/// Data from `tsnet:keyExpiring` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyExpiringEventData {
    pub expires_at: String,
}

/// Data from `tsnet:healthWarning` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HealthWarningEventData {
    pub warnings: Vec<String>,
}

/// A peer as reported by the Go sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SidecarPeer {
    pub id: String,
    pub hostname: String,
    pub dns_name: String,
    #[serde(rename = "tailscaleIPs")]
    pub tailscale_ips: Vec<String>,
    pub online: bool,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub cur_addr: String,
    #[serde(default)]
    pub relay: String,
    #[serde(default)]
    pub last_seen: Option<String>,
    #[serde(default)]
    pub key_expiry: Option<String>,
    #[serde(default)]
    pub expired: bool,
}

/// Data from `tsnet:peers` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PeersEventData {
    pub peers: Vec<SidecarPeer>,
}

/// Data from `bridge:dialResult` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DialResultEventData {
    pub request_id: String,
    pub success: bool,
    #[serde(default)]
    pub error: String,
}

/// Data from `tsnet:error` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ErrorEventData {
    pub code: String,
    pub message: String,
}

/// Data from `tsnet:listening` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListeningEventData {
    pub port: u16,
}

/// Data from `tsnet:unlistened` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UnlistenedEventData {
    pub port: u16,
}

/// Data from `tsnet:listeningPacket` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListeningPacketEventData {
    /// The tsnet-bound port (the one requested by Rust).
    pub port: u16,
    /// The local relay port that Rust should send/recv datagrams to/from.
    pub local_port: u16,
}

/// Data from `tsnet:pingResult` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PingResultEventData {
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub latency_ms: f64,
    #[serde(default)]
    pub direct: bool,
    #[serde(default)]
    pub relay: String,
    #[serde(default)]
    pub peer_addr: String,
    #[serde(default)]
    pub error: String,
}

/// Data from `tsnet:peerChanged` event (from WatchIPNBus).
///
/// Represents a single peer change notification.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PeerChangedEventData {
    /// "joined", "left", or "updated"
    pub change_type: String,
    /// The peer data (absent for "left" events).
    pub peer: Option<SidecarPeer>,
    /// Peer ID (always present, used for "left" events).
    pub peer_id: String,
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
            ephemeral: None,
            tags: None,
        };
        let cmd = SidecarCommand {
            command: command_type::START,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"tsnet:start\""));
        assert!(json.contains("\"hostname\":\"my-node\""));
        assert!(json.contains("\"bridgePort\":12345"));
        // auth_key should be absent (None -> skip)
        assert!(!json.contains("authKey"));
    }

    #[test]
    fn serialize_dial_command() {
        let data = DialCommandData {
            request_id: "req-123".to_string(),
            target: "peer.tailnet.ts.net".to_string(),
            port: 9417,
        };
        let cmd = SidecarCommand {
            command: command_type::DIAL,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"bridge:dial\""));
        assert!(json.contains("\"requestId\":\"req-123\""));
        assert!(json.contains("\"port\":9417"));
    }

    #[test]
    fn serialize_listen_command() {
        let data = ListenCommandData {
            port: 8080,
            tls: None,
        };
        let cmd = SidecarCommand {
            command: command_type::LISTEN,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"tsnet:listen\""));
        assert!(json.contains("\"port\":8080"));
        assert!(!json.contains("tls"));
    }

    #[test]
    fn serialize_ping_command() {
        let data = PingCommandData {
            target: "100.64.0.2".to_string(),
            ping_type: Some("TSMP".to_string()),
        };
        let cmd = SidecarCommand {
            command: command_type::PING,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"tsnet:ping\""));
        assert!(json.contains("\"target\":\"100.64.0.2\""));
        assert!(json.contains("\"pingType\":\"TSMP\""));
    }

    #[test]
    fn serialize_watch_peers_command() {
        let data = WatchPeersCommandData { include_all: None };
        let cmd = SidecarCommand {
            command: command_type::WATCH_PEERS,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"tsnet:watchPeers\""));
    }

    #[test]
    fn deserialize_status_event() {
        let json = r#"{"event":"tsnet:status","data":{"state":"running","hostname":"my-node","dnsName":"my-node.tailnet.ts.net","tailscaleIP":"100.64.0.1"}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, "tsnet:status");
        let data: StatusEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.state, "running");
        assert_eq!(data.hostname, "my-node");
        assert_eq!(data.tailscale_ip, "100.64.0.1");
    }

    #[test]
    fn deserialize_peers_event() {
        let json = r#"{"event":"tsnet:peers","data":{"peers":[{"id":"node123","hostname":"truffle-cli-abc","dnsName":"truffle-cli-abc.tailnet.ts.net","tailscaleIPs":["100.64.0.2"],"online":true,"os":"linux","curAddr":"192.168.1.5:41641","relay":"","lastSeen":"2026-03-24T10:00:00Z","keyExpiry":"2026-06-24T10:00:00Z","expired":false}]}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, "tsnet:peers");
        let data: PeersEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.peers.len(), 1);
        assert_eq!(data.peers[0].id, "node123");
        assert_eq!(data.peers[0].hostname, "truffle-cli-abc");
        assert!(data.peers[0].online);
    }

    #[test]
    fn deserialize_dial_result_event() {
        let json = r#"{"event":"bridge:dialResult","data":{"requestId":"req-456","success":true}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        let data: DialResultEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.request_id, "req-456");
        assert!(data.success);
        assert!(data.error.is_empty());
    }

    #[test]
    fn deserialize_dial_result_failure() {
        let json = r#"{"event":"bridge:dialResult","data":{"requestId":"req-789","success":false,"error":"connection refused"}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        let data: DialResultEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.request_id, "req-789");
        assert!(!data.success);
        assert_eq!(data.error, "connection refused");
    }

    #[test]
    fn deserialize_ping_result_event() {
        let json = r#"{"event":"tsnet:pingResult","data":{"target":"100.64.0.2","latencyMs":12.5,"direct":true,"relay":"","peerAddr":"192.168.1.5:41641"}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        let data: PingResultEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.target, "100.64.0.2");
        assert!((data.latency_ms - 12.5).abs() < f64::EPSILON);
        assert!(data.direct);
    }

    #[test]
    fn deserialize_peer_changed_event() {
        let json = r#"{"event":"tsnet:peerChanged","data":{"changeType":"joined","peerId":"node123","peer":{"id":"node123","hostname":"truffle-cli-abc","dnsName":"truffle-cli-abc.tailnet.ts.net","tailscaleIPs":["100.64.0.2"],"online":true}}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        let data: PeerChangedEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.change_type, "joined");
        assert_eq!(data.peer_id, "node123");
        assert!(data.peer.is_some());
    }

    #[test]
    fn deserialize_peer_left_event() {
        let json =
            r#"{"event":"tsnet:peerChanged","data":{"changeType":"left","peerId":"node456"}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        let data: PeerChangedEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.change_type, "left");
        assert_eq!(data.peer_id, "node456");
        assert!(data.peer.is_none());
    }

    #[test]
    fn deserialize_error_event() {
        let json =
            r#"{"event":"tsnet:error","data":{"code":"NOT_RUNNING","message":"node not running"}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        let data: ErrorEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.code, "NOT_RUNNING");
        assert_eq!(data.message, "node not running");
    }

    #[test]
    fn deserialize_auth_required_event() {
        let json = r#"{"event":"tsnet:authRequired","data":{"authUrl":"https://login.tailscale.com/a/abc123"}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        let data: AuthRequiredEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.auth_url, "https://login.tailscale.com/a/abc123");
    }

    #[test]
    fn deserialize_listening_event() {
        let json = r#"{"event":"tsnet:listening","data":{"port":8080}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        let data: ListeningEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.port, 8080);
    }

    #[test]
    fn deserialize_health_warning_event() {
        let json = r#"{"event":"tsnet:healthWarning","data":{"warnings":["DNS is failing","MagicDNS not working"]}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        let data: HealthWarningEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.warnings.len(), 2);
    }

    #[test]
    fn serialize_listen_packet_command() {
        let data = ListenPacketCommandData { port: 19420 };
        let cmd = SidecarCommand {
            command: command_type::LISTEN_PACKET,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"tsnet:listenPacket\""));
        assert!(json.contains("\"port\":19420"));
    }

    #[test]
    fn deserialize_listening_packet_event() {
        let json = r#"{"event":"tsnet:listeningPacket","data":{"port":19420,"localPort":54321}}"#;
        let event: SidecarEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, "tsnet:listeningPacket");
        let data: ListeningPacketEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.port, 19420);
        assert_eq!(data.local_port, 54321);
    }
}
