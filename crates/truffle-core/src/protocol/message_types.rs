use serde::{Deserialize, Serialize};

use crate::types::BaseDevice;

/// Base mesh message wrapper.
///
/// Future work: Flatten `from`, `to`, `correlation_id` fields into `MeshEnvelope`
/// and remove this struct. The addressing fields are duplicated between MeshMessage
/// and MeshEnvelope. Deferred because MeshMessage is deeply embedded in test helpers,
/// node.rs event loops, and election broadcasting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshMessage {
    /// Message type (e.g., "device-announce", "election-start").
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Source device ID.
    pub from: String,
    /// Target device ID (for routed messages, omit for broadcast).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Message payload.
    pub payload: serde_json::Value,
    /// Timestamp (ms since epoch).
    pub timestamp: u64,
    /// Correlation ID for request/response matching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl MeshMessage {
    /// Create a new mesh message with the current timestamp.
    pub fn new(msg_type: impl Into<String>, from: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            msg_type: msg_type.into(),
            from: from.into(),
            to: None,
            payload,
            timestamp: current_timestamp_ms(),
            correlation_id: None,
        }
    }
}

/// Device announce payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAnnouncePayload {
    pub device: BaseDevice,
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
}

fn default_protocol_version() -> u32 {
    2
}

/// Device list payload (response from primary).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceListPayload {
    pub devices: Vec<BaseDevice>,
    pub primary_id: String,
}

/// Device goodbye payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceGoodbyePayload {
    pub device_id: String,
    pub reason: String,
}

/// Route message payload (wraps another envelope for routing via primary).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteMessagePayload {
    pub target_device_id: String,
    pub envelope: serde_json::Value,
}

/// Route broadcast payload (wraps an envelope for broadcast via primary).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteBroadcastPayload {
    pub envelope: serde_json::Value,
}

fn current_timestamp_ms() -> u64 {
    crate::util::current_timestamp_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_message_serde_roundtrip() {
        let msg = MeshMessage::new(
            "device-announce",
            "device-123",
            serde_json::json!({"key": "value"}),
        );
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: MeshMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.msg_type, "device-announce");
        assert_eq!(parsed.from, "device-123");
    }

    #[test]
    fn device_announce_payload_default_version() {
        let json = r#"{"device":{"id":"a","type":"desktop","name":"D","tailscaleHostname":"h","status":"online","capabilities":[]}}"#;
        let parsed: DeviceAnnouncePayload = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.protocol_version, 2);
    }
}
