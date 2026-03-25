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

// ===== Integration tests (require real Tailscale network) =====

/// Integration test: Start provider, authenticate, verify running state.
///
/// Requires:
/// - Go sidecar binary built at the expected path
/// - Valid Tailscale auth key or interactive auth
/// - Active Tailscale network
///
/// Run with: `cargo test -p truffle-core-v2 -- --ignored integration_start`
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
/// Run with: `cargo test -p truffle-core-v2 -- --ignored integration_dial`
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
/// Run with: `cargo test -p truffle-core-v2 -- --ignored integration_ping`
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
