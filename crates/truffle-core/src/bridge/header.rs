use std::fmt;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Magic bytes: "TRFF" (0x54524646)
pub const MAGIC: u32 = 0x5452_4646;

/// Current protocol version
pub const VERSION: u8 = 0x01;

/// Maximum length for RequestId field
pub const MAX_REQUEST_ID_LEN: u16 = 128;

/// Maximum length for RemoteAddr field
pub const MAX_REMOTE_ADDR_LEN: u16 = 256;

/// Maximum length for RemoteDNSName field.
/// Increased from 256 to 512 to accommodate JSON-encoded PeerIdentity payloads
/// that include profile URLs.
pub const MAX_REMOTE_DNS_NAME_LEN: u16 = 512;

/// Minimum header size: magic(4) + version(1) + token(32) + direction(1) + port(2) + 3 length fields(2 each)
pub const MIN_HEADER_SIZE: usize = 4 + 1 + 32 + 1 + 2 + 2 + 2 + 2;

/// Direction of a bridge connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Direction {
    /// Connection initiated by a remote peer to us.
    Incoming = 0x01,
    /// Connection initiated by us to a remote peer.
    Outgoing = 0x02,
}

impl Direction {
    fn from_byte(b: u8) -> Result<Self, HeaderError> {
        match b {
            0x01 => Ok(Direction::Incoming),
            0x02 => Ok(Direction::Outgoing),
            _ => Err(HeaderError::InvalidDirection(b)),
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::Incoming => write!(f, "incoming"),
            Direction::Outgoing => write!(f, "outgoing"),
        }
    }
}

/// Errors that can occur when reading or writing a bridge header.
#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    #[error("invalid magic: expected 0x54524646, got 0x{0:08X}")]
    InvalidMagic(u32),

    #[error("unsupported version: expected 1, got {0}")]
    UnsupportedVersion(u8),

    #[error("invalid direction byte: 0x{0:02X}")]
    InvalidDirection(u8),

    #[error("RequestIdLen {0} exceeds maximum {MAX_REQUEST_ID_LEN}")]
    RequestIdTooLong(u16),

    #[error("RemoteAddrLen {0} exceeds maximum {MAX_REMOTE_ADDR_LEN}")]
    RemoteAddrTooLong(u16),

    #[error("RemoteDNSNameLen {0} exceeds maximum {MAX_REMOTE_DNS_NAME_LEN}")]
    RemoteDnsNameTooLong(u16),

    #[error("incoming direction must have RequestIdLen == 0, got {0}")]
    IncomingWithRequestId(u16),

    #[error("invalid UTF-8 in {field}: {source}")]
    InvalidUtf8 {
        field: &'static str,
        source: std::string::FromUtf8Error,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Binary header sent at the start of every bridge connection.
///
/// See RFC 003 "Bridge Header Format" for the wire layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeHeader {
    pub session_token: [u8; 32],
    pub direction: Direction,
    pub service_port: u16,
    pub request_id: String,
    pub remote_addr: String,
    pub remote_dns_name: String,
}

