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
    /// If true, the node is ephemeral and will be cleaned up when it goes offline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
    /// ACL tags to advertise (e.g. ["tag:truffle"]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
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
    pub const NEEDS_APPROVAL: &str = "tsnet:needsApproval";
    pub const STATE_CHANGE: &str = "tsnet:stateChange";
    pub const KEY_EXPIRING: &str = "tsnet:keyExpiring";
    pub const HEALTH_WARNING: &str = "tsnet:healthWarning";
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

/// Identity of a connecting peer, resolved via WhoIs in the Go sidecar.
/// Encoded as JSON in the bridge header's `remote_dns_name` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerIdentity {
    /// Tailscale DNS name (FQDN without trailing dot).
    #[serde(default)]
    pub dns_name: String,
    /// Login name (e.g. "alice@example.com").
    #[serde(default)]
    pub login_name: String,
    /// Display name (e.g. "Alice Smith").
    #[serde(default)]
    pub display_name: String,
    /// Profile picture URL.
    #[serde(default)]
    pub profile_pic_url: String,
    /// Tailscale stable node ID.
    #[serde(default)]
    pub node_id: String,
}

/// Data from `tsnet:stateChange` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateChangeEventData {
    pub state: String,
    #[serde(default)]
    pub auth_url: String,
}

/// Data from `tsnet:keyExpiring` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyExpiringEventData {
    /// ISO 8601 timestamp when the key expires.
    pub expires_at: String,
    /// Seconds remaining until expiry.
    #[serde(default)]
    pub expires_in_secs: i64,
}

