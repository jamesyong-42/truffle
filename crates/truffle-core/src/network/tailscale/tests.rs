//! Tests for the Tailscale network provider internals.
//!
//! ## Unit tests
//! - Binary header serialization/deserialization
//! - Command/event JSON serialization
//! - Peer event filtering (only "truffle-*" hostnames)
//!
//! ## Integration tests (require real Tailscale network)
//! - Start provider, authenticate, discover peers
//! - dial_tcp between two nodes, exchange bytes
//! - listen_tcp, accept incoming connection
//! - ping between nodes
//! - WatchIPNBus real-time peer events
//!
//! Integration tests are marked with `#[ignore]` and documented below.

use super::header::{BridgeHeader, Direction, HeaderError, MAGIC, MIN_HEADER_SIZE, VERSION};
use super::protocol::*;
use std::io::Cursor;

// ===== Helper functions =====

fn test_token() -> [u8; 32] {
    let mut token = [0u8; 32];
    for (i, byte) in token.iter_mut().enumerate() {
        *byte = i as u8;
    }
    token
}

// ===== Binary header tests =====

#[tokio::test]
async fn header_roundtrip_incoming() {
    let header = BridgeHeader {
        session_token: test_token(),
        direction: Direction::Incoming,
        service_port: 443,
        request_id: String::new(),
        remote_addr: "100.64.0.2:12345".to_string(),
        remote_dns_name: "peer.tailnet.ts.net".to_string(),
    };

    let mut buf = Vec::new();
    header.write_to(&mut buf).await.unwrap();

    let mut cursor = Cursor::new(buf);
    let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();

    assert_eq!(header, parsed);
}

#[tokio::test]
async fn header_roundtrip_outgoing() {
    let header = BridgeHeader {
        session_token: test_token(),
        direction: Direction::Outgoing,
        service_port: 9417,
        request_id: "dial-req-abc123".to_string(),
        remote_addr: "100.64.0.3:9417".to_string(),
        remote_dns_name: "target.tailnet.ts.net".to_string(),
    };

    let mut buf = Vec::new();
    header.write_to(&mut buf).await.unwrap();

    let mut cursor = Cursor::new(buf);
    let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();

    assert_eq!(header, parsed);
}

#[tokio::test]
async fn header_empty_fields() {
    let header = BridgeHeader {
        session_token: test_token(),
        direction: Direction::Incoming,
        service_port: 9417,
        request_id: String::new(),
        remote_addr: String::new(),
        remote_dns_name: String::new(),
    };

    let mut buf = Vec::new();
    header.write_to(&mut buf).await.unwrap();

    // With all empty fields, the size should be exactly MIN_HEADER_SIZE
    assert_eq!(buf.len(), MIN_HEADER_SIZE);

    let mut cursor = Cursor::new(buf);
    let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
    assert_eq!(header, parsed);
}

#[tokio::test]
async fn header_wire_len_matches() {
    let header = BridgeHeader {
        session_token: test_token(),
        direction: Direction::Outgoing,
        service_port: 443,
        request_id: "req-uuid-here".to_string(),
        remote_addr: "100.64.0.5:41641".to_string(),
        remote_dns_name: r#"{"dnsName":"peer.ts.net","loginName":"alice@example.com"}"#.to_string(),
    };

    let mut buf = Vec::new();
    header.write_to(&mut buf).await.unwrap();
    assert_eq!(header.wire_len(), buf.len());
}

#[tokio::test]
async fn header_reject_bad_magic() {
    let mut buf = vec![0xBA, 0xAD, 0xF0, 0x0D]; // wrong magic
    buf.push(VERSION);
    buf.extend_from_slice(&[0u8; 32]); // token
    buf.push(0x01); // incoming
    buf.extend_from_slice(&443u16.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes()); // req_id_len
    buf.extend_from_slice(&0u16.to_be_bytes()); // remote_addr_len
    buf.extend_from_slice(&0u16.to_be_bytes()); // remote_dns_len

    let mut cursor = Cursor::new(buf);
    let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
    assert!(matches!(err, HeaderError::InvalidMagic(_)));
}

#[tokio::test]
async fn header_reject_bad_version() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&MAGIC.to_be_bytes());
    buf.push(0x99); // bad version
    buf.extend_from_slice(&[0u8; 32]);
    buf.push(0x01);
    buf.extend_from_slice(&443u16.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes());

    let mut cursor = Cursor::new(buf);
    let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
    assert!(matches!(err, HeaderError::UnsupportedVersion(0x99)));
}

