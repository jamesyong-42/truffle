use serde::{Deserialize, Serialize};

/// Envelope format for messages sent over the mesh network.
/// Used by MessageBus for namespace-based pub/sub.
///
/// Architecture:
///   Layer 2: Application (MessageBus) - uses MeshEnvelope
///       |
///   Layer 1: Routing & Serialization (MeshNode)
///       |
///   Layer 0: Transport (WebSocketTransport)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshEnvelope {
    /// Message namespace (e.g., "mesh", "sync", "file-transfer").
    pub namespace: String,
    /// Message type within namespace.
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Message payload.
    pub payload: serde_json::Value,
    /// Timestamp (optional, added by transport).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
}

impl MeshEnvelope {
    pub fn new(namespace: impl Into<String>, msg_type: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            namespace: namespace.into(),
            msg_type: msg_type.into(),
            payload,
            timestamp: None,
        }
    }

    /// Create an envelope with the current timestamp.
    pub fn with_timestamp(mut self) -> Self {
        self.timestamp = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        );
        self
    }
}

/// The "mesh" namespace constant used for internal mesh protocol messages.
pub const MESH_NAMESPACE: &str = "mesh";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_serde_roundtrip() {
        let envelope = MeshEnvelope::new("sync", "update", serde_json::json!({"key": "val"}));
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: MeshEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.namespace, "sync");
        assert_eq!(parsed.msg_type, "update");
        assert!(parsed.timestamp.is_none());
    }

    #[test]
    fn envelope_with_timestamp() {
        let envelope = MeshEnvelope::new("mesh", "message", serde_json::json!(null))
            .with_timestamp();
        assert!(envelope.timestamp.is_some());
    }

    #[test]
    fn envelope_timestamp_omitted_when_none() {
        let envelope = MeshEnvelope::new("mesh", "test", serde_json::json!(null));
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(!json.contains("timestamp"));
    }
}
