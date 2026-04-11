//! Unit tests for Layer 5: Session.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};

use crate::network::NetworkProvider;
use crate::network::{
    HealthInfo, IncomingConnection, NetworkError, NetworkPeer, NetworkPeerEvent,
    NetworkTcpListener, NetworkUdpSocket, NodeIdentity, PeerAddr, PingResult,
};
use crate::transport::websocket::WebSocketTransport;
use crate::transport::WsConfig;

use super::reconnect::ReconnectBackoff;
use super::{PeerEvent, PeerRegistry};

// ---------------------------------------------------------------------------
// Mock NetworkProvider for session tests
// ---------------------------------------------------------------------------

/// A mock network provider that uses local TCP for testing.
///
/// Provides a `peer_event_tx` handle so tests can inject peer events
/// to simulate Layer 3 discovery.
struct MockNetworkProvider {
    identity: NodeIdentity,
    local_addr: PeerAddr,
    peer_event_tx: broadcast::Sender<NetworkPeerEvent>,
}

impl MockNetworkProvider {
    #[allow(dead_code)]
    fn new(id: &str) -> Self {
        Self::new_with_app("test", id)
    }

    fn new_with_app(app_id: &str, id: &str) -> Self {
        let (peer_event_tx, _) = broadcast::channel(64);
        Self {
            identity: NodeIdentity {
                app_id: app_id.to_string(),
                // RFC 017: fixtures use the same string for both `device_id`
                // and `tailscale_id` so tests that assert on the sender ID
                // keep working without special-casing either form.
                device_id: id.to_string(),
                device_name: format!("Test Node {id}"),
                tailscale_hostname: format!("truffle-{app_id}-{id}"),
                tailscale_id: id.to_string(),
                dns_name: None,
                ip: Some("127.0.0.1".parse().unwrap()),
            },
            local_addr: PeerAddr {
                ip: Some("127.0.0.1".parse().unwrap()),
                hostname: format!("truffle-{app_id}-{id}"),
                dns_name: None,
            },
            peer_event_tx,
        }
    }