#[tokio::test]
async fn header_reject_invalid_direction() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&MAGIC.to_be_bytes());
    buf.push(VERSION);
    buf.extend_from_slice(&[0u8; 32]);
    buf.push(0xFF); // invalid direction
    buf.extend_from_slice(&443u16.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes());

    let mut cursor = Cursor::new(buf);
    let err = BridgeHeader::read_from(&mut cursor).await.unwrap_err();
    assert!(matches!(err, HeaderError::InvalidDirection(0xFF)));
}

#[tokio::test]
async fn header_reject_incoming_with_request_id() {
    // Incoming direction + non-empty request_id is a protocol violation
    let header = BridgeHeader {
        session_token: test_token(),
        direction: Direction::Incoming,
        service_port: 443,
        request_id: "should-not-have-this".to_string(),
        remote_addr: String::new(),
        remote_dns_name: String::new(),
    };

    let mut buf = Vec::new();
    let err = header.write_to(&mut buf).await.unwrap_err();
    assert!(matches!(err, HeaderError::IncomingWithRequestId(_)));
}

// ===== Command/event JSON serialization tests =====

#[test]
fn command_start_serialization() {
    let data = StartCommandData {
        hostname: "truffle-cli-test".to_string(),
        state_dir: "/tmp/tsnet-test".to_string(),
        auth_key: Some("tskey-abc123".to_string()),
        bridge_port: 54321,
        session_token: "aa".repeat(32),
        ephemeral: Some(true),
        tags: Some(vec!["tag:truffle".to_string()]),
    };
    let cmd = SidecarCommand {
        command: command_type::START,
        data: Some(serde_json::to_value(&data).unwrap()),
    };
    let json = serde_json::to_string(&cmd).unwrap();
    assert!(json.contains("\"command\":\"tsnet:start\""));
    assert!(json.contains("\"hostname\":\"truffle-cli-test\""));
    assert!(json.contains("\"bridgePort\":54321"));
    assert!(json.contains("\"authKey\":\"tskey-abc123\""));
    assert!(json.contains("\"ephemeral\":true"));
    assert!(json.contains("\"tags\":[\"tag:truffle\"]"));
}

#[test]
fn command_stop_serialization() {
    let cmd = SidecarCommand {
        command: command_type::STOP,
        data: None,
    };
    let json = serde_json::to_string(&cmd).unwrap();
    assert!(json.contains("\"command\":\"tsnet:stop\""));
    assert!(!json.contains("\"data\""));
}

#[test]
fn command_get_peers_serialization() {
    let cmd = SidecarCommand {
        command: command_type::GET_PEERS,
        data: None,
    };
    let json = serde_json::to_string(&cmd).unwrap();
    assert!(json.contains("\"command\":\"tsnet:getPeers\""));
}

#[test]
fn command_dial_serialization() {
    let data = DialCommandData {
        request_id: "dial-uuid-456".to_string(),
        target: "peer-host.tailnet.ts.net".to_string(),
        port: 9417,
    };
    let cmd = SidecarCommand {
        command: command_type::DIAL,
        data: Some(serde_json::to_value(&data).unwrap()),
    };
    let json = serde_json::to_string(&cmd).unwrap();
    assert!(json.contains("\"command\":\"bridge:dial\""));
    assert!(json.contains("\"requestId\":\"dial-uuid-456\""));
    assert!(json.contains("\"target\":\"peer-host.tailnet.ts.net\""));
    assert!(json.contains("\"port\":9417"));
}

#[test]
fn command_watch_peers_serialization() {
    let data = WatchPeersCommandData {
        include_all: Some(true),
    };
    let cmd = SidecarCommand {
        command: command_type::WATCH_PEERS,
        data: Some(serde_json::to_value(&data).unwrap()),
    };
    let json = serde_json::to_string(&cmd).unwrap();
    assert!(json.contains("\"command\":\"tsnet:watchPeers\""));
    assert!(json.contains("\"includeAll\":true"));
}

