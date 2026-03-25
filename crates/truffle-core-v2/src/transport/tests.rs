//! Unit tests for Layer 4: Transport.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};

use crate::network::{
    HealthInfo, IncomingConnection, NetworkError, NetworkPeer, NetworkPeerEvent,
    NetworkTcpListener, NodeIdentity, PeerAddr, PingResult,
};
use crate::network::NetworkProvider;
use crate::transport::{
    DatagramTransport, FramedStream, Handshake, RawTransport, StreamTransport, TransportError,
    WsConfig, PROTOCOL_VERSION,
};

// ---------------------------------------------------------------------------
// Mock NetworkProvider for unit tests
// ---------------------------------------------------------------------------

/// A mock network provider that uses local TCP for testing.
///
/// `dial_tcp` connects to `127.0.0.1:{port}` directly.
/// `listen_tcp` binds a local TCP listener and forwards connections.
struct MockNetworkProvider {
    identity: NodeIdentity,
    local_addr: PeerAddr,
    peer_event_tx: broadcast::Sender<NetworkPeerEvent>,
}

impl MockNetworkProvider {
    fn new(id: &str) -> Self {
        let (peer_event_tx, _) = broadcast::channel(16);
        Self {
            identity: NodeIdentity {
                id: id.to_string(),
                hostname: format!("truffle-test-{id}"),
                name: format!("Test Node {id}"),
                dns_name: None,
                ip: Some("127.0.0.1".parse().unwrap()),
            },
            local_addr: PeerAddr {
                ip: Some("127.0.0.1".parse().unwrap()),
                hostname: format!("truffle-test-{id}"),
                dns_name: None,
            },
            peer_event_tx,
        }
    }
}

impl NetworkProvider for MockNetworkProvider {
    async fn start(&mut self) -> Result<(), NetworkError> {
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), NetworkError> {
        Ok(())
    }

    fn local_identity(&self) -> NodeIdentity {
        self.identity.clone()
    }

    fn local_addr(&self) -> PeerAddr {
        self.local_addr.clone()
    }

    fn peer_events(&self) -> broadcast::Receiver<NetworkPeerEvent> {
        self.peer_event_tx.subscribe()
    }

    async fn peers(&self) -> Vec<NetworkPeer> {
        vec![]
    }

    async fn dial_tcp(&self, addr: &str, port: u16) -> Result<TcpStream, NetworkError> {
        let target = format!("{addr}:{port}");
        TcpStream::connect(&target)
            .await
            .map_err(|e| NetworkError::DialFailed(format!("mock dial {target}: {e}")))
    }

    async fn listen_tcp(&self, port: u16) -> Result<NetworkTcpListener, NetworkError> {
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .map_err(|e| NetworkError::ListenFailed(format!("mock listen :{port}: {e}")))?;

        let actual_port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<IncomingConnection>(64);

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        let conn = IncomingConnection {
                            stream,
                            remote_addr: addr.to_string(),
                            remote_identity: String::new(),
                            port: actual_port,
                        };
                        if tx.send(conn).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("mock listener error: {e}");
                        break;
                    }
                }
            }
        });

        Ok(NetworkTcpListener {
            port: actual_port,
            incoming: rx,
        })
    }

    async fn unlisten_tcp(&self, _port: u16) -> Result<(), NetworkError> {
        Ok(())
    }

    async fn bind_udp(&self, _port: u16) -> Result<tokio::net::UdpSocket, NetworkError> {
        Err(NetworkError::Internal("mock: UDP not supported".into()))
    }

    async fn ping(&self, _addr: &str) -> Result<PingResult, NetworkError> {
        Ok(PingResult {
            latency: Duration::from_millis(1),
            connection: "direct".to_string(),
            peer_addr: None,
        })
    }

    async fn health(&self) -> HealthInfo {
        HealthInfo {
            state: "running".to_string(),
            healthy: true,
            ..Default::default()
        }
    }
}

// ===========================================================================
// Handshake serialization tests
// ===========================================================================

#[test]
fn handshake_serialize_roundtrip() {
    let hs = Handshake {
        peer_id: "node-abc123".to_string(),
        capabilities: vec!["ws".to_string(), "binary".to_string()],
        protocol_version: PROTOCOL_VERSION,
    };

    let json = serde_json::to_string(&hs).unwrap();
    let parsed: Handshake = serde_json::from_str(&json).unwrap();

    assert_eq!(hs, parsed);
}