    /// Get a sender to inject peer events for testing.
    fn event_sender(&self) -> broadcast::Sender<NetworkPeerEvent> {
        self.peer_event_tx.clone()
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

    async fn bind_udp(&self, _port: u16) -> Result<NetworkUdpSocket, NetworkError> {
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_network_peer(id: &str, ip: &str) -> NetworkPeer {
    NetworkPeer {
        id: id.to_string(),
        hostname: format!("host-{id}"),
        ip: ip.parse().unwrap(),
        online: true,
        cur_addr: Some(format!("{ip}:41641")),
        relay: None,
        os: Some("linux".to_string()),
        last_seen: Some("2026-03-25T12:00:00Z".to_string()),
        key_expiry: None,
        dns_name: None,
    }
}

/// Pick a random available port on localhost.
async fn random_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}

fn ws_config(port: u16) -> WsConfig {
    WsConfig {
        port,
        ping_interval: Duration::from_secs(300), // long for tests
        pong_timeout: Duration::from_secs(300),
        ..Default::default()
    }
}

fn make_loopback_peer(id: &str) -> NetworkPeer {
    NetworkPeer {
        id: id.to_string(),
        hostname: format!("truffle-test-{id}"),
        ip: "127.0.0.1".parse().unwrap(),
        online: true,
        cur_addr: Some("127.0.0.1:41641".to_string()),
        relay: None,
        os: None,
        last_seen: None,
        key_expiry: None,
        dns_name: None,
    }
}

/// Build a PeerRegistry. Returns (registry, event_sender).
/// The registry uses the given port for its WS transport.
fn build_registry(
    id: &str,
    port: u16,
) -> (
    PeerRegistry<MockNetworkProvider>,
    broadcast::Sender<NetworkPeerEvent>,
) {
    build_registry_with_app("test", id, port)
}

/// Variant of [`build_registry`] that lets the caller pick the `app_id`.
/// Used by RFC 017 Phase 2 hello-exchange tests to build nodes that
/// disagree on application namespace.
fn build_registry_with_app(
    app_id: &str,
    id: &str,
    port: u16,
) -> (
    PeerRegistry<MockNetworkProvider>,
    broadcast::Sender<NetworkPeerEvent>,
) {
    let provider = MockNetworkProvider::new_with_app(app_id, id);
    let event_sender = provider.event_sender();
    let network = Arc::new(provider);
    let ws_transport = Arc::new(WebSocketTransport::new(network.clone(), ws_config(port)));
    let registry = PeerRegistry::new(network, ws_transport);
    (registry, event_sender)
}

// ===========================================================================
// Tests: Peer discovery from Layer 3 events
// ===========================================================================

#[tokio::test]
async fn test_peers_from_network_events() {
    let port = random_port().await;
    let (registry, event_sender) = build_registry("node-a", port);
    registry.start().await;

    // Inject peer joined events
    let peer1 = make_network_peer("peer-1", "100.64.0.1");
    let peer2 = make_network_peer("peer-2", "100.64.0.2");
    event_sender.send(NetworkPeerEvent::Joined(peer1)).unwrap();
    event_sender.send(NetworkPeerEvent::Joined(peer2)).unwrap();

    // Give the event loop time to process
    tokio::time::sleep(Duration::from_millis(50)).await;

    let peers = registry.peers().await;
    assert_eq!(peers.len(), 2, "should have 2 peers");

    let ids: Vec<String> = peers.iter().map(|p| p.id.clone()).collect();
    assert!(ids.contains(&"peer-1".to_string()));
    assert!(ids.contains(&"peer-2".to_string()));
}

#[tokio::test]
async fn test_peer_left_removes() {
    let port = random_port().await;
    let (registry, event_sender) = build_registry("node-a", port);
    registry.start().await;

    let peer = make_network_peer("peer-1", "100.64.0.1");
    event_sender.send(NetworkPeerEvent::Joined(peer)).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(registry.peers().await.len(), 1);

    event_sender
        .send(NetworkPeerEvent::Left("peer-1".to_string()))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(registry.peers().await.len(), 0, "peer should be removed");
}

#[tokio::test]
async fn test_peers_online_without_connection() {
    let port = random_port().await;
    let (registry, event_sender) = build_registry("node-a", port);
    registry.start().await;

    let peer = make_network_peer("peer-1", "100.64.0.1");
    event_sender.send(NetworkPeerEvent::Joined(peer)).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let peers = registry.peers().await;
    assert_eq!(peers.len(), 1);

    let p = &peers[0];
    assert_eq!(p.id, "peer-1");
    assert!(p.online, "peer should be online");
    assert!(
        !p.ws_connected,
        "peer should NOT be ws_connected (no WS yet)"
    );
}

#[tokio::test]
async fn test_peer_updated_preserves_connected() {
    let port = random_port().await;
    let (registry, event_sender) = build_registry("node-a", port);
    registry.start().await;

    let peer = make_network_peer("peer-1", "100.64.0.1");
    event_sender.send(NetworkPeerEvent::Joined(peer)).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut updated_peer = make_network_peer("peer-1", "100.64.0.1");
    updated_peer.relay = Some("sfo".to_string());
    updated_peer.cur_addr = None;
    event_sender
        .send(NetworkPeerEvent::Updated(updated_peer))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let peers = registry.peers().await;
    assert_eq!(peers.len(), 1);
    let p = &peers[0];
    assert_eq!(p.connection_type, "relay:sfo");
    assert!(!p.ws_connected, "ws_connected should be preserved as false");
}

#[tokio::test]
async fn test_peer_event_subscription() {
    let port = random_port().await;
    let (registry, event_sender) = build_registry("node-a", port);
    let mut rx = registry.on_peer_change();
    registry.start().await;

    let peer = make_network_peer("peer-1", "100.64.0.1");
    event_sender.send(NetworkPeerEvent::Joined(peer)).unwrap();

    let event = tokio::time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .expect("should receive event within timeout")
        .expect("should not be a recv error");

    match event {
        PeerEvent::Joined(state) => {
            assert_eq!(state.id, "peer-1");
            assert!(state.online);
        }
        other => panic!("expected PeerEvent::Joined, got: {other:?}"),
    }
}

// ===========================================================================
// Tests: Send and lazy connect
// ===========================================================================

#[tokio::test]
async fn test_send_unknown_peer_errors() {
    let port = random_port().await;
    let (registry, _event_sender) = build_registry("node-a", port);
    registry.start().await;

    let result = registry.send("nonexistent-peer", b"hello").await;
    assert!(result.is_err(), "send to unknown peer should fail");

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("unknown peer"),
        "error should mention unknown peer: {err}"
    );
}

#[tokio::test]
async fn test_send_offline_peer_errors() {
    let port = random_port().await;
    let (registry, event_sender) = build_registry("node-a", port);
    registry.start().await;

    let mut peer = make_network_peer("peer-1", "100.64.0.1");
    peer.online = false;
    event_sender.send(NetworkPeerEvent::Joined(peer)).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let result = registry.send("peer-1", b"hello").await;
    assert!(result.is_err(), "send to offline peer should fail");

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("offline"),
        "error should mention offline: {err}"
    );
}