#[test]
fn event_status_running_deserialization() {
    let json = r#"{"event":"tsnet:status","data":{"state":"running","hostname":"my-node","dnsName":"my-node.tailnet.ts.net","tailscaleIP":"100.64.0.1"}}"#;
    let event: SidecarEvent = serde_json::from_str(json).unwrap();
    assert_eq!(event.event, event_type::STATUS);
    let data: StatusEventData = serde_json::from_value(event.data).unwrap();
    assert_eq!(data.state, "running");
    assert_eq!(data.hostname, "my-node");
    assert_eq!(data.dns_name, "my-node.tailnet.ts.net");
    assert_eq!(data.tailscale_ip, "100.64.0.1");
    assert!(data.error.is_empty());
}

#[test]
fn event_status_error_deserialization() {
    let json = r#"{"event":"tsnet:status","data":{"state":"error","error":"something went wrong"}}"#;
    let event: SidecarEvent = serde_json::from_str(json).unwrap();
    let data: StatusEventData = serde_json::from_value(event.data).unwrap();
    assert_eq!(data.state, "error");
    assert_eq!(data.error, "something went wrong");
}

#[test]
fn event_peers_deserialization() {
    let json = r#"{"event":"tsnet:peers","data":{"peers":[
        {"id":"nodeA","hostname":"truffle-cli-abc","dnsName":"truffle-cli-abc.tailnet.ts.net","tailscaleIPs":["100.64.0.2"],"online":true,"os":"linux","curAddr":"192.168.1.5:41641","relay":""},
        {"id":"nodeB","hostname":"truffle-cli-def","dnsName":"truffle-cli-def.tailnet.ts.net","tailscaleIPs":["100.64.0.3","fd7a::3"],"online":false,"os":"darwin","curAddr":"","relay":"sea"}
    ]}}"#;
    let event: SidecarEvent = serde_json::from_str(json).unwrap();
    let data: PeersEventData = serde_json::from_value(event.data).unwrap();
    assert_eq!(data.peers.len(), 2);

    assert_eq!(data.peers[0].id, "nodeA");
    assert_eq!(data.peers[0].hostname, "truffle-cli-abc");
    assert!(data.peers[0].online);
    assert_eq!(data.peers[0].os, "linux");

    assert_eq!(data.peers[1].id, "nodeB");
    assert!(!data.peers[1].online);
    assert_eq!(data.peers[1].relay, "sea");
    assert_eq!(data.peers[1].tailscale_ips.len(), 2);
}

#[test]
fn event_dial_result_success_deserialization() {
    let json = r#"{"event":"bridge:dialResult","data":{"requestId":"req-123","success":true}}"#;
    let event: SidecarEvent = serde_json::from_str(json).unwrap();
    let data: DialResultEventData = serde_json::from_value(event.data).unwrap();
    assert_eq!(data.request_id, "req-123");
    assert!(data.success);
    assert!(data.error.is_empty());
}

#[test]
fn event_dial_result_failure_deserialization() {
    let json = r#"{"event":"bridge:dialResult","data":{"requestId":"req-456","success":false,"error":"dial tcp: connection refused"}}"#;
    let event: SidecarEvent = serde_json::from_str(json).unwrap();
    let data: DialResultEventData = serde_json::from_value(event.data).unwrap();
    assert_eq!(data.request_id, "req-456");
    assert!(!data.success);
    assert_eq!(data.error, "dial tcp: connection refused");
}

#[test]
fn event_ping_result_direct_deserialization() {
    let json = r#"{"event":"tsnet:pingResult","data":{"target":"100.64.0.2","latencyMs":5.3,"direct":true,"relay":"","peerAddr":"192.168.1.5:41641"}}"#;
    let event: SidecarEvent = serde_json::from_str(json).unwrap();
    let data: PingResultEventData = serde_json::from_value(event.data).unwrap();
    assert_eq!(data.target, "100.64.0.2");
    assert!((data.latency_ms - 5.3).abs() < 0.001);
    assert!(data.direct);
    assert!(data.relay.is_empty());
    assert_eq!(data.peer_addr, "192.168.1.5:41641");
}

#[test]
fn event_ping_result_relayed_deserialization() {
    let json = r#"{"event":"tsnet:pingResult","data":{"target":"100.64.0.3","latencyMs":45.7,"direct":false,"relay":"sfo","peerAddr":""}}"#;
    let event: SidecarEvent = serde_json::from_str(json).unwrap();
    let data: PingResultEventData = serde_json::from_value(event.data).unwrap();
    assert!(!data.direct);
    assert_eq!(data.relay, "sfo");
}