#[test]
fn handshake_json_structure() {
    let hs = Handshake {
        peer_id: "test-peer".to_string(),
        capabilities: vec!["ws".to_string()],
        protocol_version: 1,
    };

    let json = serde_json::to_string(&hs).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(value["peer_id"], "test-peer");
    assert_eq!(value["protocol_version"], 1);
    assert!(value["capabilities"].is_array());
    assert_eq!(value["capabilities"][0], "ws");
}

#[test]
fn handshake_deserialize_unknown_fields_ignored() {
    let json = r#"{
        "peer_id": "node-x",
        "capabilities": ["ws"],
        "protocol_version": 1,
        "extra_field": "should be ignored"
    }"#;

    // serde default behavior is to ignore unknown fields
    let hs: Handshake = serde_json::from_str(json).unwrap();
    assert_eq!(hs.peer_id, "node-x");
    assert_eq!(hs.protocol_version, 1);
}

#[test]
fn handshake_empty_capabilities() {
    let hs = Handshake {
        peer_id: "minimal".to_string(),
        capabilities: vec![],
        protocol_version: 1,
    };

    let json = serde_json::to_string(&hs).unwrap();
    let parsed: Handshake = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.capabilities.len(), 0);
}

// ===========================================================================
// WsConfig defaults
// ===========================================================================

#[test]
fn ws_config_defaults() {
    let config = WsConfig::default();
    assert_eq!(config.port, 9417);
    assert_eq!(config.ping_interval, Duration::from_secs(10));
    assert_eq!(config.pong_timeout, Duration::from_secs(30));
    assert_eq!(config.max_message_size, 16 * 1024 * 1024);
}

// ===========================================================================
// TransportError formatting
// ===========================================================================

#[test]
fn transport_error_display() {
    let err = TransportError::NotImplemented("QUIC".to_string());
    assert_eq!(err.to_string(), "not implemented: QUIC");

    let err = TransportError::VersionMismatch {
        local: 1,
        remote: 2,
    };
    assert_eq!(
        err.to_string(),
        "protocol version mismatch: local=1, remote=2"
    );

    let err = TransportError::HeartbeatTimeout(Duration::from_secs(30));
    assert_eq!(err.to_string(), "heartbeat timeout after 30s");
}

// ===========================================================================
// WebSocket transport integration tests (loopback)
// ===========================================================================

#[tokio::test]
async fn ws_connect_and_exchange_messages() {
    use crate::transport::websocket::WebSocketTransport;

    // Create two mock providers (server and client)
    let server_provider = Arc::new(MockNetworkProvider::new("server"));
    let client_provider = Arc::new(MockNetworkProvider::new("client"));

    // Pick a random high port
    let port = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };

    let config = WsConfig {
        port,
        ping_interval: Duration::from_secs(60), // long interval for test
        pong_timeout: Duration::from_secs(60),
        ..Default::default()
    };

    let server_ws = WebSocketTransport::new(server_provider, config.clone());
    let mut listener = server_ws.listen().await.unwrap();
    assert_eq!(listener.port, port);

    // Client: connect
    let client_ws = WebSocketTransport::new(client_provider, config);

    let peer_addr = PeerAddr {
        ip: Some("127.0.0.1".parse().unwrap()),
        hostname: "localhost".to_string(),
        dns_name: None,
    };

    // Connect client and accept on server concurrently
    let (client_result, server_stream) = tokio::join!(
        client_ws.connect(&peer_addr),
        async { listener.accept().await }
    );

    let mut client_stream = client_result.expect("client connect should succeed");
    let mut server_stream = server_stream.expect("server should accept a connection");

    // Verify peer addresses
    assert!(!client_stream.peer_addr().is_empty());
    assert!(!server_stream.peer_addr().is_empty());

    // Verify remote peer IDs from handshake
    assert_eq!(client_stream.remote_peer_id(), "server");
    assert_eq!(server_stream.remote_peer_id(), "client");

    // Client sends, server receives
    let msg = b"hello from client";
    client_stream.send(msg).await.unwrap();

    let received = server_stream.recv().await.unwrap().expect("should receive message");
    assert_eq!(received, msg);

    // Server sends, client receives
    let reply = b"hello from server";
    server_stream.send(reply).await.unwrap();

    let received = client_stream.recv().await.unwrap().expect("should receive reply");
    assert_eq!(received, reply);

    // Clean close
    client_stream.close().await.unwrap();
    server_stream.close().await.unwrap();
}

