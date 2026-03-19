use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// FrameType
// ---------------------------------------------------------------------------

/// Frame type discriminant -- first byte of every binary WS message (v3 protocol).
///
/// Introduced by RFC 009 to structurally discriminate control frames (heartbeats,
/// handshakes) from data frames (MeshEnvelope) and error frames.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameType {
    /// Protocol-level control messages (ping, pong, handshake).
    Control = 0x01,
    /// Data messages (MeshEnvelope).
    Data = 0x02,
    /// Protocol-level error notifications.
    Error = 0x03,
}

impl TryFrom<u8> for FrameType {
    type Error = WireError;

    fn try_from(b: u8) -> Result<Self, WireError> {
        match b {
            0x01 => Ok(Self::Control),
            0x02 => Ok(Self::Data),
            0x03 => Ok(Self::Error),
            other => Err(WireError::UnknownFrameType(other)),
        }
    }
}

impl fmt::Display for FrameType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Control => write!(f, "Control(0x01)"),
            Self::Data => write!(f, "Data(0x02)"),
            Self::Error => write!(f, "Error(0x03)"),
        }
    }
}

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

/// Flags byte -- second byte of every binary WS message (v3 protocol).
///
/// Bit 0: encoding (0 = MessagePack, 1 = JSON).
/// Bits 1-7: reserved (must be zero).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags(u8);

impl Flags {
    /// Bit 0 controls encoding format.
    const ENCODING_MASK: u8 = 0x01;
    /// Bits 1-7 are reserved and must be zero.
    const RESERVED_MASK: u8 = 0xFE;

    /// Create a new `Flags` value with the given encoding preference.
    pub fn new(use_json: bool) -> Self {
        Self(if use_json { 1 } else { 0 })
    }

    /// Parse a flags byte, rejecting any frame with reserved bits set.
    pub fn from_byte(b: u8) -> Result<Self, WireError> {
        if b & Self::RESERVED_MASK != 0 {
            return Err(WireError::ReservedBitsSet(b));
        }
        Ok(Self(b))
    }

    /// Returns `true` if the payload is JSON-encoded, `false` for MessagePack.
    pub fn is_json(&self) -> bool {
        self.0 & Self::ENCODING_MASK != 0
    }

    /// Returns the raw byte value.
    pub fn as_byte(&self) -> u8 {
        self.0
    }
}

impl fmt::Display for Flags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Flags(0x{:02X}, encoding={})",
            self.0,
            if self.is_json() { "json" } else { "msgpack" }
        )
    }
}

// ---------------------------------------------------------------------------
// WireError
// ---------------------------------------------------------------------------

/// Errors that can occur when parsing wire-level frame headers.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WireError {
    /// Byte 0 is not a recognized frame type.
    #[error("unknown frame type: 0x{0:02X}")]
    UnknownFrameType(u8),

    /// Reserved bits are set in the flags byte.
    #[error("reserved bits set in flags byte: 0x{0:02X}")]
    ReservedBitsSet(u8),
}

// ---------------------------------------------------------------------------
// ControlMessage
// ---------------------------------------------------------------------------

/// Control frame payload (frame type `0x01`).
///
/// Control messages are consumed by the transport layer and are **not** routed
/// through the message bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ControlMessage {
    /// Heartbeat ping.
    Ping {
        /// Sender timestamp (ms since epoch).
        timestamp: u64,
    },
    /// Heartbeat pong (response to ping).
    Pong {
        /// Responder timestamp (ms since epoch).
        timestamp: u64,
        /// The timestamp from the ping being acknowledged.
        echo_timestamp: u64,
    },
    /// Protocol version handshake (sent on connection open).
    Handshake {
        /// The sender's maximum supported protocol version.
        protocol_version: u32,
        /// The sender's device ID.
        device_id: String,
        /// Capabilities the sender supports.
        capabilities: Vec<String>,
    },
    /// Handshake acknowledgment.
    HandshakeAck {
        /// The responder's maximum supported protocol version.
        protocol_version: u32,
        /// The responder's device ID.
        device_id: String,
        /// Negotiated protocol version (min of both sides).
        negotiated_version: u32,
    },
}

// ---------------------------------------------------------------------------
// ErrorCode + ProtocolError
// ---------------------------------------------------------------------------

/// Machine-readable error codes for protocol-level error frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// Unrecognized frame type byte.
    UnknownFrameType,
    /// Reserved bits set in flags byte.
    InvalidFlags,
    /// Payload failed to deserialize.
    MalformedPayload,
    /// Unknown namespace in envelope.
    UnknownNamespace,
    /// Unknown message type in envelope.
    UnknownMessageType,
    /// Protocol version mismatch.
    VersionMismatch,
    /// Message too large.
    MessageTooLarge,
    /// Rate limit exceeded.
    RateLimited,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Serialize to the kebab-case string for display.
        let s = match self {
            Self::UnknownFrameType => "unknown-frame-type",
            Self::InvalidFlags => "invalid-flags",
            Self::MalformedPayload => "malformed-payload",
            Self::UnknownNamespace => "unknown-namespace",
            Self::UnknownMessageType => "unknown-message-type",
            Self::VersionMismatch => "version-mismatch",
            Self::MessageTooLarge => "message-too-large",
            Self::RateLimited => "rate-limited",
        };
        write!(f, "{}", s)
    }
}