#[test]
fn event_peer_changed_join_deserialization() {
    let json = r#"{"event":"tsnet:peerChanged","data":{"changeType":"joined","peerId":"nodeNew","peer":{"id":"nodeNew","hostname":"truffle-cli-new","dnsName":"truffle-cli-new.tailnet.ts.net","tailscaleIPs":["100.64.0.10"],"online":true}}}"#;
    let event: SidecarEvent = serde_json::from_str(json).unwrap();
    let data: PeerChangedEventData = serde_json::from_value(event.data).unwrap();
    assert_eq!(data.change_type, "joined");
    assert_eq!(data.peer_id, "nodeNew");
    assert!(data.peer.is_some());
    assert_eq!(data.peer.unwrap().hostname, "truffle-cli-new");
}

#[test]
fn event_peer_changed_left_deserialization() {
    let json = r#"{"event":"tsnet:peerChanged","data":{"changeType":"left","peerId":"nodeGone"}}"#;
    let event: SidecarEvent = serde_json::from_str(json).unwrap();
    let data: PeerChangedEventData = serde_json::from_value(event.data).unwrap();
    assert_eq!(data.change_type, "left");
    assert_eq!(data.peer_id, "nodeGone");
    assert!(data.peer.is_none());
}

// ===== Peer filtering tests =====

#[test]
fn filter_truffle_peers_only() {
    let peers = vec![
        SidecarPeer {
            id: "node1".into(),
            hostname: "truffle-cli-abc".into(),
            dns_name: "truffle-cli-abc.tailnet.ts.net".into(),
            tailscale_ips: vec!["100.64.0.2".into()],
            online: true,
            os: "linux".into(),
            cur_addr: String::new(),
            relay: String::new(),
            last_seen: None,
            key_expiry: None,
            expired: false,
        },
        SidecarPeer {
            id: "node2".into(),
            hostname: "my-laptop".into(), // NOT a truffle peer
            dns_name: "my-laptop.tailnet.ts.net".into(),
            tailscale_ips: vec!["100.64.0.3".into()],
            online: true,
            os: "darwin".into(),
            cur_addr: String::new(),
            relay: String::new(),
            last_seen: None,
            key_expiry: None,
            expired: false,
        },
        SidecarPeer {
            id: "node3".into(),
            hostname: "truffle-daemon-xyz".into(),
            dns_name: "truffle-daemon-xyz.tailnet.ts.net".into(),
            tailscale_ips: vec!["100.64.0.4".into()],
            online: false,
            os: "linux".into(),
            cur_addr: String::new(),
            relay: String::new(),
            last_seen: None,
            key_expiry: None,
            expired: false,
        },
    ];

    let truffle_peers: Vec<_> = peers
        .iter()
        .filter(|p| super::provider::is_truffle_peer(&p.hostname))
        .collect();

    assert_eq!(truffle_peers.len(), 2);
    assert_eq!(truffle_peers[0].hostname, "truffle-cli-abc");
    assert_eq!(truffle_peers[1].hostname, "truffle-daemon-xyz");
}

#[test]
fn non_truffle_hostnames_rejected() {
    assert!(!super::provider::is_truffle_peer("my-laptop"));
    assert!(!super::provider::is_truffle_peer("desktop-abc"));
    assert!(!super::provider::is_truffle_peer(""));
    assert!(!super::provider::is_truffle_peer("truffles")); // not truffle- prefix
}

#[test]
fn truffle_hostnames_accepted() {
    assert!(super::provider::is_truffle_peer("truffle-cli-abc123"));
    assert!(super::provider::is_truffle_peer("truffle-daemon-xyz"));
    assert!(super::provider::is_truffle_peer("truffle-test"));
    assert!(super::provider::is_truffle_peer("truffle-"));
}

// ===== Phase 1 audit fix tests =====

/// Verify that `bind_udp()` returns `NetworkError::NotRunning` when provider not started.
/// The provider must be running to send commands to the sidecar.
#[tokio::test]
async fn test_bind_udp_returns_not_running() {
    use super::provider::{TailscaleConfig, TailscaleProvider};
    use crate::network::NetworkProvider;

    let config = TailscaleConfig {
        binary_path: "/nonexistent/sidecar".into(),
        hostname: "truffle-test".to_string(),
        state_dir: "/tmp/test-state".to_string(),
        auth_key: None,
        ephemeral: None,
        tags: None,
    };
    let provider = TailscaleProvider::new(config);

    let result = provider.bind_udp(1234).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, crate::network::NetworkError::NotRunning),
        "expected NetworkError::NotRunning, got: {err:?}"
    );
}

