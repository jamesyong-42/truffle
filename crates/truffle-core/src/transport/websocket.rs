use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{
    protocol::{CloseFrame, WebSocketConfig},
    Message,
};
use tokio_tungstenite::WebSocketStream;

use crate::protocol::frame::{ControlMessage, Flags, FrameType, ProtocolError, WireError};

/// Maximum WebSocket message size (16 MB, matching old FrameCodec MAX_MESSAGE_SIZE).
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Errors from WebSocket operations.
#[derive(Debug, thiserror::Error)]
pub enum WsError {
    #[error("WebSocket error: {0}")]
    Tungstenite(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("connection closed")]
    Closed,

    #[error("message too large: {0} bytes (max {MAX_MESSAGE_SIZE})")]
    MessageTooLarge(usize),

    #[error("empty binary frame (missing flags byte)")]
    EmptyBinaryFrame,

    #[error("MessagePack decode error: {0}")]
    MsgpackDecode(#[from] rmp_serde::decode::Error),

    #[error("MessagePack encode error: {0}")]
    MsgpackEncode(#[from] rmp_serde::encode::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("v3 wire error: {0}")]
    Wire(#[from] WireError),

    #[error("v3 frame too short: need at least 2 bytes, got {0}")]
    FrameTooShort(usize),
}

/// A decoded mesh message from the wire.
#[derive(Debug, Clone)]
pub struct WireMessage {
    /// The deserialized envelope.
    pub payload: serde_json::Value,
    /// Whether this was received as JSON (debug mode) or MessagePack.
    pub was_json: bool,
}

// ---------------------------------------------------------------------------
// v3 frame types (RFC 009 Phase 2)
// ---------------------------------------------------------------------------

/// A decoded v3 frame from the wire.
///
/// All frames use the v3 format: 2-byte header (frame type + flags) + payload.
#[derive(Debug, Clone)]
pub enum DecodedFrame {
    /// v3 control frame (heartbeat ping/pong, handshake, etc.).
    V3Control(ControlMessage),
    /// v3 data frame (MeshEnvelope payload).
    V3Data {
        payload: serde_json::Value,
        was_json: bool,
    },
    /// v3 error frame (protocol-level error from peer).
    V3Error(ProtocolError),
}

// ---------------------------------------------------------------------------
// v3 encode functions
// ---------------------------------------------------------------------------

/// Encode a v3 frame with the given frame type, flags, and raw payload bytes.
///
/// Produces: `[frame_type_byte, flags_byte, ...payload]`
pub fn encode_v3_frame(frame_type: FrameType, flags: Flags, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(2 + payload.len());
    buf.push(frame_type as u8);
    buf.push(flags.as_byte());
    buf.extend_from_slice(payload);
    buf
}

/// Encode a v3 data frame containing a `serde_json::Value` payload.
///
/// Produces: `[0x02, flags, msgpack_or_json(value)]`
pub fn encode_data_frame(value: &serde_json::Value, use_json: bool) -> Result<Vec<u8>, WsError> {
    let flags = Flags::new(use_json);
    let payload = encode_payload_bytes(value, use_json)?;
    Ok(encode_v3_frame(FrameType::Data, flags, &payload))
}

/// Encode a v3 control frame from a `ControlMessage`.
///
/// Produces: `[0x01, flags, msgpack_or_json(control_msg)]`
pub fn encode_control_frame(msg: &ControlMessage, use_json: bool) -> Result<Vec<u8>, WsError> {
    let flags = Flags::new(use_json);
    let payload = if use_json {
        serde_json::to_vec(msg)?
    } else {
        rmp_serde::to_vec_named(msg)?
    };
    Ok(encode_v3_frame(FrameType::Control, flags, &payload))
}

/// Encode a v3 error frame from a `ProtocolError`.
///
/// Produces: `[0x03, flags, msgpack_or_json(error)]`
pub fn encode_error_frame(error: &ProtocolError, use_json: bool) -> Result<Vec<u8>, WsError> {
    let flags = Flags::new(use_json);
    let payload = if use_json {
        serde_json::to_vec(error)?
    } else {
        rmp_serde::to_vec_named(error)?
    };
    Ok(encode_v3_frame(FrameType::Error, flags, &payload))
}

// ---------------------------------------------------------------------------
// v3 decode functions
// ---------------------------------------------------------------------------

/// Decode a binary WebSocket message (v3 format only).
///
/// All frames must use the v3 wire format: `[frame_type, flags, ...payload]`.
/// The first byte must be a valid `FrameType` (`0x01`, `0x02`, or `0x03`).
pub fn decode_frame(data: &[u8]) -> Result<DecodedFrame, WsError> {
    if data.is_empty() {
        return Err(WsError::EmptyBinaryFrame);
    }

    decode_v3_frame(data)
}

/// Decode a v3 frame (assumes byte 0 is a valid FrameType).
fn decode_v3_frame(data: &[u8]) -> Result<DecodedFrame, WsError> {
    if data.len() < 2 {
        return Err(WsError::FrameTooShort(data.len()));
    }

    let frame_type = FrameType::try_from(data[0])?;
    let flags = Flags::from_byte(data[1])?;
    let payload_bytes = &data[2..];

    match frame_type {
        FrameType::Control => {
            let msg: ControlMessage = if flags.is_json() {
                serde_json::from_slice(payload_bytes)?
            } else {
                rmp_serde::from_slice(payload_bytes)?
            };
            Ok(DecodedFrame::V3Control(msg))
        }
        FrameType::Data => {
            let value: serde_json::Value = if flags.is_json() {
                serde_json::from_slice(payload_bytes)?
            } else {
                rmp_serde::from_slice(payload_bytes)?
            };
            Ok(DecodedFrame::V3Data {
                payload: value,
                was_json: flags.is_json(),
            })
        }
        FrameType::Error => {
            let error: ProtocolError = if flags.is_json() {
                serde_json::from_slice(payload_bytes)?
            } else {
                rmp_serde::from_slice(payload_bytes)?
            };
            Ok(DecodedFrame::V3Error(error))
        }
    }
}

/// Serialize a value to msgpack or JSON bytes (helper for encode functions).
fn encode_payload_bytes(
    value: &serde_json::Value,
    use_json: bool,
) -> Result<Vec<u8>, WsError> {
    if use_json {
        Ok(serde_json::to_vec(value)?)
    } else {
        Ok(rmp_serde::to_vec(value)?)
    }
}

/// WebSocket configuration for tokio-tungstenite.
pub fn ws_config() -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE_SIZE);
    config.max_frame_size = Some(MAX_MESSAGE_SIZE);
    config
}

/// Result of the read pump — why it stopped.
#[derive(Debug)]
pub enum ReadPumpExit {
    /// Remote sent a close frame.
    ClosedByRemote(Option<CloseFrame>),
    /// Local side requested close.
    ClosedByLocal,
    /// WebSocket error.
    Error(WsError),
}

/// Run the WebSocket read pump.
///
/// Reads messages from the WS stream and forwards decoded `DecodedFrame`s to
/// `msg_tx`. Supports both v2 (legacy) and v3 binary frames.
///
/// Ping/pong is handled at the protocol level by tokio-tungstenite automatically.
/// Returns when the connection closes or errors.
///
/// Generic over the underlying stream type `S` (e.g., `TcpStream`,
/// `TokioIo<Upgraded>`, etc.).
pub async fn read_pump<S>(
    mut read_half: futures_util::stream::SplitStream<WebSocketStream<S>>,
    msg_tx: mpsc::Sender<DecodedFrame>,
) -> ReadPumpExit
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        match read_half.next().await {
            Some(Ok(Message::Binary(data))) => {
                if data.len() > MAX_MESSAGE_SIZE {
                    return ReadPumpExit::Error(WsError::MessageTooLarge(data.len()));
                }
                match decode_frame(&data) {
                    Ok(frame) => {
                        if msg_tx.send(frame).await.is_err() {
                            return ReadPumpExit::ClosedByLocal;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to decode WS message: {e}");
                        // Skip malformed messages rather than killing the connection
                    }
                }
            }
            Some(Ok(Message::Text(_text))) => {
                // Text frames are not supported in v3; only binary frames are used.
                tracing::warn!("Received text frame — not supported in v3, ignoring");
            }
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {
                // Handled by tokio-tungstenite automatically
            }
            Some(Ok(Message::Close(frame))) => {
                return ReadPumpExit::ClosedByRemote(frame);
            }
            Some(Ok(Message::Frame(_))) => {
                // Raw frame — ignore
            }
            Some(Err(e)) => {
                return ReadPumpExit::Error(WsError::Tungstenite(e));
            }
            None => {
                // Stream ended
                return ReadPumpExit::ClosedByRemote(None);
            }
        }
    }
}