/// Protocol-level error notification (frame type `0x03`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolError {
    /// Machine-readable error code.
    pub code: ErrorCode,
    /// Human-readable description.
    pub message: String,
    /// Whether the sender will close the connection after this error.
    pub fatal: bool,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ProtocolError({}, fatal={}, {})",
            self.code, self.fatal, self.message
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- FrameType tests --

    #[test]
    fn frame_type_from_valid_bytes() {
        assert_eq!(FrameType::try_from(0x01).unwrap(), FrameType::Control);
        assert_eq!(FrameType::try_from(0x02).unwrap(), FrameType::Data);
        assert_eq!(FrameType::try_from(0x03).unwrap(), FrameType::Error);
    }

    #[test]
    fn frame_type_from_zero() {
        let err = FrameType::try_from(0x00).unwrap_err();
        assert!(matches!(err, WireError::UnknownFrameType(0x00)));
    }

    #[test]
    fn frame_type_from_out_of_range() {
        for b in [0x04, 0x10, 0x80, 0xFF] {
            let err = FrameType::try_from(b).unwrap_err();
            assert!(matches!(err, WireError::UnknownFrameType(v) if v == b));
        }
    }

    #[test]
    fn frame_type_repr_values() {
        assert_eq!(FrameType::Control as u8, 0x01);
        assert_eq!(FrameType::Data as u8, 0x02);
        assert_eq!(FrameType::Error as u8, 0x03);
    }

    #[test]
    fn frame_type_display() {
        assert_eq!(format!("{}", FrameType::Control), "Control(0x01)");
        assert_eq!(format!("{}", FrameType::Data), "Data(0x02)");
        assert_eq!(format!("{}", FrameType::Error), "Error(0x03)");
    }

    #[test]
    fn frame_type_roundtrip_all_variants() {
        for b in [0x01u8, 0x02, 0x03] {
            let ft = FrameType::try_from(b).unwrap();
            assert_eq!(ft as u8, b);
        }
    }

    // -- Flags tests --

    #[test]
    fn flags_new_msgpack() {
        let f = Flags::new(false);
        assert!(!f.is_json());
        assert_eq!(f.as_byte(), 0x00);
    }

    #[test]
    fn flags_new_json() {
        let f = Flags::new(true);
        assert!(f.is_json());
        assert_eq!(f.as_byte(), 0x01);
    }

    #[test]
    fn flags_from_byte_valid() {
        let f0 = Flags::from_byte(0x00).unwrap();
        assert!(!f0.is_json());

        let f1 = Flags::from_byte(0x01).unwrap();
        assert!(f1.is_json());
    }

    #[test]
    fn flags_from_byte_reserved_bits_rejected() {
        // Every byte with any of bits 1-7 set should be rejected.
        for b in [0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0xFE, 0xFF, 0x03] {
            let err = Flags::from_byte(b).unwrap_err();
            assert!(matches!(err, WireError::ReservedBitsSet(v) if v == b));
        }
    }

    #[test]
    fn flags_display() {
        let f = Flags::new(true);
        let display = format!("{}", f);
        assert!(display.contains("json"));

        let f = Flags::new(false);
        let display = format!("{}", f);
        assert!(display.contains("msgpack"));
    }

    // -- ControlMessage tests --

    #[test]
    fn control_message_ping_serde_roundtrip() {
        let msg = ControlMessage::Ping {
            timestamp: 1710764400000,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ping""#));
        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn control_message_pong_serde_roundtrip() {
        let msg = ControlMessage::Pong {
            timestamp: 1710764400001,
            echo_timestamp: 1710764400000,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"pong""#));
        assert!(json.contains("echoTimestamp") || json.contains("echo_timestamp"));
        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn control_message_handshake_serde_roundtrip() {
        let msg = ControlMessage::Handshake {
            protocol_version: 3,
            device_id: "device-abc".to_string(),
            capabilities: vec!["mesh".to_string(), "sync".to_string()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"handshake""#));
        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn control_message_handshake_ack_serde_roundtrip() {
        let msg = ControlMessage::HandshakeAck {
            protocol_version: 3,
            device_id: "device-xyz".to_string(),
            negotiated_version: 3,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"handshake-ack""#));
        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn control_message_handshake_empty_capabilities() {
        let msg = ControlMessage::Handshake {
            protocol_version: 2,
            device_id: "d".to_string(),
            capabilities: vec![],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn control_message_pong_field_names_kebab_case_on_wire() {
        let msg = ControlMessage::Pong {
            timestamp: 100,
            echo_timestamp: 99,
        };
        let json = serde_json::to_string(&msg).unwrap();
        // serde with rename_all = kebab-case uses kebab for the tag,
        // but field names follow the struct (snake_case by default in the
        // Serialize derive). Let's verify the tag is right.
        assert!(json.contains(r#""type":"pong""#));
    }

    #[test]
    fn control_message_msgpack_roundtrip() {
        let msg = ControlMessage::Ping {
            timestamp: 42,
        };
        let packed = rmp_serde::to_vec_named(&msg).unwrap();
        let parsed: ControlMessage = rmp_serde::from_slice(&packed).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn control_message_handshake_msgpack_roundtrip() {
        let msg = ControlMessage::Handshake {
            protocol_version: 3,
            device_id: "dev-1".to_string(),
            capabilities: vec!["mesh".to_string()],
        };
        let packed = rmp_serde::to_vec_named(&msg).unwrap();
        let parsed: ControlMessage = rmp_serde::from_slice(&packed).unwrap();
        assert_eq!(parsed, msg);
    }

    // -- ErrorCode tests --

    #[test]
    fn error_code_serde_roundtrip_all_variants() {
        let variants = [
            ErrorCode::UnknownFrameType,
            ErrorCode::InvalidFlags,
            ErrorCode::MalformedPayload,
            ErrorCode::UnknownNamespace,
            ErrorCode::UnknownMessageType,
            ErrorCode::VersionMismatch,
            ErrorCode::MessageTooLarge,
            ErrorCode::RateLimited,
        ];
        for code in variants {
            let json = serde_json::to_string(&code).unwrap();
            let parsed: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, code, "Roundtrip failed for {code:?}");
        }
    }

    #[test]
    fn error_code_wire_strings() {
        assert_eq!(
            serde_json::to_string(&ErrorCode::UnknownFrameType).unwrap(),
            r#""unknown-frame-type""#
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::InvalidFlags).unwrap(),
            r#""invalid-flags""#
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::MalformedPayload).unwrap(),
            r#""malformed-payload""#
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::UnknownNamespace).unwrap(),
            r#""unknown-namespace""#
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::UnknownMessageType).unwrap(),
            r#""unknown-message-type""#
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::VersionMismatch).unwrap(),
            r#""version-mismatch""#
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::MessageTooLarge).unwrap(),
            r#""message-too-large""#
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::RateLimited).unwrap(),
            r#""rate-limited""#
        );
    }

    #[test]
    fn error_code_display() {
        assert_eq!(format!("{}", ErrorCode::MalformedPayload), "malformed-payload");
        assert_eq!(format!("{}", ErrorCode::RateLimited), "rate-limited");
    }

    // -- ProtocolError tests --

    #[test]
    fn protocol_error_serde_roundtrip() {
        let err = ProtocolError {
            code: ErrorCode::MalformedPayload,
            message: "Failed to deserialize MeshEnvelope".to_string(),
            fatal: false,
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains(r#""code":"malformed-payload""#));
        assert!(json.contains(r#""fatal":false"#));
        let parsed: ProtocolError = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, err);
    }

    #[test]
    fn protocol_error_fatal() {
        let err = ProtocolError {
            code: ErrorCode::VersionMismatch,
            message: "Incompatible protocol version".to_string(),
            fatal: true,
        };
        let json = serde_json::to_string(&err).unwrap();
        let parsed: ProtocolError = serde_json::from_str(&json).unwrap();
        assert!(parsed.fatal);
        assert_eq!(parsed.code, ErrorCode::VersionMismatch);
    }

    #[test]
    fn protocol_error_msgpack_roundtrip() {
        let err = ProtocolError {
            code: ErrorCode::MessageTooLarge,
            message: "Exceeded 16MB limit".to_string(),
            fatal: true,
        };
        let packed = rmp_serde::to_vec_named(&err).unwrap();
        let parsed: ProtocolError = rmp_serde::from_slice(&packed).unwrap();
        assert_eq!(parsed, err);
    }

    #[test]
    fn protocol_error_display() {
        let err = ProtocolError {
            code: ErrorCode::RateLimited,
            message: "Too many requests".to_string(),
            fatal: false,
        };
        let display = format!("{}", err);
        assert!(display.contains("rate-limited"));
        assert!(display.contains("Too many requests"));
        assert!(display.contains("fatal=false"));
    }

    #[test]
    fn protocol_error_all_codes_roundtrip() {
        let codes = [
            ErrorCode::UnknownFrameType,
            ErrorCode::InvalidFlags,
            ErrorCode::MalformedPayload,
            ErrorCode::UnknownNamespace,
            ErrorCode::UnknownMessageType,
            ErrorCode::VersionMismatch,
            ErrorCode::MessageTooLarge,
            ErrorCode::RateLimited,
        ];
        for code in codes {
            let err = ProtocolError {
                code,
                message: format!("test {code}"),
                fatal: false,
            };
            let json = serde_json::to_string(&err).unwrap();
            let parsed: ProtocolError = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.code, code);
        }
    }

    // -- WireError tests --

    #[test]
    fn wire_error_display() {
        let e1 = WireError::UnknownFrameType(0xFF);
        assert!(format!("{}", e1).contains("0xFF"));

        let e2 = WireError::ReservedBitsSet(0x82);
        assert!(format!("{}", e2).contains("0x82"));
    }
}