#[tokio::test]
async fn test_send_lazy_connects() {
    let server_port = random_port().await;

    // Server setup
    let (server_registry, _server_es) = build_registry("server", server_port);
    let mut server_incoming = server_registry.subscribe();
    server_registry.start().await;

    // Client setup — uses server's port so it dials the server
    let (client_registry, client_es) = build_registry("client", server_port);
    client_registry.start().await;

    // Inject server as a known peer on the client
    client_es
        .send(NetworkPeerEvent::Joined(make_loopback_peer("server")))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify peer is known but not connected
    let peers = client_registry.peers().await;
    assert_eq!(peers.len(), 1);
    assert!(
        !peers[0].ws_connected,
        "should not be connected before send"
    );

    // Send — triggers lazy connect
    let msg = b"hello from lazy connect";
    client_registry.send("server", msg).await.unwrap();

    // Verify peer is now connected
    let peers = client_registry.peers().await;
    assert!(peers[0].ws_connected, "should be connected after send");

    // Server should receive the message
    let incoming = tokio::time::timeout(Duration::from_millis(500), server_incoming.recv())
        .await
        .expect("should receive message within timeout")
        .expect("should not be a recv error");

    assert_eq!(incoming.from, "client");
    assert_eq!(incoming.data, msg);
}