/// Verify that `local_identity()` returns a default `NodeIdentity` before `start()` is called,
/// rather than panicking. (Phase 1 audit fix: replaced RwLock::unwrap panic with safe default.)
#[test]
fn test_local_identity_default_before_start() {
    use super::provider::{TailscaleConfig, TailscaleProvider};
    use crate::network::NetworkProvider;

    let config = TailscaleConfig {
        binary_path: "/nonexistent/sidecar".into(),
        hostname: "truffle-test".to_string(),
        state_dir: "/tmp/test-state".to_string(),
        auth_key: None,
        ephemeral: None,
        tags: None,
    };
    let provider = TailscaleProvider::new(config);

    // This must not panic — it should return a default NodeIdentity
    let identity = provider.local_identity();
    assert!(identity.hostname.is_empty(), "hostname should be empty before start");
    assert!(identity.id.is_empty(), "id should be empty before start");
    assert!(identity.name.is_empty(), "name should be empty before start");
    assert!(identity.dns_name.is_none(), "dns_name should be None before start");
    assert!(identity.ip.is_none(), "ip should be None before start");
}

/// Verify that `local_addr()` returns a default `PeerAddr` before `start()` is called,
/// rather than panicking. (Phase 1 audit fix: replaced RwLock::unwrap panic with safe default.)
#[test]
fn test_local_addr_default_before_start() {
    use super::provider::{TailscaleConfig, TailscaleProvider};
    use crate::network::NetworkProvider;

    let config = TailscaleConfig {
        binary_path: "/nonexistent/sidecar".into(),
        hostname: "truffle-test".to_string(),
        state_dir: "/tmp/test-state".to_string(),
        auth_key: None,
        ephemeral: None,
        tags: None,
    };
    let provider = TailscaleProvider::new(config);

    // This must not panic — it should return a default PeerAddr
    let addr = provider.local_addr();
    assert!(addr.hostname.is_empty(), "hostname should be empty before start");
    assert!(addr.ip.is_none(), "ip should be None before start");
    assert!(addr.dns_name.is_none(), "dns_name should be None before start");
}

/// Verify that `Bridge::local_port()` returns `Result<u16>` (not a panic) and gives a valid port.
/// (Phase 1 audit fix: changed from `u16` to `Result<u16, NetworkError>`.)
#[tokio::test]
async fn test_bridge_local_port_returns_result() {
    use super::bridge::Bridge;

    let token = [0xABu8; 32];
    let bridge = Bridge::bind(token).await.expect("bridge should bind");

    // local_port() now returns Result — verify it succeeds and gives a real port
    let port = bridge.local_port().expect("local_port should return Ok");
    assert!(port > 0, "port should be non-zero (ephemeral port)");
}

/// Verify that BridgeHeader roundtrips correctly when request_id, remote_addr, and
/// remote_dns_name are all empty with Outgoing direction.
/// (Edge case: outgoing with empty remote fields — not the same as the existing
/// empty-fields test which uses Incoming direction.)
#[tokio::test]
async fn test_header_write_read_roundtrip_with_empty_fields() {
    let header = BridgeHeader {
        session_token: test_token(),
        direction: Direction::Outgoing,
        service_port: 8080,
        request_id: String::new(),
        remote_addr: String::new(),
        remote_dns_name: String::new(),
    };

    let mut buf = Vec::new();
    header.write_to(&mut buf).await.unwrap();

    // All variable-length fields are empty, so size = MIN_HEADER_SIZE
    assert_eq!(buf.len(), MIN_HEADER_SIZE);

    let mut cursor = Cursor::new(buf);
    let parsed = BridgeHeader::read_from(&mut cursor).await.unwrap();
    assert_eq!(header, parsed);

    // Verify each field individually to catch any corruption
    assert_eq!(parsed.direction, Direction::Outgoing);
    assert_eq!(parsed.service_port, 8080);
    assert!(parsed.request_id.is_empty());
    assert!(parsed.remote_addr.is_empty());
    assert!(parsed.remote_dns_name.is_empty());
}