impl BridgeHeader {
    /// Read a BridgeHeader from an async reader.
    ///
    /// This does NOT apply the 2-second timeout — the caller (BridgeManager) is
    /// responsible for wrapping this in `tokio::time::timeout`.
    pub async fn read_from<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, HeaderError> {
        // Magic (4 bytes, big-endian)
        let magic = reader.read_u32().await?;
        if magic != MAGIC {
            return Err(HeaderError::InvalidMagic(magic));
        }

        // Version (1 byte)
        let version = reader.read_u8().await?;
        if version != VERSION {
            return Err(HeaderError::UnsupportedVersion(version));
        }

        // SessionToken (32 bytes)
        let mut session_token = [0u8; 32];
        reader.read_exact(&mut session_token).await?;

        // Direction (1 byte)
        let direction = Direction::from_byte(reader.read_u8().await?)?;

        // ServicePort (2 bytes, big-endian)
        let service_port = reader.read_u16().await?;

        // RequestIdLen (2 bytes, big-endian)
        let request_id_len = reader.read_u16().await?;
        if request_id_len > MAX_REQUEST_ID_LEN {
            return Err(HeaderError::RequestIdTooLong(request_id_len));
        }

        // Incoming direction invariant: RequestIdLen must be 0
        if direction == Direction::Incoming && request_id_len != 0 {
            return Err(HeaderError::IncomingWithRequestId(request_id_len));
        }

        // RequestId (N bytes, UTF-8)
        let request_id = if request_id_len > 0 {
            let mut buf = vec![0u8; request_id_len as usize];
            reader.read_exact(&mut buf).await?;
            String::from_utf8(buf).map_err(|e| HeaderError::InvalidUtf8 {
                field: "RequestId",
                source: e,
            })?
        } else {
            String::new()
        };

        // RemoteAddrLen (2 bytes, big-endian)
        let remote_addr_len = reader.read_u16().await?;
        if remote_addr_len > MAX_REMOTE_ADDR_LEN {
            return Err(HeaderError::RemoteAddrTooLong(remote_addr_len));
        }

        // RemoteAddr (M bytes, UTF-8)
        let remote_addr = if remote_addr_len > 0 {
            let mut buf = vec![0u8; remote_addr_len as usize];
            reader.read_exact(&mut buf).await?;
            String::from_utf8(buf).map_err(|e| HeaderError::InvalidUtf8 {
                field: "RemoteAddr",
                source: e,
            })?
        } else {
            String::new()
        };

        // RemoteDNSNameLen (2 bytes, big-endian)
        let remote_dns_name_len = reader.read_u16().await?;
        if remote_dns_name_len > MAX_REMOTE_DNS_NAME_LEN {
            return Err(HeaderError::RemoteDnsNameTooLong(remote_dns_name_len));
        }

        // RemoteDNSName (K bytes, UTF-8)
        let remote_dns_name = if remote_dns_name_len > 0 {
            let mut buf = vec![0u8; remote_dns_name_len as usize];
            reader.read_exact(&mut buf).await?;
            String::from_utf8(buf).map_err(|e| HeaderError::InvalidUtf8 {
                field: "RemoteDNSName",
                source: e,
            })?
        } else {
            String::new()
        };

        Ok(BridgeHeader {
            session_token,
            direction,
            service_port,
            request_id,
            remote_addr,
            remote_dns_name,
        })
    }

    /// Write a BridgeHeader to an async writer.
    pub async fn write_to<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> Result<(), HeaderError> {
        let request_id_bytes = self.request_id.as_bytes();
        let remote_addr_bytes = self.remote_addr.as_bytes();
        let remote_dns_name_bytes = self.remote_dns_name.as_bytes();

        // Validate lengths before writing
        if request_id_bytes.len() > MAX_REQUEST_ID_LEN as usize {
            return Err(HeaderError::RequestIdTooLong(request_id_bytes.len() as u16));
        }
        if remote_addr_bytes.len() > MAX_REMOTE_ADDR_LEN as usize {
            return Err(HeaderError::RemoteAddrTooLong(
                remote_addr_bytes.len() as u16,
            ));
        }
        if remote_dns_name_bytes.len() > MAX_REMOTE_DNS_NAME_LEN as usize {
            return Err(HeaderError::RemoteDnsNameTooLong(
                remote_dns_name_bytes.len() as u16,
            ));
        }

        // Validate incoming direction invariant
        if self.direction == Direction::Incoming && !self.request_id.is_empty() {
            return Err(HeaderError::IncomingWithRequestId(
                request_id_bytes.len() as u16,
            ));
        }

        // Build the complete header into a buffer for a single write
        let total_len = MIN_HEADER_SIZE
            + request_id_bytes.len()
            + remote_addr_bytes.len()
            + remote_dns_name_bytes.len();
        let mut buf = Vec::with_capacity(total_len);

        // Magic
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        // Version
        buf.push(VERSION);
        // SessionToken
        buf.extend_from_slice(&self.session_token);
        // Direction
        buf.push(self.direction as u8);
        // ServicePort
        buf.extend_from_slice(&self.service_port.to_be_bytes());
        // RequestIdLen + RequestId
        buf.extend_from_slice(&(request_id_bytes.len() as u16).to_be_bytes());
        buf.extend_from_slice(request_id_bytes);
        // RemoteAddrLen + RemoteAddr
        buf.extend_from_slice(&(remote_addr_bytes.len() as u16).to_be_bytes());
        buf.extend_from_slice(remote_addr_bytes);
        // RemoteDNSNameLen + RemoteDNSName
        buf.extend_from_slice(&(remote_dns_name_bytes.len() as u16).to_be_bytes());
        buf.extend_from_slice(remote_dns_name_bytes);

        writer.write_all(&buf).await?;
        writer.flush().await?;

        Ok(())
    }

