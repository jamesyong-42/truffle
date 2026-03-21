use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Device status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceStatus {
    Online,
    Offline,
    Connecting,
}

/// Tailscale authentication status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthStatus {
    /// Not yet known (sidecar not started or no auth info received).
    Unknown,
    /// Auth is required; the user must visit the URL to authenticate.
    Required,
    /// Authenticated successfully (sidecar reported a Tailscale IP).
    Authenticated,
}

/// A device in the mesh network.
/// Generic - device type and capabilities are strings, not fixed enums.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseDevice {
    /// Unique device identifier (persisted, never changes).
    pub id: String,
    /// Device type (e.g., "desktop", "mobile", "server") - application-defined.
    #[serde(rename = "type")]
    pub device_type: String,
    /// User-editable friendly name.
    pub name: String,
    /// Tailscale hostname: {prefix}-{type}-{id}.
    pub tailscale_hostname: String,
    /// Full MagicDNS name for TLS.
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "tailscaleDNSName"
    )]
    pub tailscale_dns_name: Option<String>,
    /// Tailscale IP address (assigned when connected).
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "tailscaleIP"
    )]
    pub tailscale_ip: Option<String>,
    /// Device status.
    pub status: DeviceStatus,
    /// Device capabilities - application-defined strings.
    pub capabilities: Vec<String>,
    /// Application-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    /// Last seen timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u64>,
    /// Startup timestamp (used for primary election).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    /// OS information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// Latency to this device in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
}

/// Tailnet peer from Tailscale API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TailnetPeer {
    pub id: String,
    pub hostname: String,
    #[serde(alias = "dnsName")]
    pub dns_name: String,
    #[serde(rename = "tailscaleIPs", alias = "tailscaleIps")]
    pub tailscale_ips: Vec<String>,
    pub online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// Direct address (e.g. "192.168.1.5:41641"), None if relayed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cur_addr: Option<String>,
    /// DERP relay region (e.g. "sea"), None if direct.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// Last seen timestamp (RFC 3339 string).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
    /// Key expiry timestamp (RFC 3339 string).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_expiry: Option<String>,
    /// Whether the key has expired.
    #[serde(default)]
    pub expired: bool,
}

/// Identity of a peer, extracted from dnsName metadata or Tailscale node info.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerIdentity {
    /// Tailscale node ID.
    #[serde(default)]
    pub node_id: String,
    /// MagicDNS name.
    #[serde(default)]
    pub dns_name: String,
    /// Whether this node uses ACL tags instead of a user identity.
    #[serde(default)]
    pub is_tagged: bool,
    /// User ID (absent for tagged nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// User display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Login name (e.g. "alice@example.com") from WhoIs UserProfile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_name: Option<String>,
    /// Profile picture URL from WhoIs UserProfile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_pic_url: Option<String>,
}

impl PeerIdentity {
    /// Try to parse a PeerIdentity from a remote DNS name field.
    ///
    /// Some implementations encode identity as JSON in the dnsName field.
    /// If the string is valid JSON containing at least `nodeId` and `dnsName`,
    /// it is parsed; otherwise returns None.
    pub fn from_remote_dns(dns_name: &str) -> Option<Self> {
        serde_json::from_str::<Self>(dns_name).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_status_serde_roundtrip() {
        let json = serde_json::to_string(&DeviceStatus::Online).unwrap();
        assert_eq!(json, "\"online\"");
        let parsed: DeviceStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, DeviceStatus::Online);
    }

