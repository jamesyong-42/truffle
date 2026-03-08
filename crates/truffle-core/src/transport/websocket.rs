use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{
    protocol::{CloseFrame, WebSocketConfig},
    Message,
};
use tokio_tungstenite::WebSocketStream;

/// Maximum WebSocket message size (16 MB, matching old FrameCodec MAX_MESSAGE_SIZE).
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// 1-byte flags prefix on WS binary payload.
pub const FLAG_FORMAT_MSGPACK: u8 = 0x00;
pub const FLAG_FORMAT_JSON: u8 = 0x02;

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

    #[error("unknown format flag: 0x{0:02X}")]
    UnknownFormat(u8),

    #[error("MessagePack decode error: {0}")]
    MsgpackDecode(#[from] rmp_serde::decode::Error),

    #[error("MessagePack encode error: {0}")]
    MsgpackEncode(#[from] rmp_serde::encode::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// A decoded mesh message from the wire.
#[derive(Debug, Clone)]
pub struct WireMessage {
    /// The deserialized envelope.
    pub payload: serde_json::Value,
    /// Whether this was received as JSON (debug mode) or MessagePack.
    pub was_json: bool,
}

/// WebSocket configuration for tokio-tungstenite.
pub fn ws_config() -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE_SIZE);
    config.max_frame_size = Some(MAX_MESSAGE_SIZE);
    config
}

/// Encode a message for sending over WebSocket.
///
/// Uses 1-byte flags prefix: 0x00 = MessagePack, 0x02 = JSON.
pub fn encode_message(value: &serde_json::Value, use_json: bool) -> Result<Vec<u8>, WsError> {
    if use_json {
        let json_bytes = serde_json::to_vec(value)?;
        let mut buf = Vec::with_capacity(1 + json_bytes.len());
        buf.push(FLAG_FORMAT_JSON);
        buf.extend_from_slice(&json_bytes);
        Ok(buf)
    } else {
        let msgpack_bytes = rmp_serde::to_vec(value)?;
        let mut buf = Vec::with_capacity(1 + msgpack_bytes.len());
        buf.push(FLAG_FORMAT_MSGPACK);
        buf.extend_from_slice(&msgpack_bytes);
        Ok(buf)
    }
}

/// Decode a binary WebSocket message.
///
/// Reads the 1-byte flags prefix to determine serialization format.
pub fn decode_message(data: &[u8]) -> Result<WireMessage, WsError> {
    if data.is_empty() {
        return Err(WsError::EmptyBinaryFrame);
    }

    let flags = data[0];
    let payload_bytes = &data[1..];

    let format = flags & 0x06; // bits 1-2
    match format {
        FLAG_FORMAT_MSGPACK => {
            let payload: serde_json::Value = rmp_serde::from_slice(payload_bytes)?;
            Ok(WireMessage {
                payload,
                was_json: false,
            })
        }
        FLAG_FORMAT_JSON => {
            let payload: serde_json::Value = serde_json::from_slice(payload_bytes)?;
            Ok(WireMessage {
                payload,
                was_json: true,
            })
        }
        _ => Err(WsError::UnknownFormat(flags)),
    }
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
/// Reads messages from the WS stream and forwards decoded messages to `msg_tx`.
/// Ping/pong is handled at the protocol level by tokio-tungstenite automatically.
/// Returns when the connection closes or errors.
pub async fn read_pump(
    mut read_half: futures_util::stream::SplitStream<WebSocketStream<TcpStream>>,
    msg_tx: mpsc::Sender<WireMessage>,
) -> ReadPumpExit {
    loop {
        match read_half.next().await {
            Some(Ok(Message::Binary(data))) => {
                if data.len() > MAX_MESSAGE_SIZE {
                    return ReadPumpExit::Error(WsError::MessageTooLarge(data.len()));
                }
                match decode_message(&data) {
                    Ok(msg) => {
                        if msg_tx.send(msg).await.is_err() {
                            return ReadPumpExit::ClosedByLocal;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to decode WS message: {e}");
                        // Skip malformed messages rather than killing the connection
                    }
                }
            }
            Some(Ok(Message::Text(text))) => {
                // Legacy text frames: try to parse as JSON
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(payload) => {
                        let msg = WireMessage {
                            payload,
                            was_json: true,
                        };
                        if msg_tx.send(msg).await.is_err() {
                            return ReadPumpExit::ClosedByLocal;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to parse WS text message: {e}");
                    }
                }
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
pub async fn write_pump(
    mut write_half: futures_util::stream::SplitSink<WebSocketStream<TcpStream>, Message>,
    mut msg_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<(), WsError> {
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

    #[test]
    fn encode_decode_msgpack_roundtrip() {
        let value = serde_json::json!({
            "namespace": "mesh",
            "type": "announce",
            "payload": { "deviceId": "abc123" }
        });

        let encoded = encode_message(&value, false).unwrap();
        assert_eq!(encoded[0], FLAG_FORMAT_MSGPACK);

        let decoded = decode_message(&encoded).unwrap();
        assert!(!decoded.was_json);
        assert_eq!(decoded.payload, value);
    }

    #[test]
    fn encode_decode_json_roundtrip() {
        let value = serde_json::json!({
            "namespace": "mesh",
            "type": "announce",
            "payload": { "deviceId": "abc123" }
        });

        let encoded = encode_message(&value, true).unwrap();
        assert_eq!(encoded[0], FLAG_FORMAT_JSON);

        let decoded = decode_message(&encoded).unwrap();
        assert!(decoded.was_json);
        assert_eq!(decoded.payload, value);
    }

    #[test]
    fn decode_empty_binary_frame() {
        let err = decode_message(&[]).unwrap_err();
        assert!(matches!(err, WsError::EmptyBinaryFrame));
    }

    #[test]
    fn decode_unknown_format() {
        // flags = 0x04 (bits 1-2 = 10, which is reserved)
        let data = vec![0x04, 0x00];
        let err = decode_message(&data).unwrap_err();
        assert!(matches!(err, WsError::UnknownFormat(0x04)));
    }

    #[test]
    fn msgpack_smaller_than_json() {
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

        let msgpack = encode_message(&value, false).unwrap();
        let json = encode_message(&value, true).unwrap();

        assert!(
            msgpack.len() < json.len(),
            "msgpack ({}) should be smaller than json ({})",
            msgpack.len(),
            json.len()
        );
    }

    #[test]
    fn ws_config_max_size() {
        let config = ws_config();
        assert_eq!(config.max_message_size, Some(MAX_MESSAGE_SIZE));
    }
}