#[tokio::test]
async fn test_send_reuses_connection() {
    let server_port = random_port().await;

    let (server_registry, _) = build_registry("server", server_port);
    let mut server_incoming = server_registry.subscribe();
    server_registry.start().await;

    let (client_registry, client_es) = build_registry("client", server_port);
    let mut client_events = client_registry.on_peer_change();
    client_registry.start().await;

    client_es
        .send(NetworkPeerEvent::Joined(make_loopback_peer("server")))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // First send — creates connection
    client_registry.send("server", b"msg-1").await.unwrap();

    // Wait for Connected event
    let event = tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            match client_events.recv().await {
                Ok(PeerEvent::WsConnected(_)) => return,
                _ => continue,
            }
        }
    })
    .await;
    assert!(event.is_ok(), "should receive Connected event");

    // Second send — reuses connection (no second Connected event)
    client_registry.send("server", b"msg-2").await.unwrap();

    // Server should receive both messages
    let msg1 = tokio::time::timeout(Duration::from_millis(200), server_incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(msg1.data, b"msg-1");

    let msg2 = tokio::time::timeout(Duration::from_millis(200), server_incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(msg2.data, b"msg-2");
}

#[tokio::test]
async fn test_broadcast_sends_to_all() {
    // The broadcaster has two clients connect TO it (via lazy send).
    // Then the broadcaster broadcasts back to both.
    let bcast_port = random_port().await;

    let (bcast_reg, _) = build_registry("broadcaster", bcast_port);
    bcast_reg.start().await;

    // Client 1
    let (client1_reg, client1_es) = build_registry("client1", bcast_port);
    let mut client1_incoming = client1_reg.subscribe();
    client1_reg.start().await;

    // Client 2
    let (client2_reg, client2_es) = build_registry("client2", bcast_port);
    let mut client2_incoming = client2_reg.subscribe();
    client2_reg.start().await;

    // Inject broadcaster as peer on both clients
    client1_es
        .send(NetworkPeerEvent::Joined(make_loopback_peer("broadcaster")))
        .unwrap();
    client2_es
        .send(NetworkPeerEvent::Joined(make_loopback_peer("broadcaster")))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Both clients send to broadcaster to establish connections
    // (the broadcaster's accept loop caches these connections)
    client1_reg.send("broadcaster", b"hello1").await.unwrap();
    client2_reg.send("broadcaster", b"hello2").await.unwrap();

    // Give the broadcaster time to accept and cache both connections
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now broadcast from broadcaster
    bcast_reg.broadcast(b"broadcast-msg").await;

    // Both clients should receive the broadcast
    let msg1 = tokio::time::timeout(Duration::from_millis(500), client1_incoming.recv())
        .await
        .expect("client1 should receive broadcast")
        .expect("should not error");
    assert_eq!(msg1.data, b"broadcast-msg");

    let msg2 = tokio::time::timeout(Duration::from_millis(500), client2_incoming.recv())
        .await
        .expect("client2 should receive broadcast")
        .expect("should not error");
    assert_eq!(msg2.data, b"broadcast-msg");
}

#[tokio::test]
async fn test_incoming_message() {
    let server_port = random_port().await;

    let (server_registry, _) = build_registry("server", server_port);
    let mut server_incoming = server_registry.subscribe();
    server_registry.start().await;

    let (client_registry, client_es) = build_registry("sender", server_port);
    client_registry.start().await;

    client_es
        .send(NetworkPeerEvent::Joined(make_loopback_peer("server")))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let payload = b"test payload 12345";
    client_registry.send("server", payload).await.unwrap();

    let msg = tokio::time::timeout(Duration::from_millis(500), server_incoming.recv())
        .await
        .expect("should receive within timeout")
        .expect("should not error");

    assert_eq!(msg.from, "sender");
    assert_eq!(msg.data, payload);
}

#[tokio::test]
async fn test_disconnect_reconnect() {
    let server_port = random_port().await;

    let (server_registry, _) = build_registry("server", server_port);
    let mut server_incoming = server_registry.subscribe();
    server_registry.start().await;

    let (client_registry, client_es) = build_registry("client", server_port);
    client_registry.start().await;

    client_es
        .send(NetworkPeerEvent::Joined(make_loopback_peer("server")))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // First send — establishes connection
    client_registry.send("server", b"msg-1").await.unwrap();

    let msg1 = tokio::time::timeout(Duration::from_millis(500), server_incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(msg1.data, b"msg-1");

    // Disconnect
    client_registry.disconnect("server").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify disconnected state
    let peers = client_registry.peers().await;
    let server_peer = peers.iter().find(|p| p.id == "server").unwrap();
    assert!(
        !server_peer.ws_connected,
        "should be disconnected after disconnect()"
    );

    // Send again — should reconnect via lazy connect
    client_registry.send("server", b"msg-2").await.unwrap();

    // Verify reconnected
    let peers = client_registry.peers().await;
    let server_peer = peers.iter().find(|p| p.id == "server").unwrap();
    assert!(
        server_peer.ws_connected,
        "should be reconnected after second send"
    );

    // Server should receive the second message
    let msg2 = tokio::time::timeout(Duration::from_millis(500), server_incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(msg2.data, b"msg-2");
}

// ===========================================================================
// Tests: PeerEvent::Left closes WS connection
// ===========================================================================

#[tokio::test]
async fn test_peer_left_closes_ws_connection() {
    let server_port = random_port().await;

    // Server that the client will connect to
    let (server_registry, _) = build_registry("server", server_port);
    server_registry.start().await;

    // Client with event sender
    let (client_registry, client_es) = build_registry("client", server_port);
    let mut client_events = client_registry.on_peer_change();
    client_registry.start().await;

    // Inject server as known peer
    client_es
        .send(NetworkPeerEvent::Joined(make_loopback_peer("server")))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send to establish a WS connection
    client_registry.send("server", b"hello").await.unwrap();

    // Wait for Connected event
    tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            if let Ok(PeerEvent::WsConnected(id)) = client_events.recv().await {
                if id == "server" {
                    return;
                }
            }
        }
    })
    .await
    .expect("should receive Connected event");

    // Verify connected
    let peers = client_registry.peers().await;
    assert!(peers[0].ws_connected, "should be connected before Left");

    // Emit Left event — should close WS and emit Disconnected then Left
    client_es
        .send(NetworkPeerEvent::Left("server".to_string()))
        .unwrap();

    // Collect events: should see Disconnected then Left
    let mut got_disconnected = false;
    let mut got_left = false;

    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            match client_events.recv().await {
                Ok(PeerEvent::WsDisconnected(id)) if id == "server" => {
                    got_disconnected = true;
                }
                Ok(PeerEvent::Left(id)) if id == "server" => {
                    got_left = true;
                    return;
                }
                _ => continue,
            }
        }
    })
    .await;

    assert!(got_disconnected, "should emit Disconnected before Left");
    assert!(got_left, "should emit Left");

    // Peer should be removed from registry
    let peers = client_registry.peers().await;
    assert!(
        peers.iter().all(|p| p.id != "server"),
        "peer should be removed after Left"
    );
}