    #[test]
    fn base_device_serde_roundtrip() {
        let device = BaseDevice {
            id: "abc123".to_string(),
            device_type: "desktop".to_string(),
            name: "My Desktop".to_string(),
            tailscale_hostname: "app-desktop-abc123".to_string(),
            tailscale_dns_name: Some("app-desktop-abc123.tailnet.ts.net".to_string()),
            tailscale_ip: Some("100.64.0.1".to_string()),
            status: DeviceStatus::Online,
            capabilities: vec!["file-transfer".to_string()],
            metadata: None,
            last_seen: None,
            started_at: Some(1000),
            os: Some("darwin".to_string()),
            latency_ms: None,
        };

        let json = serde_json::to_string(&device).unwrap();
        let parsed: BaseDevice = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, device);
    }

    #[test]
    fn deserialize_base_device_with_acronym_casing() {
        let json = r#"{
            "id":"d1","type":"desktop","name":"D",
            "tailscaleHostname":"h",
            "tailscaleDNSName":"d.ts.net",
            "tailscaleIP":"100.64.0.1",
            "status":"online","capabilities":[]
        }"#;
        let dev: BaseDevice = serde_json::from_str(json).unwrap();
        assert_eq!(dev.tailscale_dns_name, Some("d.ts.net".into()));
        assert_eq!(dev.tailscale_ip, Some("100.64.0.1".into()));
    }

    #[test]
    fn deserialize_tailnet_peer_from_go_format() {
        let json = r#"{
            "id":"1","hostname":"h","dnsName":"h.ts.net",
            "tailscaleIPs":["100.64.0.2"],"online":true,"os":"linux"
        }"#;
        let peer: TailnetPeer = serde_json::from_str(json).unwrap();
        assert_eq!(peer.os, Some("linux".into()));
        assert_eq!(peer.tailscale_ips, vec!["100.64.0.2"]);
    }

    #[test]
    fn base_device_optional_fields_omitted() {
        let device = BaseDevice {
            id: "abc".to_string(),
            device_type: "mobile".to_string(),
            name: "Phone".to_string(),
            tailscale_hostname: "app-mobile-abc".to_string(),
            tailscale_dns_name: None,
            tailscale_ip: None,
            status: DeviceStatus::Offline,
            capabilities: vec![],
            metadata: None,
            last_seen: None,
            started_at: None,
            os: None,
            latency_ms: None,
        };

        let json = serde_json::to_string(&device).unwrap();
        assert!(!json.contains("tailscaleDnsName"));
        assert!(!json.contains("tailscaleIp"));
    }

    // ── BUG-2 regression tests: serde case mismatches ──────────────────

    /// BUG-2: TS/Go sends "tailscaleIP" (uppercase IP). The Rust camelCase
    /// form is "tailscaleIp". Both must be accepted.
    #[test]
    fn base_device_deserializes_tailscale_ip_uppercase() {
        let json = r#"{
            "id": "dev-1", "type": "desktop", "name": "D",
            "tailscaleHostname": "h",
            "tailscaleIP": "100.64.0.1",
            "status": "online", "capabilities": []
        }"#;
        let device: BaseDevice = serde_json::from_str(json).unwrap();
        assert_eq!(device.tailscale_ip.as_deref(), Some("100.64.0.1"),
            "BaseDevice must accept 'tailscaleIP' (uppercase IP) from TS/Go");
    }

    /// Verify the Rust-native camelCase form also works.
    #[test]
    fn base_device_deserializes_tailscale_ip_lowercase() {
        let json = r#"{
            "id": "dev-1", "type": "desktop", "name": "D",
            "tailscaleHostname": "h",
            "tailscaleIp": "100.64.0.2",
            "status": "online", "capabilities": []
        }"#;
        let device: BaseDevice = serde_json::from_str(json).unwrap();
        assert_eq!(device.tailscale_ip.as_deref(), Some("100.64.0.2"),
            "BaseDevice must also accept 'tailscaleIp' (Rust camelCase)");
    }

    /// BUG-2: TS sends "tailscaleDNSName" but camelCase produces "tailscaleDnsName".
    #[test]
    fn base_device_deserializes_tailscale_dns_name_uppercase() {
        let json = r#"{
            "id": "dev-1", "type": "desktop", "name": "D",
            "tailscaleHostname": "h",
            "tailscaleDNSName": "app-desktop-dev-1.tailnet.ts.net",
            "status": "online", "capabilities": []
        }"#;
        let device: BaseDevice = serde_json::from_str(json).unwrap();
        assert_eq!(
            device.tailscale_dns_name.as_deref(),
            Some("app-desktop-dev-1.tailnet.ts.net"),
            "BaseDevice must accept 'tailscaleDNSName' from TS"
        );
    }

    /// BUG-2: TailnetPeer uses "tailscaleIPs" (uppercase IPs) from TS/Go.
    #[test]
    fn tailnet_peer_deserializes_tailscale_ips_uppercase() {
        let json = r#"{
            "id": "peer-1", "hostname": "h",
            "dnsName": "h.ts.net",
            "tailscaleIPs": ["100.64.0.2", "fd7a::1"],
            "online": true
        }"#;
        let peer: TailnetPeer = serde_json::from_str(json).unwrap();
        assert_eq!(peer.tailscale_ips, vec!["100.64.0.2", "fd7a::1"],
            "TailnetPeer must accept 'tailscaleIPs' (uppercase IPs)");
    }

    /// TailnetPeer must also accept lowercase "tailscaleIps".
    #[test]
    fn tailnet_peer_deserializes_tailscale_ips_lowercase() {
        let json = r#"{
            "id": "peer-1", "hostname": "h",
            "dnsName": "h.ts.net",
            "tailscaleIps": ["100.64.0.3"],
            "online": false
        }"#;
        let peer: TailnetPeer = serde_json::from_str(json).unwrap();
        assert_eq!(peer.tailscale_ips, vec!["100.64.0.3"],
            "TailnetPeer must also accept 'tailscaleIps' (alias)");
    }

    /// Full roundtrip: serialize then deserialize preserves all fields.
    #[test]
    fn base_device_full_roundtrip_preserves_all_fields() {
        let device = BaseDevice {
            id: "roundtrip-1".to_string(),
            device_type: "server".to_string(),
            name: "Roundtrip Server".to_string(),
            tailscale_hostname: "app-server-roundtrip-1".to_string(),
            tailscale_dns_name: Some("app-server-roundtrip-1.tailnet.ts.net".to_string()),
            tailscale_ip: Some("100.64.0.42".to_string()),
            status: DeviceStatus::Online,
            capabilities: vec!["file-transfer".to_string(), "clipboard".to_string()],
            metadata: Some({
                let mut m = HashMap::new();
                m.insert("version".to_string(), serde_json::json!("1.0.0"));
                m
            }),
            last_seen: Some(1710000000000),
            started_at: Some(1709999000000),
            os: Some("linux".to_string()),
            latency_ms: Some(12.5),
        };

        let json = serde_json::to_string(&device).unwrap();
        let parsed: BaseDevice = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, device, "Full roundtrip must preserve all fields");
    }

    /// TailnetPeer roundtrip test.
    #[test]
    fn tailnet_peer_full_roundtrip() {
        let peer = TailnetPeer {
            id: "node-abc".to_string(),
            hostname: "app-desktop-peer-1".to_string(),
            dns_name: "app-desktop-peer-1.tailnet.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.5".to_string()],
            online: true,
            os: Some("darwin".to_string()),
            cur_addr: None,
            relay: None,
            last_seen: None,
            key_expiry: None,
            expired: false,
        };

        let json = serde_json::to_string(&peer).unwrap();
        let parsed: TailnetPeer = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, peer, "TailnetPeer roundtrip must preserve all fields");
    }

    /// TailnetPeer serializes with "tailscaleIPs" (uppercase) for Go interop.
    #[test]
    fn tailnet_peer_serializes_tailscale_ips_uppercase() {
        let peer = TailnetPeer {
            id: "p".to_string(),
            hostname: "h".to_string(),
            dns_name: "d".to_string(),
            tailscale_ips: vec!["100.64.0.1".to_string()],
            online: true,
            os: None,
            cur_addr: None,
            relay: None,
            last_seen: None,
            key_expiry: None,
            expired: false,
        };

        let json = serde_json::to_string(&peer).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("tailscaleIPs").is_some(),
            "TailnetPeer must serialize 'tailscale_ips' as 'tailscaleIPs'");
    }

    /// StatusEventData from bridge/protocol.rs handles tailscaleIP alias.
    #[test]
    fn status_event_data_tailscale_ip_alias() {
        use crate::bridge::protocol::StatusEventData;

        let json = r#"{"state":"running","hostname":"n","dnsName":"n.ts.net","tailscaleIP":"100.64.0.1"}"#;
        let data: StatusEventData = serde_json::from_str(json).unwrap();
        assert_eq!(data.tailscale_ip, "100.64.0.1",
            "StatusEventData must accept 'tailscaleIP' alias");
    }

    /// bridge/protocol.rs TailnetPeer uses rename for tailscaleIPs.
    #[test]
    fn bridge_protocol_tailnet_peer_tailscale_ips() {
        use crate::bridge::protocol::PeersEventData;

        let json = r#"{"peers":[{"id":"1","hostname":"h","dnsName":"d","tailscaleIPs":["100.64.0.2"],"online":true}]}"#;
        let data: PeersEventData = serde_json::from_str(json).unwrap();
        assert_eq!(data.peers[0].tailscale_ips, vec!["100.64.0.2"]);
    }

    // ── Phase 2: Rich peer info + PeerIdentity ──────────────────────────

    #[test]
    fn tailnet_peer_rich_fields_roundtrip() {
        let peer = TailnetPeer {
            id: "node-rich".to_string(),
            hostname: "rich-peer".to_string(),
            dns_name: "rich-peer.tail.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.10".to_string()],
            online: true,
            os: Some("linux".to_string()),
            cur_addr: Some("192.168.1.5:41641".to_string()),
            relay: Some("sea".to_string()),
            last_seen: Some("2026-03-18T12:00:00Z".to_string()),
            key_expiry: Some("2026-04-18T12:00:00Z".to_string()),
            expired: false,
        };

        let json = serde_json::to_string(&peer).unwrap();
        let parsed: TailnetPeer = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, peer);
        assert_eq!(parsed.cur_addr, Some("192.168.1.5:41641".to_string()));
        assert_eq!(parsed.relay, Some("sea".to_string()));
        assert_eq!(parsed.last_seen, Some("2026-03-18T12:00:00Z".to_string()));
        assert_eq!(parsed.key_expiry, Some("2026-04-18T12:00:00Z".to_string()));
    }

    #[test]
    fn tailnet_peer_rich_fields_omitted_when_none() {
        let peer = TailnetPeer {
            id: "p".to_string(),
            hostname: "h".to_string(),
            dns_name: "d".to_string(),
            tailscale_ips: vec![],
            online: false,
            os: None,
            cur_addr: None,
            relay: None,
            last_seen: None,
            key_expiry: None,
            expired: false,
        };

        let json = serde_json::to_string(&peer).unwrap();
        assert!(!json.contains("curAddr"));
        assert!(!json.contains("relay"));
        assert!(!json.contains("lastSeen"));
        assert!(!json.contains("keyExpiry"));
    }

    #[test]
    fn peer_identity_from_remote_dns_valid_json() {
        let json = r#"{"nodeId":"stable-abc","dnsName":"peer.ts.net","loginName":"user@example.com","displayName":"User"}"#;
        let identity = PeerIdentity::from_remote_dns(json).unwrap();
        assert_eq!(identity.node_id, "stable-abc");
        assert_eq!(identity.dns_name, "peer.ts.net");
        assert_eq!(identity.display_name, Some("User".to_string()));
    }

    #[test]
    fn peer_identity_from_remote_dns_plain_string() {
        // Non-JSON string (e.g., a plain DNS name) should return None
        let result = PeerIdentity::from_remote_dns("peer.ts.net");
        assert!(result.is_none());
    }

    #[test]
    fn peer_identity_from_remote_dns_empty() {
        let result = PeerIdentity::from_remote_dns("");
        assert!(result.is_none());
    }

    // ── Phase 2: PeerIdentity comprehensive tests ────────────────────────

    #[test]
    fn peer_identity_parse_from_valid_json() {
        let json = r#"{"nodeId":"stable-xyz","dnsName":"mynode.tail.ts.net","isTagged":false,"userId":"user-123","displayName":"James"}"#;
        let identity: PeerIdentity = serde_json::from_str(json).unwrap();
        assert_eq!(identity.node_id, "stable-xyz");
        assert_eq!(identity.dns_name, "mynode.tail.ts.net");
        assert!(!identity.is_tagged);
        assert_eq!(identity.user_id, Some("user-123".to_string()));
        assert_eq!(identity.display_name, Some("James".to_string()));
    }

    #[test]
    fn peer_identity_from_remote_dns_plain_returns_none() {
        // A plain DNS name (not JSON) should return None
        assert!(PeerIdentity::from_remote_dns("plain.dns.name").is_none());
    }

    #[test]
    fn peer_identity_from_remote_dns_json_returns_some() {
        let json = r#"{"nodeId":"abc","dnsName":"x.ts.net","isTagged":false}"#;
        let result = PeerIdentity::from_remote_dns(json);
        assert!(result.is_some());
        let identity = result.unwrap();
        assert_eq!(identity.node_id, "abc");
        assert_eq!(identity.dns_name, "x.ts.net");
        assert!(!identity.is_tagged);
    }

    #[test]
    fn peer_identity_roundtrip_serialize_deserialize() {
        let identity = PeerIdentity {
            node_id: "node-123".to_string(),
            dns_name: "node-123.tail.ts.net".to_string(),
            is_tagged: false,
            user_id: Some("user-456".to_string()),
            display_name: Some("Test User".to_string()),
            login_name: Some("test@example.com".to_string()),
            profile_pic_url: Some("https://example.com/pic.png".to_string()),
        };

        let json = serde_json::to_string(&identity).unwrap();
        let parsed: PeerIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, identity, "PeerIdentity roundtrip must preserve all fields");
    }

    #[test]
    fn peer_identity_missing_optional_fields_deserializes_with_defaults() {
        // Only required/defaulted fields present, optional ones absent
        let json = r#"{"nodeId":"n1","dnsName":"n1.ts.net"}"#;
        let identity: PeerIdentity = serde_json::from_str(json).unwrap();
        assert_eq!(identity.node_id, "n1");
        assert_eq!(identity.dns_name, "n1.ts.net");
        assert!(!identity.is_tagged, "isTagged should default to false");
        assert_eq!(identity.user_id, None, "userId should default to None");
        assert_eq!(identity.display_name, None, "displayName should default to None");
        assert_eq!(identity.login_name, None, "loginName should default to None");
        assert_eq!(identity.profile_pic_url, None, "profilePicUrl should default to None");
    }

    #[test]
    fn peer_identity_tagged_node() {
        let json = r#"{"nodeId":"tagged-node","dnsName":"tagged.ts.net","isTagged":true}"#;
        let identity: PeerIdentity = serde_json::from_str(json).unwrap();
        assert!(identity.is_tagged, "isTagged should be true for tagged nodes");
        assert_eq!(identity.user_id, None, "Tagged nodes should not have userId");
        assert_eq!(identity.node_id, "tagged-node");
    }

    #[test]
    fn peer_identity_optional_fields_omitted_in_json_when_none() {
        let identity = PeerIdentity {
            node_id: "n".to_string(),
            dns_name: "n.ts.net".to_string(),
            is_tagged: false,
            user_id: None,
            display_name: None,
            login_name: None,
            profile_pic_url: None,
        };
        let json = serde_json::to_string(&identity).unwrap();
        assert!(!json.contains("userId"), "None userId should be omitted");
        assert!(!json.contains("displayName"), "None displayName should be omitted");
        assert!(!json.contains("loginName"), "None loginName should be omitted");
        assert!(!json.contains("profilePicUrl"), "None profilePicUrl should be omitted");
    }
}
