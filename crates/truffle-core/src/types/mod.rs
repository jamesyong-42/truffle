use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Device role in STAR topology.
/// - Primary: Hub device that all others connect to
/// - Secondary: Connects to primary, routes through primary
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceRole {
    Primary,
    Secondary,
}

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
    /// Device role in STAR topology.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<DeviceRole>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_role_serde_roundtrip() {
        let json = serde_json::to_string(&DeviceRole::Primary).unwrap();
        assert_eq!(json, "\"primary\"");
        let parsed: DeviceRole = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, DeviceRole::Primary);
    }

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
            role: Some(DeviceRole::Primary),
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
            role: None,
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
        assert!(!json.contains("role"));
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
            role: Some(DeviceRole::Secondary),
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
}
