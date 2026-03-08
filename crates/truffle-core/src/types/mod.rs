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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tailscale_dns_name: Option<String>,
    /// Tailscale IP address (assigned when connected).
    #[serde(skip_serializing_if = "Option::is_none")]
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
    pub dns_name: String,
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
}