/// Verify that all `NetworkError` variants produce meaningful `Display` output.
/// (Phase 1 audit: ensure thiserror derives produce non-empty messages.)
#[test]
fn test_network_error_display() {
    use crate::network::NetworkError;
    use std::time::Duration;

    let cases: Vec<(NetworkError, &str)> = vec![
        (NetworkError::NotRunning, "not running"),
        (NetworkError::AlreadyRunning, "already running"),
        (NetworkError::StartFailed("oops".into()), "oops"),
        (NetworkError::StopFailed("halt".into()), "halt"),
        (
            NetworkError::AuthRequired {
                url: "https://login.tailscale.com/a/xyz".into(),
            },
            "https://login.tailscale.com/a/xyz",
        ),
        (NetworkError::DialFailed("refused".into()), "refused"),
        (NetworkError::DialTimeout(Duration::from_secs(30)), "30"),
        (NetworkError::ListenFailed("bind".into()), "bind"),
        (NetworkError::PingFailed("timeout".into()), "timeout"),
        (NetworkError::SidecarError("crash".into()), "crash"),
        (NetworkError::BridgeError("bad header".into()), "bad header"),
        (NetworkError::Internal("bug".into()), "bug"),
    ];

    for (error, expected_substring) in &cases {
        let display = format!("{error}");
        assert!(
            !display.is_empty(),
            "Display for {error:?} should not be empty"
        );
        assert!(
            display.contains(expected_substring),
            "Display for {error:?} should contain '{expected_substring}', got: '{display}'"
        );
    }

    // Also test the Io and Serialize variants produce non-empty output
    let io_err = NetworkError::Io(std::io::Error::new(
        std::io::ErrorKind::ConnectionRefused,
        "refused",
    ));
    let io_display = format!("{io_err}");
    assert!(!io_display.is_empty());
    assert!(io_display.contains("refused"), "Io display: {io_display}");

    let json_err: Result<serde_json::Value, _> = serde_json::from_str("{bad json");
    let ser_err = NetworkError::Serialize(json_err.unwrap_err());
    let ser_display = format!("{ser_err}");
    assert!(!ser_display.is_empty());
}

// ===== NetworkUdpSocket framing tests =====

/// Verify that NetworkUdpSocket send_to/recv_from correctly frames and unframes
/// address headers through a loopback UDP relay.
#[tokio::test]
async fn test_network_udp_socket_framing_roundtrip() {
    use crate::network::NetworkUdpSocket;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    // Create a "relay" UDP socket that simulates the Go sidecar relay
    let relay = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay.local_addr().unwrap();

    // Create the NetworkUdpSocket connected to the relay
    let rust_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let rust_addr = rust_socket.local_addr().unwrap();
    rust_socket
        .connect(relay_addr)
        .await
        .unwrap();

    // Also connect the relay to the rust socket so send/recv work
    relay.connect(rust_addr).await.unwrap();

    let net_socket = NetworkUdpSocket::new(rust_socket, 19420);

    // Test send_to: Rust -> relay (verify framing)
    let target_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 5)), 9999);
    let payload = b"hello from rust";
    net_socket.send_to(payload, target_addr).await.unwrap();

    // Relay receives the framed packet
    let mut buf = [0u8; 1024];
    let n = relay.recv(&mut buf).await.unwrap();
    assert_eq!(n, 6 + payload.len()); // 6-byte header + payload

    // Verify the header
    assert_eq!(&buf[0..4], &[100, 64, 0, 5]); // IPv4 octets
    assert_eq!(&buf[4..6], &9999u16.to_be_bytes()); // port BE
    assert_eq!(&buf[6..n], payload);

    // Test recv_from: relay -> Rust (verify unframing)
    let sender_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7)), 8888);
    let reply = b"reply from peer";
    let mut framed = Vec::new();
    framed.extend_from_slice(&Ipv4Addr::new(100, 64, 0, 7).octets());
    framed.extend_from_slice(&8888u16.to_be_bytes());
    framed.extend_from_slice(reply);
    relay.send(&framed).await.unwrap();

    let mut recv_buf = [0u8; 1024];
    let (n, from_addr) = net_socket.recv_from(&mut recv_buf).await.unwrap();
    assert_eq!(n, reply.len());
    assert_eq!(&recv_buf[..n], reply);
    assert_eq!(from_addr, sender_addr);
}