#[tokio::test]
async fn ws_handshake_version_mismatch() {
    // This tests the handshake parsing logic directly since we can't easily
    // inject a version mismatch through the full transport flow without
    // another WS implementation.
    let hs_v1 = Handshake {
        peer_id: "node-a".to_string(),
        capabilities: vec!["ws".to_string()],
        protocol_version: 1,
    };
    let hs_v99 = Handshake {
        peer_id: "node-b".to_string(),
        capabilities: vec!["ws".to_string()],
        protocol_version: 99,
    };

    // Serialization works for both
    let json_v1 = serde_json::to_string(&hs_v1).unwrap();
    let json_v99 = serde_json::to_string(&hs_v99).unwrap();

    let parsed_v1: Handshake = serde_json::from_str(&json_v1).unwrap();
    let parsed_v99: Handshake = serde_json::from_str(&json_v99).unwrap();

    // Version comparison logic
    assert_eq!(parsed_v1.protocol_version, PROTOCOL_VERSION);
    assert_ne!(parsed_v99.protocol_version, PROTOCOL_VERSION);
}

#[tokio::test]
async fn ws_binary_frame_roundtrip() {
    use crate::transport::websocket::WebSocketTransport;

    let server_provider = Arc::new(MockNetworkProvider::new("server"));
    let client_provider = Arc::new(MockNetworkProvider::new("client"));

    let port = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };

    let config = WsConfig {
        port,
        ping_interval: Duration::from_secs(60),
        pong_timeout: Duration::from_secs(60),
        ..Default::default()
    };

    let server_ws = WebSocketTransport::new(server_provider, config.clone());
    let mut listener = server_ws.listen().await.unwrap();

    let client_ws = WebSocketTransport::new(client_provider, config);
    let peer_addr = PeerAddr {
        ip: Some("127.0.0.1".parse().unwrap()),
        hostname: "localhost".to_string(),
        dns_name: None,
    };

    let (client_result, server_stream) = tokio::join!(
        client_ws.connect(&peer_addr),
        async { listener.accept().await }
    );

    let mut client_stream = client_result.unwrap();
    let mut server_stream = server_stream.unwrap();

    // Test various binary payloads
    let test_cases: Vec<Vec<u8>> = vec![
        vec![],                                  // empty
        vec![0x00],                              // single null byte
        vec![0xFF; 1024],                        // 1KB of 0xFF
        (0..=255).map(|b| b as u8).collect(),    // all byte values
        b"utf8 text as binary".to_vec(),
    ];

    for payload in &test_cases {
        client_stream.send(payload).await.unwrap();
        let received = server_stream.recv().await.unwrap().expect("should receive");
        assert_eq!(
            &received, payload,
            "binary roundtrip failed for payload of len {}",
            payload.len()
        );
    }

    client_stream.close().await.unwrap();
    server_stream.close().await.unwrap();
}

// ===========================================================================
// TCP transport tests
// ===========================================================================

#[tokio::test]
async fn tcp_open_and_transfer() {
    use crate::transport::tcp::TcpTransport;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let provider = Arc::new(MockNetworkProvider::new("tcp-test"));

    // Start a raw TCP server
    let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp_listener.local_addr().unwrap().port();

    let server_handle = tokio::spawn(async move {
        let (mut stream, _addr) = tcp_listener.accept().await.unwrap();
        let mut buf = [0u8; 64];
        let n = stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello tcp");
        stream.write_all(b"goodbye tcp").await.unwrap();
    });

    // Client: open via TcpTransport
    let tcp = TcpTransport::new(provider);
    let peer_addr = PeerAddr {
        ip: Some("127.0.0.1".parse().unwrap()),
        hostname: "localhost".to_string(),
        dns_name: None,
    };

    let mut stream = tcp.open(&peer_addr, port).await.unwrap();

    stream.write_all(b"hello tcp").await.unwrap();

    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"goodbye tcp");

    server_handle.await.unwrap();
}

#[tokio::test]
async fn tcp_listen_and_accept() {
    use crate::transport::tcp::TcpTransport;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let provider = Arc::new(MockNetworkProvider::new("tcp-listen"));
    let tcp = TcpTransport::new(provider);

    // Listen on ephemeral port
    let mut listener = tcp.listen(0).await.unwrap();
    let port = listener.port;

    // Client connects directly
    let client_handle = tokio::spawn(async move {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        stream.write_all(b"from client").await.unwrap();
        stream.shutdown().await.unwrap();
    });

    // Accept on the RawListener
    let incoming = listener.accept().await.expect("should accept connection");
    let mut stream = incoming.stream;

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    assert_eq!(buf, b"from client");

    client_handle.await.unwrap();
}