/// Data from `tsnet:healthWarning` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthWarningEventData {
    pub warnings: Vec<String>,
}

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
    /// Direct address (e.g. "192.168.1.5:41641"), empty if relayed.
    #[serde(default)]
    pub cur_addr: String,
    /// DERP relay region (e.g. "sea"), empty if direct.
    #[serde(default)]
    pub relay: String,
    /// Last seen timestamp (RFC 3339 string from Go sidecar).
    #[serde(default)]
    pub last_seen: Option<String>,
    /// Key expiry timestamp (RFC 3339 string from Go sidecar).
    #[serde(default)]
    pub key_expiry: Option<String>,
    /// Whether the key has expired.
    #[serde(default)]
    pub expired: bool,
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
            cur_addr: if self.cur_addr.is_empty() { None } else { Some(self.cur_addr.clone()) },
            relay: if self.relay.is_empty() { None } else { Some(self.relay.clone()) },
            last_seen: self.last_seen.clone(),
            key_expiry: self.key_expiry.clone(),
            expired: self.expired,
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
            ephemeral: None,
            tags: None,
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

    // ── Layer 2: Extended event parsing tests ────────────────────────────

    #[test]
    fn deserialize_started_event() {
        let json = r#"{"event":"tsnet:started","data":{"state":"running","hostname":"n1","dnsName":"n1.ts.net","tailscaleIP":"100.64.0.1"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::STARTED);
        let data: StatusEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.state, "running");
        assert_eq!(data.tailscale_ip, "100.64.0.1");
    }

    #[test]
    fn deserialize_stopped_event() {
        let json = r#"{"event":"tsnet:stopped","data":null}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::STOPPED);
    }

    #[test]
    fn deserialize_stopped_event_no_data_field() {
        // Go emits `{"event":"tsnet:stopped"}` with no data key at all
        let json = r#"{"event":"tsnet:stopped"}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::STOPPED);
        assert!(event.data.is_null());
    }

    #[test]
    fn deserialize_error_event() {
        let json = r#"{"event":"tsnet:error","data":{"code":"LISTEN_ERROR","message":"ListenTLS :443: bind failed"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::ERROR);
        let data: ErrorEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.code, "LISTEN_ERROR");
        assert!(data.message.contains("bind failed"));
    }

    #[test]
    fn deserialize_status_starting() {
        let json = r#"{"event":"tsnet:status","data":{"state":"starting","hostname":"test-node"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        let data: StatusEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.state, "starting");
        assert_eq!(data.hostname, "test-node");
        // Optional fields default to empty string
        assert_eq!(data.dns_name, "");
        assert_eq!(data.tailscale_ip, "");
        assert_eq!(data.error, "");
    }

    #[test]
    fn deserialize_status_error_state() {
        let json = r#"{"event":"tsnet:status","data":{"state":"error","error":"failed to bind"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        let data: StatusEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.state, "error");
        assert_eq!(data.error, "failed to bind");
    }

    #[test]
    fn status_event_tailscale_ip_alias() {
        // Go sends "tailscaleIP" (camelCase with uppercase IP).
        // StatusEventData uses #[serde(alias = "tailscaleIP")] on tailscale_ip.
        let json = r#"{"state":"running","hostname":"n","dnsName":"n.ts","tailscaleIP":"100.64.0.5"}"#;
        let data: StatusEventData = serde_json::from_str(json).unwrap();
        assert_eq!(data.tailscale_ip, "100.64.0.5");
    }

    #[test]
    fn deserialize_peers_event_multiple_peers() {
        let json = r#"{"event":"tsnet:peers","data":{"peers":[
            {"id":"p1","hostname":"host1","dnsName":"host1.ts.net","tailscaleIPs":["100.64.0.2","fd7a::1"],"online":true,"os":"linux"},
            {"id":"p2","hostname":"host2","dnsName":"host2.ts.net","tailscaleIPs":["100.64.0.3"],"online":false}
        ]}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        let data: PeersEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.peers.len(), 2);

        assert_eq!(data.peers[0].id, "p1");
        assert_eq!(data.peers[0].tailscale_ips, vec!["100.64.0.2", "fd7a::1"]);
        assert!(data.peers[0].online);
        assert_eq!(data.peers[0].os, "linux");

        assert_eq!(data.peers[1].id, "p2");
        assert!(!data.peers[1].online);
        // os defaults to empty string when missing
        assert_eq!(data.peers[1].os, "");
    }

    #[test]
    fn deserialize_peers_event_empty_list() {
        let json = r#"{"event":"tsnet:peers","data":{"peers":[]}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        let data: PeersEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.peers.len(), 0);
    }

    #[test]
    fn bridge_peer_to_canonical_with_os() {
        let peer = BridgeTailnetPeer {
            id: "abc".to_string(),
            hostname: "my-host".to_string(),
            dns_name: "my-host.tail.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.1".to_string(), "fd7a::2".to_string()],
            online: true,
            os: "darwin".to_string(),
            cur_addr: String::new(),
            relay: String::new(),
            last_seen: None,
            key_expiry: None,
            expired: false,
        };
        let c = peer.to_canonical();
        assert_eq!(c.id, "abc");
        assert_eq!(c.hostname, "my-host");
        assert_eq!(c.dns_name, "my-host.tail.ts.net");
        assert_eq!(c.tailscale_ips.len(), 2);
        assert!(c.online);
        assert_eq!(c.os, Some("darwin".to_string()));
    }

    #[test]
    fn bridge_peer_to_canonical_without_os() {
        let peer = BridgeTailnetPeer {
            id: "xyz".to_string(),
            hostname: "h".to_string(),
            dns_name: "h.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.9".to_string()],
            online: false,
            os: String::new(),
            cur_addr: String::new(),
            relay: String::new(),
            last_seen: None,
            key_expiry: None,
            expired: false,
        };
        let c = peer.to_canonical();
        assert!(!c.online);
        assert_eq!(c.os, None);
    }

    #[test]
    fn bridge_peer_to_canonical_empty_ips() {
        let peer = BridgeTailnetPeer {
            id: "e".to_string(),
            hostname: "e".to_string(),
            dns_name: "e.ts.net".to_string(),
            tailscale_ips: vec![],
            online: false,
            os: String::new(),
            cur_addr: String::new(),
            relay: String::new(),
            last_seen: None,
            key_expiry: None,
            expired: false,
        };
        let c = peer.to_canonical();
        assert!(c.tailscale_ips.is_empty());
    }

    #[test]
    fn unknown_event_type_parses_as_shim_event() {
        // The ShimEvent envelope should parse fine even for events we don't know about.
        let json = r#"{"event":"tsnet:unknown_future_event","data":{"foo":"bar"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, "tsnet:unknown_future_event");
        assert_eq!(event.data["foo"], "bar");
    }

    #[test]
    fn malformed_json_fails_gracefully() {
        let bad = r#"{"event": "tsnet:status", "data": }"#;
        let result = serde_json::from_str::<ShimEvent>(bad);
        assert!(result.is_err());
    }

    #[test]
    fn empty_json_object_missing_event() {
        let json = r#"{}"#;
        let result = serde_json::from_str::<ShimEvent>(json);
        // "event" is required (no default), so this should fail
        assert!(result.is_err());
    }

    #[test]
    fn serialize_get_peers_command() {
        let cmd = ShimCommand {
            command: command_type::GET_PEERS,
            data: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"command":"tsnet:getPeers"}"#);
    }

    #[test]
    fn serialize_start_command_with_auth_key() {
        let data = StartCommandData {
            hostname: "node".to_string(),
            state_dir: "/tmp/ts".to_string(),
            auth_key: Some("tskey-auth-xxxxx".to_string()),
            bridge_port: 9999,
            session_token: "bb".repeat(32),
            ephemeral: None,
            tags: None,
        };
        let cmd = ShimCommand {
            command: command_type::START,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"authKey\":\"tskey-auth-xxxxx\""));
    }

    #[test]
    fn serialize_start_command_without_auth_key_omits_field() {
        let data = StartCommandData {
            hostname: "node".to_string(),
            state_dir: "/tmp/ts".to_string(),
            auth_key: None,
            bridge_port: 9999,
            session_token: "cc".repeat(32),
            ephemeral: None,
            tags: None,
        };
        let value = serde_json::to_value(&data).unwrap();
        // authKey should not appear at all when None
        assert!(!value.as_object().unwrap().contains_key("authKey"));
    }

    #[test]
    fn serialize_dial_command_fields() {
        let data = DialCommandData {
            request_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            target: "peer.tail.ts.net".to_string(),
            port: 9417,
        };
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["requestId"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(value["target"], "peer.tail.ts.net");
        assert_eq!(value["port"], 9417);
    }

    #[test]
    fn dial_result_success_default_error_is_empty() {
        // When success=true, the "error" field may be absent. It should default to "".
        let json = r#"{"requestId":"r1","success":true}"#;
        let data: DialResultEventData = serde_json::from_str(json).unwrap();
        assert!(data.success);
        assert_eq!(data.error, "");
    }

    #[test]
    fn error_event_data_fields() {
        let json = r#"{"code":"PARSE_ERROR","message":"failed to parse command"}"#;
        let data: ErrorEventData = serde_json::from_str(json).unwrap();
        assert_eq!(data.code, "PARSE_ERROR");
        assert_eq!(data.message, "failed to parse command");
    }

    #[test]
    fn status_event_missing_optional_fields_defaults() {
        // Only "state" is present; all optional fields should default to "".
        let json = r#"{"state":"starting"}"#;
        let data: StatusEventData = serde_json::from_str(json).unwrap();
        assert_eq!(data.state, "starting");
        assert_eq!(data.hostname, "");
        assert_eq!(data.dns_name, "");
        assert_eq!(data.tailscale_ip, "");
        assert_eq!(data.error, "");
    }

    #[test]
    fn event_type_constants_match_go_sidecar() {
        // Verify the string constants match what the Go sidecar sends.
        assert_eq!(event_type::STARTED, "tsnet:started");
        assert_eq!(event_type::STOPPED, "tsnet:stopped");
        assert_eq!(event_type::STATUS, "tsnet:status");
        assert_eq!(event_type::AUTH_REQUIRED, "tsnet:authRequired");
        assert_eq!(event_type::NEEDS_APPROVAL, "tsnet:needsApproval");
        assert_eq!(event_type::STATE_CHANGE, "tsnet:stateChange");
        assert_eq!(event_type::KEY_EXPIRING, "tsnet:keyExpiring");
        assert_eq!(event_type::HEALTH_WARNING, "tsnet:healthWarning");
        assert_eq!(event_type::PEERS, "tsnet:peers");
        assert_eq!(event_type::DIAL_RESULT, "bridge:dialResult");
        assert_eq!(event_type::ERROR, "tsnet:error");
    }

    #[test]
    fn command_type_constants_match_go_sidecar() {
        assert_eq!(command_type::START, "tsnet:start");
        assert_eq!(command_type::STOP, "tsnet:stop");
        assert_eq!(command_type::GET_PEERS, "tsnet:getPeers");
        assert_eq!(command_type::DIAL, "bridge:dial");
    }

    // ── Phase 2: New event types ──────────────────────────────────────────

    #[test]
    fn deserialize_state_change_event() {
        let json = r#"{"event":"tsnet:stateChange","data":{"state":"NeedsLogin"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::STATE_CHANGE);
        let data: StateChangeEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.state, "NeedsLogin");
        assert_eq!(data.auth_url, "");
    }

    #[test]
    fn deserialize_key_expiring_event() {
        let json = r#"{"event":"tsnet:keyExpiring","data":{"expiresAt":"2026-04-01T12:00:00Z","expiresInSecs":3600}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::KEY_EXPIRING);
        let data: KeyExpiringEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.expires_at, "2026-04-01T12:00:00Z");
        assert_eq!(data.expires_in_secs, 3600);
    }

    #[test]
    fn deserialize_health_warning_event() {
        let json = r#"{"event":"tsnet:healthWarning","data":{"warnings":["no route to host","DNS timeout"]}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::HEALTH_WARNING);
        let data: HealthWarningEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.warnings.len(), 2);
        assert_eq!(data.warnings[0], "no route to host");
    }

    #[test]
    fn deserialize_needs_approval_event() {
        let json = r#"{"event":"tsnet:needsApproval","data":null}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::NEEDS_APPROVAL);
    }

    #[test]
    fn deserialize_peer_identity() {
        let json = r#"{"dnsName":"peer1.tail.ts.net","loginName":"alice@example.com","displayName":"Alice","profilePicUrl":"https://example.com/pic.jpg","nodeId":"stable-123"}"#;
        let identity: PeerIdentity = serde_json::from_str(json).unwrap();
        assert_eq!(identity.dns_name, "peer1.tail.ts.net");
        assert_eq!(identity.login_name, "alice@example.com");
        assert_eq!(identity.display_name, "Alice");
        assert_eq!(identity.profile_pic_url, "https://example.com/pic.jpg");
        assert_eq!(identity.node_id, "stable-123");
    }

    #[test]
    fn deserialize_peer_identity_minimal() {
        let json = r#"{"dnsName":"peer1.tail.ts.net"}"#;
        let identity: PeerIdentity = serde_json::from_str(json).unwrap();
        assert_eq!(identity.dns_name, "peer1.tail.ts.net");
        assert_eq!(identity.login_name, "");
        assert_eq!(identity.display_name, "");
    }

    #[test]
    fn serialize_start_command_with_ephemeral_and_tags() {
        let data = StartCommandData {
            hostname: "node".to_string(),
            state_dir: "/tmp/ts".to_string(),
            auth_key: None,
            bridge_port: 9999,
            session_token: "dd".repeat(32),
            ephemeral: Some(true),
            tags: Some(vec!["tag:truffle".to_string()]),
        };
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["ephemeral"], true);
        assert_eq!(value["tags"][0], "tag:truffle");
    }

    #[test]
    fn serialize_start_command_ephemeral_none_omitted() {
        let data = StartCommandData {
            hostname: "node".to_string(),
            state_dir: "/tmp/ts".to_string(),
            auth_key: None,
            bridge_port: 9999,
            session_token: "ee".repeat(32),
            ephemeral: None,
            tags: None,
        };
        let value = serde_json::to_value(&data).unwrap();
        assert!(!value.as_object().unwrap().contains_key("ephemeral"));
        assert!(!value.as_object().unwrap().contains_key("tags"));
    }

    #[test]
    fn deserialize_state_change_event_with_all_fields() {
        let json = r#"{"event":"tsnet:stateChange","data":{"state":"NeedsLogin","authUrl":"https://login.tailscale.com/a/xyz"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event, event_type::STATE_CHANGE);
        let data: StateChangeEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.state, "NeedsLogin");
        assert_eq!(data.auth_url, "https://login.tailscale.com/a/xyz");
    }

    #[test]
    fn deserialize_state_change_event_empty_auth_url_defaults() {
        // When authUrl is absent from JSON, it should default to empty string
        let json = r#"{"event":"tsnet:stateChange","data":{"state":"Running"}}"#;
        let event: ShimEvent = serde_json::from_str(json).unwrap();
        let data: StateChangeEventData = serde_json::from_value(event.data).unwrap();
        assert_eq!(data.state, "Running");
        assert_eq!(data.auth_url, "", "authUrl should default to empty string when absent");
    }

    #[test]
    fn serialize_start_command_with_tags_only() {
        let data = StartCommandData {
            hostname: "tagged-node".to_string(),
            state_dir: "/tmp/ts".to_string(),
            auth_key: None,
            bridge_port: 8080,
            session_token: "ff".repeat(32),
            ephemeral: None,
            tags: Some(vec!["tag:truffle".to_string(), "tag:server".to_string()]),
        };
        let value = serde_json::to_value(&data).unwrap();
        // ephemeral omitted, tags present
        assert!(!value.as_object().unwrap().contains_key("ephemeral"),
            "ephemeral=None should be omitted from JSON");
        let tags = value["tags"].as_array().unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0], "tag:truffle");
        assert_eq!(tags[1], "tag:server");
    }

    #[test]
    fn deserialize_bridge_peer_without_rich_fields_defaults() {
        // JSON with only the base fields; new rich fields should all default
        let json = r#"{"id":"p1","hostname":"h","dnsName":"h.ts.net","tailscaleIPs":["100.64.0.2"],"online":true}"#;
        let peer: BridgeTailnetPeer = serde_json::from_str(json).unwrap();
        assert_eq!(peer.id, "p1");
        assert!(peer.online);
        // Rich fields default to empty/None/false
        assert_eq!(peer.cur_addr, "");
        assert_eq!(peer.relay, "");
        assert_eq!(peer.last_seen, None);
        assert_eq!(peer.key_expiry, None);
        assert!(!peer.expired);
    }

    #[test]
    fn to_canonical_maps_empty_strings_to_none() {
        let peer = BridgeTailnetPeer {
            id: "t1".to_string(),
            hostname: "h".to_string(),
            dns_name: "h.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.1".to_string()],
            online: true,
            os: String::new(),
            cur_addr: String::new(),
            relay: String::new(),
            last_seen: None,
            key_expiry: None,
            expired: false,
        };
        let c = peer.to_canonical();
        assert_eq!(c.os, None, "Empty os string should map to None");
        assert_eq!(c.cur_addr, None, "Empty curAddr string should map to None");
        assert_eq!(c.relay, None, "Empty relay string should map to None");
    }

    #[test]
    fn to_canonical_preserves_non_empty_cur_addr_and_relay() {
        let peer = BridgeTailnetPeer {
            id: "t2".to_string(),
            hostname: "h".to_string(),
            dns_name: "h.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.2".to_string()],
            online: true,
            os: "linux".to_string(),
            cur_addr: "10.0.0.5:41641".to_string(),
            relay: "nyc".to_string(),
            last_seen: Some("2026-03-18T10:00:00Z".to_string()),
            key_expiry: Some("2026-06-18T10:00:00Z".to_string()),
            expired: false,
        };
        let c = peer.to_canonical();
        assert_eq!(c.cur_addr, Some("10.0.0.5:41641".to_string()));
        assert_eq!(c.relay, Some("nyc".to_string()));
        assert_eq!(c.last_seen, Some("2026-03-18T10:00:00Z".to_string()));
        assert_eq!(c.key_expiry, Some("2026-06-18T10:00:00Z".to_string()));
        assert!(!c.expired);
    }

    #[test]
    fn to_canonical_expired_flag() {
        let peer = BridgeTailnetPeer {
            id: "exp".to_string(),
            hostname: "old".to_string(),
            dns_name: "old.ts.net".to_string(),
            tailscale_ips: vec![],
            online: false,
            os: String::new(),
            cur_addr: String::new(),
            relay: String::new(),
            last_seen: Some("2025-01-01T00:00:00Z".to_string()),
            key_expiry: Some("2025-06-01T00:00:00Z".to_string()),
            expired: true,
        };
        let c = peer.to_canonical();
        assert!(c.expired, "Expired flag should be preserved");
    }

    #[test]
    fn event_type_new_constants_match_expected() {
        assert_eq!(event_type::NEEDS_APPROVAL, "tsnet:needsApproval");
        assert_eq!(event_type::STATE_CHANGE, "tsnet:stateChange");
        assert_eq!(event_type::KEY_EXPIRING, "tsnet:keyExpiring");
        assert_eq!(event_type::HEALTH_WARNING, "tsnet:healthWarning");
    }

    #[test]
    fn deserialize_bridge_peer_with_rich_fields() {
        let json = r#"{"id":"p1","hostname":"h","dnsName":"h.ts.net","tailscaleIPs":["100.64.0.2"],"online":true,"curAddr":"192.168.1.5:41641","relay":"sea","lastSeen":"2026-03-18T12:00:00Z","keyExpiry":"2026-04-18T12:00:00Z","expired":false}"#;
        let peer: BridgeTailnetPeer = serde_json::from_str(json).unwrap();
        assert_eq!(peer.cur_addr, "192.168.1.5:41641");
        assert_eq!(peer.relay, "sea");
        assert_eq!(peer.last_seen, Some("2026-03-18T12:00:00Z".to_string()));
        assert_eq!(peer.key_expiry, Some("2026-04-18T12:00:00Z".to_string()));
        assert!(!peer.expired);

        let canonical = peer.to_canonical();
        assert_eq!(canonical.cur_addr, Some("192.168.1.5:41641".to_string()));
        assert_eq!(canonical.relay, Some("sea".to_string()));
        assert_eq!(canonical.last_seen, Some("2026-03-18T12:00:00Z".to_string()));
        assert_eq!(canonical.key_expiry, Some("2026-04-18T12:00:00Z".to_string()));
    }
}