    /// Returns the serialized byte length of this header.
    pub fn wire_len(&self) -> usize {
        MIN_HEADER_SIZE
            + self.request_id.len()
            + self.remote_addr.len()
            + self.remote_dns_name.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn test_token() -> [u8; 32] {
        let mut token = [0u8; 32];
        for (i, byte) in token.iter_mut().enumerate() {
            *byte = i as u8;
        }
        token
    }

    fn make_incoming_header() -> BridgeHeader {
        BridgeHeader {
            session_token: test_token(),
            direction: Direction::Incoming,
            service_port: 443,
            request_id: String::new(),
            remote_addr: "100.64.0.2:12345".to_string(),
            remote_dns_name: "peer-host.tailnet.ts.net".to_string(),
        }
    }

    fn make_outgoing_header() -> BridgeHeader {
        BridgeHeader {
            session_token: test_token(),
            direction: Direction::Outgoing,
            service_port: 9417,
            request_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            remote_addr: "100.64.0.3:9417".to_string(),
            remote_dns_name: "other-host.tailnet.ts.net".to_string(),
        }
    }

    // ---- Golden test vectors ----
    // These exact byte sequences must match the Go shim's test vectors.

    #[tokio::test]
    async fn golden_incoming_roundtrip() {
        let header = make_incoming_header();

        // Write to bytes
        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();

        // Verify exact wire format
        assert_eq!(&buf[0..4], &[0x54, 0x52, 0x46, 0x46], "magic");
        assert_eq!(buf[4], 0x01, "version");
        assert_eq!(&buf[5..37], &test_token(), "session token");
        assert_eq!(buf[37], 0x01, "direction=incoming");
        assert_eq!(&buf[38..40], &[0x01, 0xBB], "port=443");
        assert_eq!(&buf[40..42], &[0x00, 0x00], "request_id_len=0");
        // remote_addr_len = 16 ("100.64.0.2:12345")
        assert_eq!(&buf[42..44], &[0x00, 0x10], "remote_addr_len=16");
        assert_eq!(
            &buf[44..60],
            b"100.64.0.2:12345",
            "remote_addr"
        );
        // remote_dns_name_len = 24 ("peer-host.tailnet.ts.net")
        assert_eq!(&buf[60..62], &[0x00, 0x18], "remote_dns_name_len=24");
        assert_eq!(
            &buf[62..86],
            b"peer-host.tailnet.ts.net",
            "remote_dns_name"
        );
        assert_eq!(buf.len(), 86, "total wire length");

        // Read back
        let mut cursor = Cursor::new(&buf);
        let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(parsed, header);
    }

    #[tokio::test]
    async fn golden_outgoing_roundtrip() {
        let header = make_outgoing_header();

        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();

        // Verify key fields
        assert_eq!(buf[37], 0x02, "direction=outgoing");
        assert_eq!(&buf[38..40], &[0x24, 0xC9], "port=9417");
        // request_id_len = 36
        assert_eq!(&buf[40..42], &[0x00, 0x24], "request_id_len=36");
        let request_id_end = 42 + 36;
        assert_eq!(
            &buf[42..request_id_end],
            b"550e8400-e29b-41d4-a716-446655440000",
            "request_id"
        );

        // Read back
        let mut cursor = Cursor::new(&buf);
        let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(parsed, header);
    }

    #[tokio::test]
    async fn golden_minimal_header() {
        // All variable-length fields empty
        let header = BridgeHeader {
            session_token: [0xAA; 32],
            direction: Direction::Incoming,
            service_port: 443,
            request_id: String::new(),
            remote_addr: String::new(),
            remote_dns_name: String::new(),
        };

        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();

        assert_eq!(buf.len(), MIN_HEADER_SIZE, "minimal header = fixed fields only");
        assert_eq!(&buf[40..42], &[0x00, 0x00], "request_id_len=0");
        assert_eq!(&buf[42..44], &[0x00, 0x00], "remote_addr_len=0");
        assert_eq!(&buf[44..46], &[0x00, 0x00], "remote_dns_name_len=0");

        let mut cursor = Cursor::new(&buf);
        let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(parsed, header);
    }

    // ---- Error cases ----

    #[tokio::test]
    async fn reject_invalid_magic() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // wrong magic
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]); // token
        buf.push(0x01); // direction
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // req id len
        buf.extend_from_slice(&0u16.to_be_bytes()); // remote addr len
        buf.extend_from_slice(&0u16.to_be_bytes()); // remote dns name len

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::InvalidMagic(0xDEADBEEF)),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn reject_unsupported_version() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(0x02); // wrong version
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01);
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::UnsupportedVersion(2)),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn reject_invalid_direction() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x03); // invalid direction
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::InvalidDirection(0x03)),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn reject_oversized_request_id() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x02); // outgoing
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&200u16.to_be_bytes()); // exceeds MAX_REQUEST_ID_LEN(128)

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::RequestIdTooLong(200)),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn reject_oversized_remote_addr() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01); // incoming
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // request_id_len=0
        buf.extend_from_slice(&300u16.to_be_bytes()); // exceeds MAX_REMOTE_ADDR_LEN(256)

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::RemoteAddrTooLong(300)),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn reject_oversized_remote_dns_name() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01); // incoming
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // request_id_len=0
        buf.extend_from_slice(&0u16.to_be_bytes()); // remote_addr_len=0
        buf.extend_from_slice(&600u16.to_be_bytes()); // exceeds MAX_REMOTE_DNS_NAME_LEN(512)

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::RemoteDnsNameTooLong(600)),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn reject_incoming_with_request_id() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01); // incoming
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&5u16.to_be_bytes()); // non-zero request_id for incoming
        buf.extend_from_slice(b"hello");

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::IncomingWithRequestId(5)),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn reject_truncated_header() {
        // Only magic + version, no token
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, HeaderError::Io(_)), "got: {err:?}");
    }

    #[tokio::test]
    async fn reject_random_bytes() {
        let buf = vec![0xFF, 0x00, 0xAB, 0xCD, 0xEF];
        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, HeaderError::InvalidMagic(_)), "got: {err:?}");
    }

    #[tokio::test]
    async fn wire_len_matches_serialized() {
        let header = make_outgoing_header();
        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();
        assert_eq!(buf.len(), header.wire_len());
    }

    #[tokio::test]
    async fn write_rejects_incoming_with_request_id() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Incoming,
            service_port: 443,
            request_id: "should-not-be-here".to_string(),
            remote_addr: String::new(),
            remote_dns_name: String::new(),
        };

        let mut buf = Vec::new();
        let err = header.write_to(&mut buf).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::IncomingWithRequestId(_)),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn write_rejects_oversized_request_id() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Outgoing,
            service_port: 443,
            request_id: "x".repeat(200),
            remote_addr: String::new(),
            remote_dns_name: String::new(),
        };

        let mut buf = Vec::new();
        let err = header.write_to(&mut buf).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::RequestIdTooLong(_)),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn port_9417_roundtrip() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Incoming,
            service_port: 9417,
            request_id: String::new(),
            remote_addr: "100.64.0.5:54321".to_string(),
            remote_dns_name: "file-peer.tailnet.ts.net".to_string(),
        };

        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();

        let mut cursor = Cursor::new(&buf);
        let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(parsed, header);
        assert_eq!(parsed.service_port, 9417);
    }

    #[tokio::test]
    async fn max_length_fields_roundtrip() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Outgoing,
            service_port: 443,
            request_id: "a".repeat(MAX_REQUEST_ID_LEN as usize),
            remote_addr: "b".repeat(MAX_REMOTE_ADDR_LEN as usize),
            remote_dns_name: "c".repeat(MAX_REMOTE_DNS_NAME_LEN as usize),
        };

        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();

        let mut cursor = Cursor::new(&buf);
        let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(parsed, header);
    }

    // ── Adversarial edge case tests ─────────────────────────────────────

    /// Edge case 1: Truncated header — only magic bytes, nothing else.
    /// Must return an I/O error (UnexpectedEof), never panic.
    #[tokio::test]
    async fn adversarial_truncated_header_only_magic() {
        let buf = MAGIC.to_be_bytes().to_vec();
        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, HeaderError::Io(_)), "got: {err:?}");
    }

    /// Edge case 1b: Truncated header — magic + version + partial token (10 bytes of 32).
    #[tokio::test]
    async fn adversarial_truncated_header_partial_token() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0xAA; 10]); // only 10 of 32 token bytes
        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, HeaderError::Io(_)), "got: {err:?}");
    }

    /// Edge case 1c: Truncated header — everything correct up to the service port,
    /// but missing the length fields.
    #[tokio::test]
    async fn adversarial_truncated_header_missing_length_fields() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]); // token
        buf.push(0x01); // direction incoming
        buf.extend_from_slice(&443u16.to_be_bytes()); // service port
        // missing: request_id_len, remote_addr_len, remote_dns_name_len
        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, HeaderError::Io(_)), "got: {err:?}");
    }

    /// Edge case 1d: Empty input — zero bytes.
    #[tokio::test]
    async fn adversarial_empty_input() {
        let buf: Vec<u8> = vec![];
        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, HeaderError::Io(_)), "got: {err:?}");
    }

    /// Edge case 2: Zero-length request_id with outgoing direction.
    /// This is valid — outgoing connections can have empty request_id.
    #[tokio::test]
    async fn adversarial_zero_length_request_id_outgoing() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Outgoing,
            service_port: 443,
            request_id: String::new(), // zero-length
            remote_addr: "10.0.0.1:80".to_string(),
            remote_dns_name: String::new(),
        };
        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();

        let mut cursor = Cursor::new(&buf);
        let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(parsed.request_id, "");
        assert_eq!(parsed.direction, Direction::Outgoing);
    }

    /// Edge case 3: Request ID at exactly MAX_LEN (128 bytes).
    #[tokio::test]
    async fn adversarial_request_id_exactly_max_len() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Outgoing,
            service_port: 443,
            request_id: "x".repeat(MAX_REQUEST_ID_LEN as usize),
            remote_addr: String::new(),
            remote_dns_name: String::new(),
        };
        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();

        let mut cursor = Cursor::new(&buf);
        let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(parsed.request_id.len(), MAX_REQUEST_ID_LEN as usize);
    }

    /// Edge case 3b: Remote addr at exactly MAX_LEN (256 bytes).
    #[tokio::test]
    async fn adversarial_remote_addr_exactly_max_len() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Incoming,
            service_port: 443,
            request_id: String::new(),
            remote_addr: "a".repeat(MAX_REMOTE_ADDR_LEN as usize),
            remote_dns_name: String::new(),
        };
        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();

        let mut cursor = Cursor::new(&buf);
        let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(parsed.remote_addr.len(), MAX_REMOTE_ADDR_LEN as usize);
    }

    /// Edge case 3c: Remote DNS name at exactly MAX_LEN (512 bytes).
    #[tokio::test]
    async fn adversarial_remote_dns_name_exactly_max_len() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Incoming,
            service_port: 443,
            request_id: String::new(),
            remote_addr: String::new(),
            remote_dns_name: "d".repeat(MAX_REMOTE_DNS_NAME_LEN as usize),
        };
        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();

        let mut cursor = Cursor::new(&buf);
        let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(parsed.remote_dns_name.len(), MAX_REMOTE_DNS_NAME_LEN as usize);
    }

    /// Edge case 4: Request ID one byte over MAX_LEN — rejected on read.
    #[tokio::test]
    async fn adversarial_request_id_one_over_max() {
        let over_len = (MAX_REQUEST_ID_LEN + 1) as u16;
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x02); // outgoing
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&over_len.to_be_bytes());
        // Don't need actual data — the length check fires first
        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::RequestIdTooLong(len) if len == over_len),
            "got: {err:?}"
        );
    }

    /// Edge case 4b: Remote addr one byte over MAX_LEN — rejected on read.
    #[tokio::test]
    async fn adversarial_remote_addr_one_over_max() {
        let over_len = (MAX_REMOTE_ADDR_LEN + 1) as u16;
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01); // incoming
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // request_id_len = 0
        buf.extend_from_slice(&over_len.to_be_bytes()); // remote_addr_len one over
        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::RemoteAddrTooLong(len) if len == over_len),
            "got: {err:?}"
        );
    }

    /// Edge case 4c: Remote DNS name one byte over MAX_LEN — rejected on read.
    #[tokio::test]
    async fn adversarial_remote_dns_name_one_over_max() {
        let over_len = (MAX_REMOTE_DNS_NAME_LEN + 1) as u16;
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01); // incoming
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // request_id_len = 0
        buf.extend_from_slice(&0u16.to_be_bytes()); // remote_addr_len = 0
        buf.extend_from_slice(&over_len.to_be_bytes()); // remote_dns_name_len one over
        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::RemoteDnsNameTooLong(len) if len == over_len),
            "got: {err:?}"
        );
    }

    /// Edge case 4d: Write rejects request_id one byte over MAX_LEN.
    #[tokio::test]
    async fn adversarial_write_rejects_request_id_one_over_max() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Outgoing,
            service_port: 443,
            request_id: "x".repeat(MAX_REQUEST_ID_LEN as usize + 1),
            remote_addr: String::new(),
            remote_dns_name: String::new(),
        };
        let mut buf = Vec::new();
        let err = header.write_to(&mut buf).await.unwrap_err();
        assert!(matches!(err, HeaderError::RequestIdTooLong(_)), "got: {err:?}");
    }

    /// Edge case 4e: Write rejects remote_addr one byte over MAX_LEN.
    #[tokio::test]
    async fn adversarial_write_rejects_remote_addr_one_over_max() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Incoming,
            service_port: 443,
            request_id: String::new(),
            remote_addr: "x".repeat(MAX_REMOTE_ADDR_LEN as usize + 1),
            remote_dns_name: String::new(),
        };
        let mut buf = Vec::new();
        let err = header.write_to(&mut buf).await.unwrap_err();
        assert!(matches!(err, HeaderError::RemoteAddrTooLong(_)), "got: {err:?}");
    }

    /// Edge case 4f: Write rejects remote_dns_name one byte over MAX_LEN.
    #[tokio::test]
    async fn adversarial_write_rejects_remote_dns_name_one_over_max() {
        let header = BridgeHeader {
            session_token: test_token(),
            direction: Direction::Incoming,
            service_port: 443,
            request_id: String::new(),
            remote_addr: String::new(),
            remote_dns_name: "x".repeat(MAX_REMOTE_DNS_NAME_LEN as usize + 1),
        };
        let mut buf = Vec::new();
        let err = header.write_to(&mut buf).await.unwrap_err();
        assert!(matches!(err, HeaderError::RemoteDnsNameTooLong(_)), "got: {err:?}");
    }

    /// Edge case 5: Wrong magic bytes "TRFG" (0x54524647) instead of "TRFF" (0x54524646).
    #[tokio::test]
    async fn adversarial_wrong_magic_trfg() {
        let wrong_magic: u32 = 0x5452_4647; // "TRFG"
        let mut buf = Vec::new();
        buf.extend_from_slice(&wrong_magic.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01);
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::InvalidMagic(0x5452_4647)),
            "got: {err:?}"
        );
    }

    /// Edge case 6: Wrong version 0x02 instead of 0x01.
    #[tokio::test]
    async fn adversarial_wrong_version_0x02() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(0x02); // wrong version
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01);
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::UnsupportedVersion(2)),
            "got: {err:?}"
        );
    }

    /// Edge case 6b: Wrong version 0x00.
    #[tokio::test]
    async fn adversarial_wrong_version_0x00() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(0x00);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01);
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::UnsupportedVersion(0)),
            "got: {err:?}"
        );
    }

    /// Edge case 6c: Wrong version 0xFF.
    #[tokio::test]
    async fn adversarial_wrong_version_0xff() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(0xFF);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01);
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::UnsupportedVersion(0xFF)),
            "got: {err:?}"
        );
    }

    /// Edge case 7: Direction byte 0x00 (invalid — neither incoming nor outgoing).
    #[tokio::test]
    async fn adversarial_direction_0x00() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x00); // invalid direction
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::InvalidDirection(0x00)),
            "got: {err:?}"
        );
    }

    /// Edge case 8: Direction byte 0xFF (out of range).
    #[tokio::test]
    async fn adversarial_direction_0xff() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0xFF); // out of range direction
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::InvalidDirection(0xFF)),
            "got: {err:?}"
        );
    }

    /// Edge case: Direction byte 0x03 (adjacent to valid range).
    #[tokio::test]
    async fn adversarial_direction_0x03() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x03);
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::InvalidDirection(0x03)),
            "got: {err:?}"
        );
    }

    /// Edge case: Header claims a request_id_len but the data is truncated
    /// (declared length exceeds available bytes).
    #[tokio::test]
    async fn adversarial_request_id_declared_but_truncated() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x02); // outgoing
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&50u16.to_be_bytes()); // claims 50 bytes
        buf.extend_from_slice(b"short"); // only 5 bytes
        // EOF after 5 bytes

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, HeaderError::Io(_)), "got: {err:?}");
    }

    /// Edge case: Invalid UTF-8 in the request_id field.
    #[tokio::test]
    async fn adversarial_invalid_utf8_in_request_id() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x02); // outgoing
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&4u16.to_be_bytes()); // 4 bytes
        buf.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0xFC]); // invalid UTF-8

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::InvalidUtf8 { field: "RequestId", .. }),
            "got: {err:?}"
        );
    }

    /// Edge case: Invalid UTF-8 in the remote_addr field.
    #[tokio::test]
    async fn adversarial_invalid_utf8_in_remote_addr() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01); // incoming
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // request_id_len = 0
        buf.extend_from_slice(&3u16.to_be_bytes()); // remote_addr_len = 3
        buf.extend_from_slice(&[0x80, 0x81, 0x82]); // invalid UTF-8

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::InvalidUtf8 { field: "RemoteAddr", .. }),
            "got: {err:?}"
        );
    }

    /// Edge case: Invalid UTF-8 in the remote_dns_name field.
    #[tokio::test]
    async fn adversarial_invalid_utf8_in_remote_dns_name() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01); // incoming
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // request_id_len = 0
        buf.extend_from_slice(&0u16.to_be_bytes()); // remote_addr_len = 0
        buf.extend_from_slice(&2u16.to_be_bytes()); // remote_dns_name_len = 2
        buf.extend_from_slice(&[0xC0, 0x01]); // invalid UTF-8

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(
            matches!(err, HeaderError::InvalidUtf8 { field: "RemoteDNSName", .. }),
            "got: {err:?}"
        );
    }

    /// Edge case: All-zero magic bytes (0x00000000).
    #[tokio::test]
    async fn adversarial_all_zero_magic() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.push(VERSION);
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0x01);
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, HeaderError::InvalidMagic(0)), "got: {err:?}");
    }

    /// Edge case: Single byte input.
    #[tokio::test]
    async fn adversarial_single_byte_input() {
        let buf = vec![0x54]; // just the first byte of "TRFF"
        let mut cursor = Cursor::new(&buf);
        let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, HeaderError::Io(_)), "got: {err:?}");
    }

    /// Edge case: Maximum possible field lengths simultaneously (all at max).
    /// Verify write + read roundtrip.
    #[tokio::test]
    async fn adversarial_all_fields_at_max_simultaneously() {
        let header = BridgeHeader {
            session_token: [0xFF; 32],
            direction: Direction::Outgoing,
            service_port: u16::MAX,
            request_id: "R".repeat(MAX_REQUEST_ID_LEN as usize),
            remote_addr: "A".repeat(MAX_REMOTE_ADDR_LEN as usize),
            remote_dns_name: "D".repeat(MAX_REMOTE_DNS_NAME_LEN as usize),
        };
        let mut buf = Vec::new();
        header.write_to(&mut buf).await.unwrap();

        let expected_len = MIN_HEADER_SIZE
            + MAX_REQUEST_ID_LEN as usize
            + MAX_REMOTE_ADDR_LEN as usize
            + MAX_REMOTE_DNS_NAME_LEN as usize;
        assert_eq!(buf.len(), expected_len);

        let mut cursor = Cursor::new(&buf);
        let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(parsed, header);
    }
}