/// Verify that NetworkUdpSocket rejects IPv6 addresses.
#[tokio::test]
async fn test_network_udp_socket_rejects_ipv6() {
    use crate::network::NetworkUdpSocket;
    use std::net::{IpAddr, Ipv6Addr, SocketAddr};

    let relay = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay.local_addr().unwrap();

    let rust_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    rust_socket.connect(relay_addr).await.unwrap();

    let net_socket = NetworkUdpSocket::new(rust_socket, 19420);

    let ipv6_addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 1234);
    let result = net_socket.send_to(b"test", ipv6_addr).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("IPv6"), "expected IPv6 error, got: {msg}");
}

/// Verify that recv_from rejects packets that are too short.
#[tokio::test]
async fn test_network_udp_socket_short_packet() {
    use crate::network::NetworkUdpSocket;

    let relay = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay.local_addr().unwrap();

    let rust_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let rust_addr = rust_socket.local_addr().unwrap();
    rust_socket.connect(relay_addr).await.unwrap();
    relay.connect(rust_addr).await.unwrap();

    let net_socket = NetworkUdpSocket::new(rust_socket, 19420);

    // Send a packet that's too short for the address header
    relay.send(&[1, 2, 3]).await.unwrap();

    let mut buf = [0u8; 1024];
    let result = net_socket.recv_from(&mut buf).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("too short"), "expected 'too short' error, got: {msg}");
}

/// Verify that the sidecar ListeningPacket event is correctly mapped.
#[test]
fn test_sidecar_listening_packet_event_deserialization() {
    use super::protocol::{SidecarEvent, ListeningPacketEventData, event_type};

    let json = r#"{"event":"tsnet:listeningPacket","data":{"port":19420,"localPort":54321}}"#;
    let event: SidecarEvent = serde_json::from_str(json).unwrap();
    assert_eq!(event.event, event_type::LISTENING_PACKET);
    let data: ListeningPacketEventData = serde_json::from_value(event.data).unwrap();
    assert_eq!(data.port, 19420);
    assert_eq!(data.local_port, 54321);
}

/// Verify that the sidecar ListenPacket command serializes correctly.
#[test]
fn test_sidecar_listen_packet_command_serialization() {
    use super::protocol::{SidecarCommand, ListenPacketCommandData, command_type};

    let data = ListenPacketCommandData { port: 19420 };
    let cmd = SidecarCommand {
        command: command_type::LISTEN_PACKET,
        data: Some(serde_json::to_value(&data).unwrap()),
    };
    let json = serde_json::to_string(&cmd).unwrap();
    assert!(json.contains("\"command\":\"tsnet:listenPacket\""));
    assert!(json.contains("\"port\":19420"));
}

// ===== Integration tests (require real Tailscale network) =====

/// Integration test: Start provider, authenticate, verify running state.
///
/// Requires:
/// - Go sidecar binary built at the expected path
/// - Valid Tailscale auth key or interactive auth
/// - Active Tailscale network
///
/// Run with: `cargo test -p truffle-core -- --ignored integration_start`
#[tokio::test]
#[ignore = "requires real Tailscale network and Go sidecar binary"]
async fn integration_start_and_discover_peers() {
    // This test would:
    // 1. Create a TailscaleConfig pointing to the real sidecar binary
    // 2. Start the provider
    // 3. Verify it reaches Running state
    // 4. Call peers() and verify the list is populated
    // 5. Stop the provider
    todo!("integration test requires real Tailscale network")
}

/// Integration test: dial_tcp between two nodes.
///
/// Requires two truffle nodes on the same tailnet.
///
/// Run with: `cargo test -p truffle-core -- --ignored integration_dial`
#[tokio::test]
#[ignore = "requires two nodes on the same Tailscale network"]
async fn integration_dial_tcp() {
    // This test would:
    // 1. Start provider on node A
    // 2. Start provider on node B
    // 3. Node A: listen_tcp(9418)
    // 4. Node B: dial_tcp(node_a_dns, 9418)
    // 5. Verify bidirectional data exchange
    todo!("integration test requires two nodes")
}

/// Integration test: ping between nodes.
///
/// Run with: `cargo test -p truffle-core -- --ignored integration_ping`
#[tokio::test]
#[ignore = "requires real Tailscale network"]
async fn integration_ping() {
    // This test would:
    // 1. Start provider
    // 2. Get peer list
    // 3. Ping a known peer
    // 4. Verify PingResult has reasonable latency
    todo!("integration test requires real Tailscale network")
}
