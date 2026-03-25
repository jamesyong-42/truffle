//! Layer 6: Envelope — Namespace-routed message framing.
//!
//! All application messages exchanged over WebSocket connections are wrapped
//! in an [`Envelope`]. The envelope carries a namespace string that routes
//! messages to the correct application subscriber, and an opaque JSON payload
//! that truffle-core **never** inspects.
//!
//! # Layer rules
//!
//! - Layer 6 defines the envelope format, serialization, and deserialization
//! - Layer 6 does NOT route messages — that is the Node/Session layer's job
//! - Layer 6 does NOT know about WebSocket, TCP, or any transport
//! - The `namespace` field is an opaque string — Layer 6 never matches known values
//! - No `"mesh"`, `"device-announce"`, or `"file-transfer"` constants here

pub mod codec;

use serde::{Deserialize, Serialize};

// Re-export the codec types for convenience.
pub use codec::{EnvelopeCodec, JsonCodec};

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// A namespace-routed message. All application messages use this format
/// over WebSocket connections. truffle-core NEVER inspects the payload.
///
/// # Fields
///
/// - `namespace`: Application-owned routing key (e.g., `"ft"`, `"chat"`).
/// - `msg_type`: Application-defined message type within the namespace.
/// - `payload`: Opaque JSON value. truffle-core never deserializes this.
/// - `timestamp`: Optional millisecond Unix timestamp, set by the sender.
/// - `from`: Sender peer ID, typically set by the receiving side (Layer 5).
///
/// # Forward compatibility
///
/// Unknown fields in the JSON representation are silently ignored during
/// deserialization, allowing older nodes to receive messages from newer
/// protocol versions without error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Namespace owned by the application (e.g., `"ft"`, `"chat"`, `"bus"`).
    /// Used for routing to the correct subscriber.
    pub namespace: String,

    /// Application-defined message type within the namespace.
    pub msg_type: String,

    /// Opaque payload. truffle-core never deserializes this further.
    pub payload: serde_json::Value,

    /// Millisecond Unix timestamp, set by the sender.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,

    /// Sender peer ID, typically populated on the receiving side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl Envelope {
    /// Create a new envelope with the given namespace, message type, and payload.
    ///
    /// The `timestamp` and `from` fields are left as `None`. Use
    /// [`with_timestamp`](Self::with_timestamp) to stamp the current time.
    pub fn new(namespace: &str, msg_type: &str, payload: serde_json::Value) -> Self {
        Self {
            namespace: namespace.to_string(),
            msg_type: msg_type.to_string(),
            payload,
            timestamp: None,
            from: None,
        }
    }

    /// Set the timestamp to the current Unix time in milliseconds.
    pub fn with_timestamp(mut self) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.timestamp = Some(now);
        self
    }

    /// Serialize this envelope to JSON bytes using the default codec.
    pub fn serialize(&self) -> Result<Vec<u8>, EnvelopeError> {
        serde_json::to_vec(self).map_err(EnvelopeError::Serialization)
    }

    /// Deserialize an envelope from JSON bytes using the default codec.
    pub fn deserialize(data: &[u8]) -> Result<Self, EnvelopeError> {
        serde_json::from_slice(data).map_err(EnvelopeError::Deserialization)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from Layer 6 envelope operations.
#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    /// Failed to serialize an envelope.
    #[error("envelope serialization error: {0}")]
    Serialization(serde_json::Error),

    /// Failed to deserialize an envelope.
    #[error("envelope deserialization error: {0}")]
    Deserialization(serde_json::Error),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_envelope_serialize_deserialize_roundtrip() {
        let envelope = Envelope::new("ft", "offer", json!({"file": "test.txt", "size": 1024}))
            .with_timestamp();

        let bytes = envelope.serialize().expect("serialize");
        let decoded = Envelope::deserialize(&bytes).expect("deserialize");

        assert_eq!(decoded.namespace, "ft");
        assert_eq!(decoded.msg_type, "offer");
        assert_eq!(decoded.payload["file"], "test.txt");
        assert_eq!(decoded.payload["size"], 1024);
        assert!(decoded.timestamp.is_some());
        assert_eq!(decoded.timestamp, envelope.timestamp);
    }

    #[test]
    fn test_envelope_timestamp_generation() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let envelope = Envelope::new("test", "ping", json!(null)).with_timestamp();

        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let ts = envelope.timestamp.expect("timestamp should be set");
        assert!(ts >= before, "timestamp {ts} should be >= {before}");
        assert!(ts <= after, "timestamp {ts} should be <= {after}");
    }

    #[test]
    fn test_envelope_unknown_fields_ignored() {
        // Simulate a future protocol version that adds an extra field.
        let json_str = r#"{
            "namespace": "chat",
            "msg_type": "message",
            "payload": {"text": "hello"},
            "timestamp": 1234567890,
            "from": "peer-1",
            "future_field": "should be ignored",
            "another_new_field": 42
        }"#;

        let envelope: Envelope =
            serde_json::from_str(json_str).expect("should ignore unknown fields");
        assert_eq!(envelope.namespace, "chat");
        assert_eq!(envelope.msg_type, "message");
        assert_eq!(envelope.payload["text"], "hello");
        assert_eq!(envelope.timestamp, Some(1234567890));
        assert_eq!(envelope.from.as_deref(), Some("peer-1"));
    }

    #[test]
    fn test_envelope_empty_payload() {
        let envelope = Envelope::new("bus", "heartbeat", json!(null));
        let bytes = envelope.serialize().expect("serialize");
        let decoded = Envelope::deserialize(&bytes).expect("deserialize");

        assert_eq!(decoded.namespace, "bus");
        assert_eq!(decoded.msg_type, "heartbeat");
        assert!(decoded.payload.is_null());
        assert!(decoded.timestamp.is_none());
        assert!(decoded.from.is_none());
    }

    #[test]
    fn test_envelope_large_payload() {
        // ~100 KB payload
        let large_data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        let payload = json!({"data": large_data});

        let envelope = Envelope::new("stream", "chunk", payload.clone()).with_timestamp();
        let bytes = envelope.serialize().expect("serialize");
        let decoded = Envelope::deserialize(&bytes).expect("deserialize");

        assert_eq!(decoded.namespace, "stream");
        assert_eq!(decoded.msg_type, "chunk");
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_envelope_optional_fields_not_serialized_when_none() {
        let envelope = Envelope::new("test", "msg", json!("data"));

        let json_str = serde_json::to_string(&envelope).expect("serialize");
        assert!(!json_str.contains("timestamp"), "None timestamp should be skipped");
        assert!(!json_str.contains("from"), "None from should be skipped");
    }

    #[test]
    fn test_envelope_deserialization_error() {
        let bad_json = b"not valid json";
        let result = Envelope::deserialize(bad_json);
        assert!(result.is_err());
    }
}