#[tokio::test]
async fn tcp_transport_wraps_network_provider() {
    use crate::transport::tcp::TcpTransport;

    let provider = Arc::new(MockNetworkProvider::new("wrapper-test"));
    let tcp = TcpTransport::new(provider);

    // Attempting to connect to a non-existent address should fail with
    // ConnectFailed, proving the transport delegates to NetworkProvider.
    let addr = PeerAddr {
        ip: Some("127.0.0.1".parse().unwrap()),
        hostname: "localhost".to_string(),
        dns_name: None,
    };

    let result = tcp.open(&addr, 1).await; // port 1 should be refused
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        TransportError::ConnectFailed(msg) => {
            assert!(
                msg.contains("tcp dial"),
                "error should mention tcp dial: {msg}"
            );
        }
        other => panic!("expected ConnectFailed, got: {other}"),
    }
}

// ===========================================================================
// QUIC stub tests
// ===========================================================================

#[tokio::test]
async fn quic_stream_returns_not_implemented() {
    use crate::transport::quic::QuicTransport;

    let provider = Arc::new(MockNetworkProvider::new("quic-test"));
    let quic = QuicTransport::new(provider);

    let addr = PeerAddr {
        ip: Some("127.0.0.1".parse().unwrap()),
        hostname: "localhost".to_string(),
        dns_name: None,
    };

    // StreamTransport
    let result = quic.connect(&addr).await;
    assert!(matches!(result, Err(TransportError::NotImplemented(_))));

    let result = StreamTransport::listen(&quic).await;
    assert!(matches!(result, Err(TransportError::NotImplemented(_))));

    // RawTransport
    let result = RawTransport::open(&quic, &addr, 8080).await;
    assert!(matches!(result, Err(TransportError::NotImplemented(_))));

    let result = RawTransport::listen(&quic, 8080).await;
    assert!(matches!(result, Err(TransportError::NotImplemented(_))));
}

// ===========================================================================
// UDP stub tests
// ===========================================================================

#[tokio::test]
async fn udp_returns_not_implemented() {
    use crate::transport::udp::UdpTransport;

    let provider = Arc::new(MockNetworkProvider::new("udp-test"));
    let udp = UdpTransport::new(provider);

    let result = udp.bind(9000).await;
    assert!(matches!(result, Err(TransportError::NotImplemented(_))));
}

// ===========================================================================
// Heartbeat logic tests
// ===========================================================================

#[tokio::test]
async fn ws_heartbeat_keeps_connection_alive() {
    use crate::transport::websocket::WebSocketTransport;

    let server_provider = Arc::new(MockNetworkProvider::new("hb-server"));
    let client_provider = Arc::new(MockNetworkProvider::new("hb-client"));

    let port = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };

    // Short heartbeat interval for testing
    let config = WsConfig {
        port,
        ping_interval: Duration::from_millis(100),
        pong_timeout: Duration::from_secs(5),
        ..Default::default()
    };

    let server_ws = WebSocketTransport::new(server_provider, config.clone());
    let mut listener = server_ws.listen().await.unwrap();

    let client_ws = WebSocketTransport::new(client_provider, config);
    let peer_addr = PeerAddr {
        ip: Some("127.0.0.1".parse().unwrap()),
        hostname: "localhost".to_string(),
        dns_name: None,
    };

    let (client_result, server_stream) = tokio::join!(
        client_ws.connect(&peer_addr),
        async { listener.accept().await }
    );

    let mut client_stream = client_result.unwrap();
    let mut server_stream = server_stream.unwrap();

    // Wait long enough for several heartbeats to fire
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Connection should still be alive — send/receive should work
    client_stream.send(b"still alive").await.unwrap();
    let received = server_stream
        .recv()
        .await
        .unwrap()
        .expect("should receive after heartbeats");
    assert_eq!(received, b"still alive");

    client_stream.close().await.unwrap();
    server_stream.close().await.unwrap();
}

// ===========================================================================
// StreamListener and RawListener tests
// ===========================================================================

#[tokio::test]
async fn stream_listener_returns_none_on_channel_close() {
    use crate::transport::StreamListener;
    use crate::transport::websocket::WsFramedStream;

    let (tx, rx) = tokio::sync::mpsc::channel::<WsFramedStream>(1);
    let mut listener = StreamListener::new(rx, 9999);

    // Drop the sender
    drop(tx);

    // accept should return None
    assert!(listener.accept().await.is_none());
}

#[tokio::test]
async fn raw_listener_returns_none_on_channel_close() {
    use crate::transport::{RawIncoming, RawListener};

    let (tx, rx) = tokio::sync::mpsc::channel::<RawIncoming>(1);
    let mut listener = RawListener::new(rx, 9999);

    drop(tx);
    assert!(listener.accept().await.is_none());
}
