use super::envelope::MeshEnvelope;

/// 1-byte flags prefix on WS binary payload.
pub const FLAG_FORMAT_MSGPACK: u8 = 0x00;
pub const FLAG_FORMAT_JSON: u8 = 0x02;

/// Errors from codec operations.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("empty payload (missing flags byte)")]
    EmptyPayload,

    #[error("unknown format flag: 0x{0:02X}")]
    UnknownFormat(u8),

    #[error("MessagePack decode error: {0}")]
    MsgpackDecode(#[from] rmp_serde::decode::Error),

    #[error("MessagePack encode error: {0}")]
    MsgpackEncode(#[from] rmp_serde::encode::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Encode a MeshEnvelope for sending over WebSocket.
///
/// Uses 1-byte flags prefix: 0x00 = MessagePack, 0x02 = JSON.
/// WebSocket provides message framing, so no length prefix is needed.
pub fn encode_envelope(envelope: &MeshEnvelope, use_json: bool) -> Result<Vec<u8>, CodecError> {
    if use_json {
        let json_bytes = serde_json::to_vec(envelope)?;
        let mut buf = Vec::with_capacity(1 + json_bytes.len());
        buf.push(FLAG_FORMAT_JSON);
        buf.extend_from_slice(&json_bytes);
        Ok(buf)
    } else {
        // Use named (map-style) MessagePack to handle optional fields correctly.
        let msgpack_bytes = rmp_serde::to_vec_named(envelope)?;
        let mut buf = Vec::with_capacity(1 + msgpack_bytes.len());
        buf.push(FLAG_FORMAT_MSGPACK);
        buf.extend_from_slice(&msgpack_bytes);
        Ok(buf)
    }
}

/// Decode a binary WebSocket message into a MeshEnvelope.
///
/// Reads the 1-byte flags prefix to determine serialization format.
pub fn decode_envelope(data: &[u8]) -> Result<(MeshEnvelope, bool), CodecError> {
    if data.is_empty() {
        return Err(CodecError::EmptyPayload);
    }

    let flags = data[0];
    let payload_bytes = &data[1..];

    let format = flags & 0x06; // bits 1-2
    match format {
        FLAG_FORMAT_MSGPACK => {
            let envelope: MeshEnvelope = rmp_serde::from_slice(payload_bytes)?;
            Ok((envelope, false))
        }
        FLAG_FORMAT_JSON => {
            let envelope: MeshEnvelope = serde_json::from_slice(payload_bytes)?;
            Ok((envelope, true))
        }
        _ => Err(CodecError::UnknownFormat(flags)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_msgpack_roundtrip() {
        let envelope = MeshEnvelope::new("mesh", "announce", serde_json::json!({"deviceId": "abc123"}));
        let encoded = encode_envelope(&envelope, false).unwrap();
        assert_eq!(encoded[0], FLAG_FORMAT_MSGPACK);

        let (decoded, was_json) = decode_envelope(&encoded).unwrap();
        assert!(!was_json);
        assert_eq!(decoded.namespace, "mesh");
        assert_eq!(decoded.msg_type, "announce");
    }

    #[test]
    fn encode_decode_json_roundtrip() {
        let envelope = MeshEnvelope::new("mesh", "announce", serde_json::json!({"deviceId": "abc123"}));
        let encoded = encode_envelope(&envelope, true).unwrap();
        assert_eq!(encoded[0], FLAG_FORMAT_JSON);

        let (decoded, was_json) = decode_envelope(&encoded).unwrap();
        assert!(was_json);
        assert_eq!(decoded.namespace, "mesh");
        assert_eq!(decoded.msg_type, "announce");
    }

    #[test]
    fn decode_empty_payload() {
        let err = decode_envelope(&[]).unwrap_err();
        assert!(matches!(err, CodecError::EmptyPayload));
    }

    #[test]
    fn decode_unknown_format() {
        let data = vec![0x04, 0x00];
        let err = decode_envelope(&data).unwrap_err();
        assert!(matches!(err, CodecError::UnknownFormat(0x04)));
    }

    #[test]
    fn msgpack_smaller_than_json() {
        let envelope = MeshEnvelope::new(
            "mesh",
            "deviceAnnounce",
            serde_json::json!({
                "deviceId": "550e8400-e29b-41d4-a716-446655440000",
                "hostname": "my-device",
                "role": "secondary",
                "status": "online",
                "uptime": 12345
            }),
        );

        let msgpack = encode_envelope(&envelope, false).unwrap();
        let json = encode_envelope(&envelope, true).unwrap();

        assert!(
            msgpack.len() < json.len(),
            "msgpack ({}) should be smaller than json ({})",
            msgpack.len(),
            json.len()
        );
    }
}