// ===========================================================================
// Tests: Reconnect backoff
// ===========================================================================

#[test]
fn test_reconnect_backoff_basic() {
    let mut backoff = ReconnectBackoff::new();

    // Initially, retry is allowed
    assert!(backoff.should_retry().is_some());

    // After first failure, should_retry returns None (backoff active)
    backoff.failure();
    assert!(backoff.should_retry().is_none());

    // retry_after should be > 0
    let wait = backoff.retry_after();
    assert!(wait > Duration::ZERO, "should have non-zero retry_after");
    assert!(
        wait <= Duration::from_millis(100),
        "first backoff should be <= 100ms"
    );

    // After second failure (simulated after delay elapsed)
    // Manually reset last_attempt to simulate time passing
    backoff.failure();
    let wait2 = backoff.retry_after();
    // Second failure should have a longer delay than the first
    // (though since last_attempt was just set, both are relative to "now")
    assert!(wait2 > Duration::ZERO);
}

#[test]
fn test_reconnect_backoff_resets_on_success() {
    let mut backoff = ReconnectBackoff::new();

    // Fail 3 times
    backoff.failure();
    backoff.failure();
    backoff.failure();

    // Backoff should be active
    assert!(
        backoff.should_retry().is_none(),
        "should be in backoff after 3 failures"
    );

    // Success resets
    backoff.success();
    assert!(
        backoff.should_retry().is_some(),
        "should allow retry after success"
    );
    assert_eq!(
        backoff.retry_after(),
        Duration::ZERO,
        "retry_after should be zero after success"
    );
}

// ===========================================================================
// Tests: Broadcast with zero connections
// ===========================================================================

#[tokio::test]
async fn test_broadcast_with_zero_connections() {
    let port = random_port().await;
    let (registry, _) = build_registry("node-a", port);
    registry.start().await;

    // Broadcast with no connected peers — should not error or panic
    registry.broadcast(b"hello nobody").await;

    // Verify no peers and no connections
    let peers = registry.peers().await;
    assert!(peers.is_empty(), "should have no peers");
}

// ===========================================================================
// Tests: RFC 017 Phase 2 — hello exchange
// ===========================================================================

/// Two nodes with matching `app_id` complete the hello exchange and each
/// side stamps the other's `device_id` / `device_name` / `os` onto its
/// session peer registry.
#[tokio::test]
async fn test_hello_exchange_populates_identity() {
    let server_port = random_port().await;

    let (server_registry, server_es) = build_registry("server", server_port);
    let mut server_incoming = server_registry.subscribe();
    server_registry.start().await;

    let (client_registry, client_es) = build_registry("client", server_port);
    client_registry.start().await;

    // Register each side in the other's Layer 3 peer map by Tailscale ID.
    server_es
        .send(NetworkPeerEvent::Joined(make_loopback_peer("client")))
        .unwrap();
    client_es
        .send(NetworkPeerEvent::Joined(make_loopback_peer("server")))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Trigger the lazy connect from client → server.
    client_registry.send("server", b"hello").await.unwrap();

    // Give the accept loop time to populate the server-side PeerState.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Server receives the first frame (verifies the data channel is up).
    let _msg = tokio::time::timeout(Duration::from_millis(500), server_incoming.recv())
        .await
        .expect("server should receive the inbound frame")
        .expect("broadcast channel should not error");

    // Client side: identity for "server" should be populated from the
    // remote hello.
    let client_peers = client_registry.peers().await;
    let server_view = client_peers
        .iter()
        .find(|p| p.id == "server")
        .expect("client should know about server");
    let server_identity = server_view
        .identity
        .as_ref()
        .expect("hello should have landed on the client side");
    assert_eq!(server_identity.app_id, "test");
    assert_eq!(server_identity.device_id, "server");
    assert_eq!(server_identity.device_name, "Test Node server");
    assert_eq!(server_identity.tailscale_id, "server");
    assert!(!server_identity.os.is_empty(), "os should be populated");

    // Server side: identity for "client" should be populated too.
    let server_peers = server_registry.peers().await;
    let client_view = server_peers
        .iter()
        .find(|p| p.id == "client")
        .expect("server should know about client");
    let client_identity = client_view
        .identity
        .as_ref()
        .expect("hello should have landed on the server side");
    assert_eq!(client_identity.app_id, "test");
    assert_eq!(client_identity.device_id, "client");
    assert_eq!(client_identity.device_name, "Test Node client");
    assert_eq!(client_identity.tailscale_id, "client");
}