/// Run the WebSocket write pump.
///
/// Reads encoded messages from `msg_rx` and writes them as WS binary frames.
/// Returns when the channel closes or a write error occurs.
///
/// Generic over the underlying stream type `S` (e.g., `TcpStream`,
/// `TokioIo<Upgraded>`, etc.).
pub async fn write_pump<S>(
    mut write_half: futures_util::stream::SplitSink<WebSocketStream<S>, Message>,
    mut msg_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<(), WsError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(data) = msg_rx.recv().await {
        write_half
            .send(Message::Binary(Bytes::from(data)))
            .await?;
    }

    // Channel closed — send close frame
    let _ = write_half
        .send(Message::Close(Some(CloseFrame {
            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
            reason: "closing".into(),
        })))
        .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frame::{ControlMessage, ErrorCode, Flags, FrameType, ProtocolError};

    #[test]
    fn ws_config_max_size() {
        let config = ws_config();
        assert_eq!(config.max_message_size, Some(MAX_MESSAGE_SIZE));
    }

    #[test]
    fn v3_msgpack_smaller_than_json() {
        let value = serde_json::json!({
            "namespace": "mesh",
            "type": "deviceAnnounce",
            "payload": {
                "deviceId": "550e8400-e29b-41d4-a716-446655440000",
                "hostname": "my-device",
                "role": "secondary",
                "status": "online",
                "uptime": 12345
            }
        });

        let msgpack = encode_data_frame(&value, false).unwrap();
        let json = encode_data_frame(&value, true).unwrap();

        assert!(
            msgpack.len() < json.len(),
            "msgpack ({}) should be smaller than json ({})",
            msgpack.len(),
            json.len()
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // v3 control frame encode/decode roundtrip tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn v3_control_ping_msgpack_roundtrip() {
        let msg = ControlMessage::Ping {
            timestamp: 1710764400000,
        };
        let encoded = encode_control_frame(&msg, false).unwrap();
        assert_eq!(encoded[0], FrameType::Control as u8);
        assert_eq!(encoded[1], 0x00); // msgpack flags

        let decoded = decode_frame(&encoded).unwrap();
        match decoded {
            DecodedFrame::V3Control(ctrl) => assert_eq!(ctrl, msg),
            other => panic!("expected V3Control, got {other:?}"),
        }
    }

    #[test]
    fn v3_control_ping_json_roundtrip() {
        let msg = ControlMessage::Ping {
            timestamp: 1710764400000,
        };
        let encoded = encode_control_frame(&msg, true).unwrap();
        assert_eq!(encoded[0], FrameType::Control as u8);
        assert_eq!(encoded[1], 0x01); // json flags

        let decoded = decode_frame(&encoded).unwrap();
        match decoded {
            DecodedFrame::V3Control(ctrl) => assert_eq!(ctrl, msg),
            other => panic!("expected V3Control, got {other:?}"),
        }
    }

    #[test]
    fn v3_control_pong_msgpack_roundtrip() {
        let msg = ControlMessage::Pong {
            timestamp: 1710764400001,
            echo_timestamp: 1710764400000,
        };
        let encoded = encode_control_frame(&msg, false).unwrap();
        let decoded = decode_frame(&encoded).unwrap();
        match decoded {
            DecodedFrame::V3Control(ctrl) => assert_eq!(ctrl, msg),
            other => panic!("expected V3Control, got {other:?}"),
        }
    }

    #[test]
    fn v3_control_pong_json_roundtrip() {
        let msg = ControlMessage::Pong {
            timestamp: 1710764400001,
            echo_timestamp: 1710764400000,
        };
        let encoded = encode_control_frame(&msg, true).unwrap();
        let decoded = decode_frame(&encoded).unwrap();
        match decoded {
            DecodedFrame::V3Control(ctrl) => assert_eq!(ctrl, msg),
            other => panic!("expected V3Control, got {other:?}"),
        }
    }

    #[test]
    fn v3_control_handshake_roundtrip() {
        let msg = ControlMessage::Handshake {
            protocol_version: 3,
            device_id: "device-abc".to_string(),
            capabilities: vec!["mesh".to_string(), "sync".to_string()],
        };
        for use_json in [true, false] {
            let encoded = encode_control_frame(&msg, use_json).unwrap();
            let decoded = decode_frame(&encoded).unwrap();
            match decoded {
                DecodedFrame::V3Control(ctrl) => assert_eq!(ctrl, msg),
                other => panic!("expected V3Control, got {other:?} (json={use_json})"),
            }
        }
    }

    #[test]
    fn v3_control_handshake_ack_roundtrip() {
        let msg = ControlMessage::HandshakeAck {
            protocol_version: 3,
            device_id: "device-xyz".to_string(),
            negotiated_version: 3,
        };
        for use_json in [true, false] {
            let encoded = encode_control_frame(&msg, use_json).unwrap();
            let decoded = decode_frame(&encoded).unwrap();
            match decoded {
                DecodedFrame::V3Control(ctrl) => assert_eq!(ctrl, msg),
                other => panic!("expected V3Control, got {other:?} (json={use_json})"),
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // v3 data frame encode/decode roundtrip tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn v3_data_msgpack_roundtrip() {
        let value = serde_json::json!({
            "namespace": "mesh",
            "type": "device-announce",
            "payload": { "deviceId": "abc123" }
        });
        let encoded = encode_data_frame(&value, false).unwrap();
        assert_eq!(encoded[0], FrameType::Data as u8);
        assert_eq!(encoded[1], 0x00);

        let decoded = decode_frame(&encoded).unwrap();
        match decoded {
            DecodedFrame::V3Data { payload, was_json } => {
                assert!(!was_json);
                assert_eq!(payload, value);
            }
            other => panic!("expected V3Data, got {other:?}"),
        }
    }

    #[test]
    fn v3_data_json_roundtrip() {
        let value = serde_json::json!({
            "namespace": "mesh",
            "type": "device-announce",
            "payload": { "deviceId": "abc123" }
        });
        let encoded = encode_data_frame(&value, true).unwrap();
        assert_eq!(encoded[0], FrameType::Data as u8);
        assert_eq!(encoded[1], 0x01);

        let decoded = decode_frame(&encoded).unwrap();
        match decoded {
            DecodedFrame::V3Data { payload, was_json } => {
                assert!(was_json);
                assert_eq!(payload, value);
            }
            other => panic!("expected V3Data, got {other:?}"),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // v3 error frame encode/decode roundtrip tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn v3_error_msgpack_roundtrip() {
        let error = ProtocolError {
            code: ErrorCode::MalformedPayload,
            message: "bad payload".to_string(),
            fatal: false,
        };
        let encoded = encode_error_frame(&error, false).unwrap();
        assert_eq!(encoded[0], FrameType::Error as u8);

        let decoded = decode_frame(&encoded).unwrap();
        match decoded {
            DecodedFrame::V3Error(e) => assert_eq!(e, error),
            other => panic!("expected V3Error, got {other:?}"),
        }
    }

    #[test]
    fn v3_error_json_roundtrip() {
        let error = ProtocolError {
            code: ErrorCode::VersionMismatch,
            message: "incompatible version".to_string(),
            fatal: true,
        };
        let encoded = encode_error_frame(&error, true).unwrap();

        let decoded = decode_frame(&encoded).unwrap();
        match decoded {
            DecodedFrame::V3Error(e) => assert_eq!(e, error),
            other => panic!("expected V3Error, got {other:?}"),
        }
    }

    #[test]
    fn v3_error_all_codes_roundtrip() {
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
            for use_json in [true, false] {
                let error = ProtocolError {
                    code,
                    message: format!("test {code}"),
                    fatal: false,
                };
                let encoded = encode_error_frame(&error, use_json).unwrap();
                let decoded = decode_frame(&encoded).unwrap();
                match decoded {
                    DecodedFrame::V3Error(e) => assert_eq!(e.code, code),
                    other => panic!("expected V3Error for {code:?}, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn decode_frame_empty() {
        let err = decode_frame(&[]).unwrap_err();
        assert!(matches!(err, WsError::EmptyBinaryFrame));
    }

    #[test]
    fn decode_frame_single_byte() {
        // A single byte 0x01 is too short for a v3 frame (needs at least 2)
        let err = decode_frame(&[0x01]).unwrap_err();
        assert!(matches!(err, WsError::FrameTooShort(1)));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // v3 frame header validation
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn v3_frame_reserved_bits_rejected() {
        // Control frame with reserved bits set in flags byte
        let data = vec![0x01, 0x02, 0x00]; // flags=0x02 has bit 1 set
        let err = decode_frame(&data).unwrap_err();
        assert!(
            matches!(err, WsError::Wire(WireError::ReservedBitsSet(0x02))),
            "expected ReservedBitsSet, got {err:?}"
        );
    }

    #[test]
    fn v3_encode_frame_raw_structure() {
        let flags = Flags::new(false);
        let payload = b"hello";
        let frame = encode_v3_frame(FrameType::Data, flags, payload);
        assert_eq!(frame[0], 0x02); // Data
        assert_eq!(frame[1], 0x00); // msgpack
        assert_eq!(&frame[2..], b"hello");
    }

    #[test]
    fn v3_data_frame_empty_payload() {
        // An empty JSON object
        let value = serde_json::json!({});
        for use_json in [true, false] {
            let encoded = encode_data_frame(&value, use_json).unwrap();
            let decoded = decode_frame(&encoded).unwrap();
            match decoded {
                DecodedFrame::V3Data { payload, .. } => assert_eq!(payload, value),
                other => panic!("expected V3Data, got {other:?}"),
            }
        }
    }

    #[test]
    fn v3_control_frame_pong_echoes_encoding() {
        // Encode ping as JSON, decode, create pong, encode pong with same encoding
        let ping = ControlMessage::Ping {
            timestamp: 12345,
        };
        let encoded_ping = encode_control_frame(&ping, true).unwrap();
        assert_eq!(encoded_ping[1], 0x01); // JSON flags

        // Decode and check
        let decoded = decode_frame(&encoded_ping).unwrap();
        let was_json = encoded_ping[1] == 0x01; // echo the flags
        match decoded {
            DecodedFrame::V3Control(ControlMessage::Ping { timestamp }) => {
                let pong = ControlMessage::Pong {
                    timestamp: 12346,
                    echo_timestamp: timestamp,
                };
                // Pong should be sent with the same encoding as the ping
                let encoded_pong = encode_control_frame(&pong, was_json).unwrap();
                assert_eq!(encoded_pong[1], 0x01); // same JSON flags
                let decoded_pong = decode_frame(&encoded_pong).unwrap();
                match decoded_pong {
                    DecodedFrame::V3Control(ControlMessage::Pong {
                        echo_timestamp, ..
                    }) => {
                        assert_eq!(echo_timestamp, 12345);
                    }
                    other => panic!("expected V3Control Pong, got {other:?}"),
                }
            }
            other => panic!("expected V3Control Ping, got {other:?}"),
        }
    }

    #[test]
    fn v3_data_large_payload_roundtrip() {
        // Test with a larger payload to ensure no size-related issues
        let mut items = Vec::new();
        for i in 0..100 {
            items.push(serde_json::json!({"id": i, "name": format!("item-{i}")}));
        }
        let value = serde_json::json!({
            "namespace": "sync",
            "type": "sync-full",
            "payload": { "items": items }
        });

        for use_json in [true, false] {
            let encoded = encode_data_frame(&value, use_json).unwrap();
            let decoded = decode_frame(&encoded).unwrap();
            match decoded {
                DecodedFrame::V3Data { payload, was_json } => {
                    assert_eq!(was_json, use_json);
                    assert_eq!(payload, value);
                }
                other => panic!("expected V3Data, got {other:?}"),
            }
        }
    }
}