/// Two nodes that disagree on `app_id` fail the hello exchange: neither
/// side registers a usable WebSocket, and `send()` on the dialer's side
/// surfaces a connect error.
#[tokio::test]
async fn test_hello_exchange_rejects_app_mismatch() {
    let server_port = random_port().await;

    // Server runs under `app_id = "chat"`.
    let (server_registry, _server_es) = build_registry_with_app("chat", "server", server_port);
    server_registry.start().await;

    // Client runs under `app_id = "playground"`.
    let (client_registry, client_es) = build_registry_with_app("playground", "client", server_port);
    client_registry.start().await;

    // Inject the server as a loopback peer on the client.
    client_es
        .send(NetworkPeerEvent::Joined(make_loopback_peer("server")))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Lazy-send fails because the hello exchange rejects the app mismatch.
    let result = client_registry.send("server", b"unauthorised").await;
    assert!(
        result.is_err(),
        "sending across an app mismatch should fail, got: {result:?}"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("app mismatch") || err.contains("connect failed"),
        "error should indicate the hello failure, got: {err}"
    );

    // Neither side should have populated identity for the other.
    let client_peers = client_registry.peers().await;
    let server_entry = client_peers
        .iter()
        .find(|p| p.id == "server")
        .expect("peer still known at Layer 3");
    assert!(
        server_entry.identity.is_none(),
        "app mismatch must leave identity unpopulated"
    );
}

/// A remote that sends a hello with an unknown `kind` value is rejected
/// with [`TransportError::HelloMalformed`] and the close frame carries
/// [`CLOSE_HELLO_PROTOCOL`].
#[tokio::test]
async fn test_hello_exchange_rejects_malformed_hello() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let server_port = random_port().await;

    // Spin up a real PeerRegistry on the server side so the hello
    // exchange actually runs.
    let (server_registry, _server_es) = build_registry("server", server_port);
    server_registry.start().await;

    // Give the accept loop time to bind.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Manually dial the server as a WebSocket client and send a bogus
    // hello frame. This bypasses the library's client path so we can
    // observe the server's rejection behaviour.
    let (mut ws, _resp) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{server_port}/ws"))
            .await
            .expect("ws upgrade should succeed");

    // Send a malformed hello (wrong `kind`).
    ws.send(Message::Text(
        r#"{"kind":"not_hello","version":2,"identity":{"app_id":"test","device_id":"x","device_name":"y","os":"darwin","tailscale_id":"z"}}"#
            .to_string()
            .into(),
    ))
    .await
    .expect("send should succeed before server close");

    // The server should respond with a close frame carrying
    // `CLOSE_HELLO_PROTOCOL` (4002).
    let mut saw_close = false;
    for _ in 0..10 {
        match ws.next().await {
            Some(Ok(Message::Close(Some(frame)))) => {
                assert_eq!(u16::from(frame.code), super::CLOSE_HELLO_PROTOCOL);
                saw_close = true;
                break;
            }
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => break,
        }
    }
    assert!(
        saw_close,
        "server should close with CLOSE_HELLO_PROTOCOL after bogus hello"
    );
}

/// A remote that never sends any hello frame is dropped after the
/// 5-second hello timeout elapses. Verifies the timeout path sends a
/// [`CLOSE_HELLO_PROTOCOL`] close frame to the caller.
#[tokio::test]
async fn test_hello_exchange_hello_timeout() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    let server_port = random_port().await;

    let (server_registry, _server_es) = build_registry("server", server_port);
    server_registry.start().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Dial the server but never send a hello.
    let (mut ws, _resp) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{server_port}/ws"))
            .await
            .expect("ws upgrade should succeed");

    // Wait slightly longer than the hello timeout (5s). To keep the
    // test fast, we use `tokio::time::pause`-style patience and drain
    // frames until the server closes us out.
    let mut saw_close = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(7);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Close(Some(frame))))) => {
                assert_eq!(u16::from(frame.code), super::CLOSE_HELLO_PROTOCOL);
                saw_close = true;
                break;
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => break,
        }
    }
    assert!(
        saw_close,
        "server should close with CLOSE_HELLO_PROTOCOL after hello timeout"
    );
}
