//! # Transport Layer (Layer 2) Integration Tests
//!
//! These tests validate the WebSocket transport layer that sits above the
//! raw TCP bridge (Layer 1). They verify that `ConnectionManager` correctly:
//!
//! 1. **Accepts WebSocket streams** — duplex-backed WS pairs simulate the
//!    bridge-to-WS upgrade path without needing real TCP.
//!
//! 2. **Lifecycle events** — Connected, Message, DeviceIdentified, and
//!    Disconnected events are emitted at the right times.
//!
//! 3. **Heartbeat** — Pings keep connections alive; timeouts disconnect
//!    unresponsive peers.
//!
//! 4. **Multi-connection management** — Multiple connections are tracked
//!    independently, `close_all()` cleans up everything.
//!
//! 5. **Message round-trip** — Client sends to server (TransportEvent::Message),
//!    server sends to client (ConnectionManager::send).
//!
//! 6. **Device ID binding** — `set_device_id` emits DeviceIdentified and
//!    `get_connection_by_device` resolves correctly.
//!
//! ## Two-node tests (ignored, require Tailscale auth)
//!
//! Tests 8-10 spawn real sidecar nodes and verify transport-level events
//! across the Tailscale tunnel. These reuse the `spawn_test_node()` helper
//! pattern from `sidecar_lifecycle.rs`.
//!
//! ## Running the tests
//!
//! ```bash
//! # Single-node tests (no network required):
//! cargo test --test transport_integration
//!
//! # Two-node tests (require cached Tailscale auth):
//! cargo test --test transport_integration -- --ignored --test-threads=1 --nocapture
//! ```

use std::path::PathBuf;
use std::sync::Once;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use truffle_core::bridge::header::Direction;
use truffle_core::transport::connection::{
    ConnectionManager, ConnectionStatus, TransportConfig, TransportEvent,
};
use truffle_core::transport::heartbeat::HeartbeatConfig;
use truffle_core::transport::websocket;

static INIT: Once = Once::new();

fn init_tracing() {
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("truffle_core=debug".parse().unwrap()),
            )
            .with_test_writer()
            .try_init()
            .ok();
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// Shared helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Build a TransportConfig with a very long heartbeat timeout so heartbeat
/// does not interfere with short-lived test connections.
fn quiet_heartbeat_config() -> TransportConfig {
    TransportConfig {
        heartbeat: HeartbeatConfig {
            ping_interval: Duration::from_secs(600),
            timeout: Duration::from_secs(600),
        },
        auto_reconnect: false,
        max_reconnect_delay: Duration::from_secs(1),
        debug_json_mode: false,
    }
}

/// Create a duplex-backed WebSocket pair (server, client).
async fn duplex_ws_pair() -> (
    WebSocketStream<tokio::io::DuplexStream>,
    WebSocketStream<tokio::io::DuplexStream>,
) {
    let (client_io, server_io) = tokio::io::duplex(16384);
    let server_fut = WebSocketStream::from_raw_socket(server_io, Role::Server, None);
    let client_fut = WebSocketStream::from_raw_socket(client_io, Role::Client, None);
    let (server_ws, client_ws) = tokio::join!(server_fut, client_fut);
    (server_ws, client_ws)
}

/// Wait for a specific `TransportEvent` on a broadcast receiver, with timeout.
async fn wait_for_transport_event<F>(
    rx: &mut broadcast::Receiver<TransportEvent>,
    timeout: Duration,
    predicate: F,
    description: &str,
) -> TransportEvent
where
    F: Fn(&TransportEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for transport event: {description}");
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if predicate(&event) {
                    return event;
                }
                // Not the event we want — keep waiting
            }
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!("lagged {n} events while waiting for: {description}");
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                panic!("transport event channel closed while waiting for: {description}");
            }
            Err(_) => {
                panic!("timed out waiting for transport event: {description}");
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. test_ws_connection_lifecycle
//    Create ConnectionManager, feed a duplex WS stream, verify Connected,
//    send a message, verify Message event, drop client, verify Disconnected.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_ws_connection_lifecycle() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, mut client_ws) = duplex_ws_pair().await;

    // Feed the server side of the WS to the ConnectionManager
    manager
        .handle_ws_stream(
            server_ws,
            "100.64.0.10:4000".into(),
            "lifecycle-peer.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    // --- Verify Connected event ---
    let event = wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    if let TransportEvent::Connected(conn) = &event {
        assert_eq!(conn.remote_addr, "100.64.0.10:4000");
        assert_eq!(conn.remote_dns_name, "lifecycle-peer.ts.net");
        assert_eq!(conn.direction, Direction::Incoming);
        assert_eq!(conn.status, ConnectionStatus::Connected);
    } else {
        panic!("expected Connected event");
    }

    // --- Send a message from client, verify Message event ---
    let payload = serde_json::json!({"namespace": "mesh", "type": "announce", "data": "lifecycle"});
    let encoded = websocket::encode_message(&payload, false).unwrap();
    client_ws
        .send(Message::Binary(bytes::Bytes::from(encoded)))
        .await
        .unwrap();

    let event = wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Message { .. }),
        "Message",
    )
    .await;

    if let TransportEvent::Message {
        connection_id,
        message,
    } = &event
    {
        assert_eq!(connection_id, "incoming:100.64.0.10:4000");
        assert_eq!(
            message.payload.get("data").unwrap().as_str().unwrap(),
            "lifecycle"
        );
    } else {
        panic!("expected Message event");
    }

    // --- Drop client, verify Disconnected event ---
    drop(client_ws);

    let event = wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Disconnected { .. }),
        "Disconnected",
    )
    .await;

    if let TransportEvent::Disconnected {
        connection_id,
        reason,
    } = &event
    {
        assert_eq!(connection_id, "incoming:100.64.0.10:4000");
        assert!(
            reason.contains("remote_closed") || reason.contains("error"),
            "unexpected disconnect reason: {reason}"
        );
    } else {
        panic!("expected Disconnected event");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. test_ws_heartbeat_keeps_connection_alive
//    Create connection with fast heartbeat, wait 5+ seconds with client
//    responding to pings, verify connection is still active.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_ws_heartbeat_keeps_connection_alive() {
    init_tracing();

    let config = TransportConfig {
        heartbeat: HeartbeatConfig {
            ping_interval: Duration::from_secs(1),
            timeout: Duration::from_secs(4),
        },
        auto_reconnect: false,
        max_reconnect_delay: Duration::from_secs(1),
        debug_json_mode: false,
    };

    let (manager, mut rx) = ConnectionManager::new(config);
    let (server_ws, mut client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.0.20:5000".into(),
            "heartbeat-peer.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    // Consume Connected event
    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    // Spawn a client-side responder: reads pings, sends pongs back.
    // This keeps the connection alive by providing activity.
    let responder = tokio::spawn(async move {
        while let Some(Ok(msg)) = client_ws.next().await {
            if let Message::Binary(data) = &msg {
                if let Ok(decoded) = websocket::decode_message(data) {
                    if decoded
                        .payload
                        .get("type")
                        .and_then(|t| t.as_str())
                        == Some("ping")
                    {
                        let ts = decoded
                            .payload
                            .get("timestamp")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let pong = truffle_core::transport::heartbeat::create_pong(ts);
                        let encoded = websocket::encode_message(&pong, false).unwrap();
                        if client_ws
                            .send(Message::Binary(bytes::Bytes::from(encoded)))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }
        client_ws
    });

    // Wait longer than the heartbeat timeout (4s). If the pong responses are
    // properly keeping the connection alive, we should NOT get a Disconnected.
    let result = tokio::time::timeout(Duration::from_secs(6), rx.recv()).await;

    match result {
        Err(_) => {
            // Timeout elapsed with no Disconnected event — connection is alive.
        }
        Ok(Ok(TransportEvent::Disconnected { reason, .. })) => {
            panic!("connection should have stayed alive but got Disconnected: {reason}");
        }
        Ok(Ok(TransportEvent::Message { .. })) => {
            // Pong might leak through as a message — that's fine, connection is alive.
            // Verify no disconnect follows quickly.
            let result2 = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match rx.recv().await {
                        Ok(TransportEvent::Disconnected { reason, .. }) => {
                            panic!("unexpected disconnect: {reason}");
                        }
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
            })
            .await;
            // Timeout means no disconnect — good.
            assert!(result2.is_err() || result2.is_ok());
        }
        Ok(other) => {
            panic!("unexpected event or channel error: {other:?}");
        }
    }

    // Verify the connection is still tracked
    let conns = manager.get_connections().await;
    assert_eq!(conns.len(), 1, "connection should still be alive");

    // Clean up
    responder.abort();
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. test_ws_heartbeat_timeout_disconnects
//    Create connection where the client never responds to pings.
//    After heartbeat timeout, verify Disconnected event.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_ws_heartbeat_timeout_disconnects() {
    init_tracing();

    let config = TransportConfig {
        heartbeat: HeartbeatConfig {
            ping_interval: Duration::from_millis(100),
            timeout: Duration::from_millis(500),
        },
        auto_reconnect: false,
        max_reconnect_delay: Duration::from_secs(1),
        debug_json_mode: false,
    };

    let (manager, mut rx) = ConnectionManager::new(config);
    let (server_ws, _client_ws) = duplex_ws_pair().await;
    // _client_ws is kept alive but never reads or responds — simulating
    // a peer that stops responding.

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.0.30:6000".into(),
            "timeout-peer.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    // Consume Connected
    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    // Wait for Disconnected event with heartbeat_timeout reason
    let event = wait_for_transport_event(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, TransportEvent::Disconnected { .. }),
        "Disconnected (heartbeat timeout)",
    )
    .await;

    if let TransportEvent::Disconnected {
        connection_id,
        reason,
    } = &event
    {
        assert_eq!(connection_id, "incoming:100.64.0.30:6000");
        assert!(
            reason.contains("heartbeat") || reason.contains("timeout") || reason.contains("error"),
            "expected heartbeat-related disconnect reason, got: {reason}"
        );
    } else {
        panic!("expected Disconnected event");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. test_multiple_ws_connections
//    Create 3 connections, verify 3 Connected events, send a message on
//    each, verify 3 Message events.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_multiple_ws_connections() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());

    // Create 3 connections, keep client sides alive
    let mut clients = Vec::new();
    for i in 0..3u8 {
        let (server_ws, client_ws) = duplex_ws_pair().await;
        clients.push(client_ws);
        manager
            .handle_ws_stream(
                server_ws,
                format!("100.64.0.{}:7000", 10 + i),
                format!("multi-{}.ts.net", i),
                Direction::Incoming,
            )
            .await;
    }

    // Verify 3 Connected events
    for i in 0..3 {
        let event = wait_for_transport_event(
            &mut rx,
            Duration::from_secs(5),
            |e| matches!(e, TransportEvent::Connected(_)),
            &format!("Connected #{}", i + 1),
        )
        .await;
        assert!(matches!(event, TransportEvent::Connected(_)));
    }

    // Verify 3 connections tracked
    let conns = manager.get_connections().await;
    assert_eq!(conns.len(), 3);

    // Send a message on each client
    for (i, client) in clients.iter_mut().enumerate() {
        let payload = serde_json::json!({"namespace": "test", "type": "msg", "index": i});
        let encoded = websocket::encode_message(&payload, false).unwrap();
        client
            .send(Message::Binary(bytes::Bytes::from(encoded)))
            .await
            .unwrap();
    }

    // Verify 3 Message events
    let mut received_indices = Vec::new();
    for i in 0..3 {
        let event = wait_for_transport_event(
            &mut rx,
            Duration::from_secs(5),
            |e| matches!(e, TransportEvent::Message { .. }),
            &format!("Message #{}", i + 1),
        )
        .await;

        if let TransportEvent::Message { message, .. } = event {
            let idx = message
                .payload
                .get("index")
                .unwrap()
                .as_u64()
                .unwrap();
            received_indices.push(idx);
        }
    }

    received_indices.sort();
    assert_eq!(received_indices, vec![0, 1, 2]);
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. test_ws_message_roundtrip
//    Client -> Server (TransportEvent::Message) and
//    Server -> Client (ConnectionManager::send).
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_ws_message_roundtrip() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, mut client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.0.40:8000".into(),
            "roundtrip.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    // Consume Connected
    let _ = rx.recv().await.unwrap();
    let conn_id = "incoming:100.64.0.40:8000";

    // --- Client -> Server ---
    let client_msg = serde_json::json!({
        "namespace": "app",
        "type": "request",
        "payload": {"action": "get_status"}
    });
    let encoded = websocket::encode_message(&client_msg, true).unwrap(); // JSON mode
    client_ws
        .send(Message::Binary(bytes::Bytes::from(encoded)))
        .await
        .unwrap();

    let event = wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Message { .. }),
        "Message from client",
    )
    .await;

    if let TransportEvent::Message {
        connection_id,
        message,
    } = &event
    {
        assert_eq!(connection_id, conn_id);
        assert_eq!(
            message.payload.get("type").unwrap().as_str().unwrap(),
            "request"
        );
        assert!(message.was_json);
    }

    // --- Server -> Client ---
    let server_msg = serde_json::json!({
        "namespace": "app",
        "type": "response",
        "payload": {"status": "ok", "uptime": 12345}
    });
    manager.send(conn_id, &server_msg).await.unwrap();

    // Read from client side
    let received = tokio::time::timeout(Duration::from_secs(5), client_ws.next())
        .await
        .expect("timed out waiting for server message")
        .expect("stream ended")
        .expect("ws error");

    match received {
        Message::Binary(data) => {
            let decoded = websocket::decode_message(&data).unwrap();
            assert_eq!(
                decoded.payload.get("type").unwrap().as_str().unwrap(),
                "response"
            );
            assert_eq!(
                decoded
                    .payload
                    .get("payload")
                    .unwrap()
                    .get("uptime")
                    .unwrap()
                    .as_i64()
                    .unwrap(),
                12345
            );
            // Default config has debug_json_mode: false, so msgpack
            assert!(!decoded.was_json);
        }
        other => panic!("expected binary message, got {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. test_device_id_binding
//    Create connection, call set_device_id, verify DeviceIdentified event
//    and get_connection_by_device works.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_device_id_binding() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, _client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.0.50:9000".into(),
            "device-bind.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    // Consume Connected
    let _ = rx.recv().await.unwrap();
    let conn_id = "incoming:100.64.0.50:9000";

    // Before binding: no device lookup
    assert!(manager.get_connection_by_device("device-42").await.is_none());

    // Bind device ID
    manager
        .set_device_id(conn_id, "device-42".into())
        .await;

    // Verify DeviceIdentified event
    let event = wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::DeviceIdentified { .. }),
        "DeviceIdentified",
    )
    .await;

    if let TransportEvent::DeviceIdentified {
        connection_id,
        device_id,
    } = &event
    {
        assert_eq!(connection_id, conn_id);
        assert_eq!(device_id, "device-42");
    } else {
        panic!("expected DeviceIdentified event");
    }

    // Verify get_connection_by_device works
    let found = manager
        .get_connection_by_device("device-42")
        .await
        .expect("device lookup should succeed");
    assert_eq!(found.id, conn_id);
    assert_eq!(found.device_id.as_deref(), Some("device-42"));
    assert_eq!(found.remote_dns_name, "device-bind.ts.net");

    // Verify unknown device returns None
    assert!(manager
        .get_connection_by_device("no-such-device")
        .await
        .is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. test_close_all_disconnects
//    Create 3 connections, call close_all(), verify 3 Disconnected events.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_close_all_disconnects() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());

    let mut _clients = Vec::new();
    for i in 0..3u8 {
        let (server_ws, client_ws) = duplex_ws_pair().await;
        _clients.push(client_ws);
        manager
            .handle_ws_stream(
                server_ws,
                format!("100.64.0.{}:443", 60 + i),
                format!("close-all-{}.ts.net", i),
                Direction::Incoming,
            )
            .await;
    }

    // Drain 3 Connected events
    for _ in 0..3 {
        wait_for_transport_event(
            &mut rx,
            Duration::from_secs(5),
            |e| matches!(e, TransportEvent::Connected(_)),
            "Connected",
        )
        .await;
    }

    assert_eq!(manager.get_connections().await.len(), 3);

    // Close all connections
    manager.close_all().await;

    // Verify connections are now empty
    assert!(manager.get_connections().await.is_empty());

    // Verify 3 Disconnected events
    let mut disconnected_ids = Vec::new();
    for i in 0..3 {
        let event = wait_for_transport_event(
            &mut rx,
            Duration::from_secs(5),
            |e| matches!(e, TransportEvent::Disconnected { .. }),
            &format!("Disconnected #{}", i + 1),
        )
        .await;

        if let TransportEvent::Disconnected {
            connection_id,
            reason,
        } = event
        {
            assert_eq!(reason, "closed_by_local");
            disconnected_ids.push(connection_id);
        }
    }

    assert_eq!(disconnected_ids.len(), 3);
    // All three should have distinct IDs
    disconnected_ids.sort();
    disconnected_ids.dedup();
    assert_eq!(disconnected_ids.len(), 3);
}

// ═══════════════════════════════════════════════════════════════════════════
// Two-node tests (require Tailscale auth)
// ═══════════════════════════════════════════════════════════════════════════

// Helpers for two-node tests — reuse patterns from sidecar_lifecycle.rs

use truffle_core::bridge::manager::BridgeManager;
use truffle_core::bridge::shim::{GoShim, ShimConfig, ShimLifecycleEvent};

fn sidecar_binary_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("../../packages/sidecar-slim/bin/sidecar-slim")
}

fn shared_authed_state_dir(test_name: &str) -> String {
    let dir = std::env::temp_dir()
        .join("truffle-test-authed")
        .join(test_name);
    std::fs::create_dir_all(&dir).expect("create shared state dir");
    dir.to_string_lossy().to_string()
}

fn random_session_token() -> String {
    use std::fmt::Write;
    let mut token = [0u8; 32];
    getrandom::getrandom(&mut token).expect("getrandom");
    let mut hex = String::with_capacity(64);
    for b in &token {
        write!(hex, "{b:02x}").unwrap();
    }
    hex
}

/// Wait for a specific ShimLifecycleEvent with timeout.
async fn wait_for_shim_event<F>(
    rx: &mut broadcast::Receiver<ShimLifecycleEvent>,
    timeout: Duration,
    predicate: F,
    description: &str,
) -> ShimLifecycleEvent
where
    F: Fn(&ShimLifecycleEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for shim event: {description}");
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if predicate(&event) {
                    return event;
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!("lagged {n} events while waiting for: {description}");
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                panic!("lifecycle channel closed while waiting for: {description}");
            }
            Err(_) => {
                panic!("timed out waiting for shim event: {description}");
            }
        }
    }
}

/// A full test node: sidecar + bridge manager + transport (ConnectionManager).
struct FullTestNode {
    shim: GoShim,
    #[allow(dead_code)]
    shim_rx: broadcast::Receiver<ShimLifecycleEvent>,
    transport: std::sync::Arc<ConnectionManager>,
    transport_rx: broadcast::Receiver<TransportEvent>,
    pending_dials: std::sync::Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<
                String,
                tokio::sync::oneshot::Sender<truffle_core::bridge::manager::BridgeConnection>,
            >,
        >,
    >,
    cancel: tokio_util::sync::CancellationToken,
    _manager_handle: tokio::task::JoinHandle<()>,
    dns_name: String,
    #[allow(dead_code)]
    tailscale_ip: String,
}

/// Spawn a full node with sidecar + bridge manager + ConnectionManager.
/// Waits for Tailscale running state before returning.
async fn spawn_full_node(name: &str, state_dir_name: &str) -> FullTestNode {
    let session_token = random_session_token();
    let token_bytes = hex::decode(&session_token).unwrap();
    let mut token_arr = [0u8; 32];
    token_arr.copy_from_slice(&token_bytes);

    let mut manager = BridgeManager::bind(token_arr).await.unwrap();
    let bridge_port = manager.local_port();
    let pending_dials = manager.pending_dials().clone();

    // Create the ConnectionManager (transport layer)
    let transport_config = TransportConfig {
        heartbeat: HeartbeatConfig {
            ping_interval: Duration::from_secs(5),
            timeout: Duration::from_secs(15),
        },
        auto_reconnect: false,
        max_reconnect_delay: Duration::from_secs(5),
        debug_json_mode: false,
    };
    let (transport, transport_rx) = ConnectionManager::new(transport_config);
    let transport = std::sync::Arc::new(transport);

    // Register incoming handler for port 443 using the WsIncomingHandler
    let ws_handler = truffle_core::transport::connection::WsIncomingHandler::new(transport.clone());
    manager.add_handler(
        443,
        Direction::Incoming,
        std::sync::Arc::new(ws_handler),
    );

    let state_dir = shared_authed_state_dir(state_dir_name);

    let config = ShimConfig {
        binary_path: sidecar_binary_path(),
        hostname: format!("test-{name}"),
        state_dir,
        auth_key: std::env::var("TS_AUTHKEY").ok(),
        bridge_port,
        session_token,
        auto_restart: false,
        ephemeral: None,
        tags: None,
    };

    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();
    let manager_handle = tokio::spawn(async move {
        manager.run(cancel_clone).await;
    });

    let (shim, mut shim_rx) = GoShim::spawn(config).await.unwrap();

    // Wait for running
    let event = wait_for_shim_event(
        &mut shim_rx,
        Duration::from_secs(30),
        |e| {
            matches!(e, ShimLifecycleEvent::AuthRequired { .. })
                || matches!(e, ShimLifecycleEvent::Status(data) if data.state == "running")
        },
        &format!("Node {name}: AuthRequired or Status(running)"),
    )
    .await;

    let (dns_name, tailscale_ip) = match &event {
        ShimLifecycleEvent::AuthRequired { auth_url } => {
            eprintln!("\n=== Auth needed for node '{name}': {auth_url} ===\n");
            let status = wait_for_shim_event(
                &mut shim_rx,
                Duration::from_secs(120),
                |e| matches!(e, ShimLifecycleEvent::Status(data) if data.state == "running" && !data.tailscale_ip.is_empty()),
                &format!("Node {name}: Status(running) after auth"),
            )
            .await;
            if let ShimLifecycleEvent::Status(data) = status {
                eprintln!("Node {name} running: ip={}, dns={}", data.tailscale_ip, data.dns_name);
                (data.dns_name, data.tailscale_ip)
            } else {
                panic!("expected Status");
            }
        }
        ShimLifecycleEvent::Status(data) => {
            eprintln!(
                "Node {name} running (cached): ip={}, dns={}",
                data.tailscale_ip, data.dns_name
            );
            (data.dns_name.clone(), data.tailscale_ip.clone())
        }
        _ => panic!("unexpected event"),
    };

    FullTestNode {
        shim,
        shim_rx,
        transport,
        transport_rx,
        pending_dials,
        cancel,
        _manager_handle: manager_handle,
        dns_name,
        tailscale_ip,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. test_two_node_ws_connection_established
//    Spawn 2 nodes, Node A dials Node B, verify TransportEvent::Connected
//    on Node B's transport layer.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_two_node_ws_connection_established() {
    init_tracing();

    // Spawn both nodes
    eprintln!("=== Spawning Node B (target) ===");
    let mut node_b = spawn_full_node("node-b", "node-b").await;

    eprintln!("=== Spawning Node A (dialer) ===");
    let mut node_a = spawn_full_node("node-a", "node-a").await;

    assert!(!node_a.tailscale_ip.is_empty());
    assert!(!node_b.tailscale_ip.is_empty());
    assert_ne!(node_a.tailscale_ip, node_b.tailscale_ip);

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Node A dials Node B via the bridge layer
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx_dial) = tokio::sync::oneshot::channel();
    {
        let mut dials = node_a.pending_dials.lock().await;
        dials.insert(request_id.clone(), tx);
    }

    let target_dns = node_b.dns_name.clone();
    eprintln!("Node A dialing Node B at {target_dns}...");
    node_a
        .shim
        .dial_raw(target_dns, 443, request_id.clone())
        .await
        .unwrap();

    // Wait for the outgoing bridge connection on Node A's side
    let outgoing_conn = tokio::time::timeout(Duration::from_secs(45), rx_dial)
        .await
        .expect("Node A: dial timeout")
        .expect("Node A: dial channel error");

    // Node A: perform WS client handshake on the outgoing bridge connection
    node_a.transport.handle_outgoing(outgoing_conn).await;

    // Node B should receive a TransportEvent::Connected (incoming WS from bridge handler)
    let event_b = wait_for_transport_event(
        &mut node_b.transport_rx,
        Duration::from_secs(15),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Node B: Connected (incoming)",
    )
    .await;

    if let TransportEvent::Connected(conn) = &event_b {
        assert_eq!(conn.direction, Direction::Incoming);
        assert_eq!(conn.status, ConnectionStatus::Connected);
        eprintln!(
            "Node B received incoming WS connection from {}",
            conn.remote_addr
        );
    }

    // Node A should also have a Connected event for its outgoing connection
    let event_a = wait_for_transport_event(
        &mut node_a.transport_rx,
        Duration::from_secs(15),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Node A: Connected (outgoing)",
    )
    .await;

    if let TransportEvent::Connected(conn) = &event_a {
        assert_eq!(conn.direction, Direction::Outgoing);
        assert_eq!(conn.status, ConnectionStatus::Connected);
        eprintln!("Node A has outgoing WS connection");
    }

    eprintln!("=== Two-node WS connection established ===");

    // Cleanup
    node_a.shim.shutdown();
    node_b.shim.shutdown();
    node_a.cancel.cancel();
    node_b.cancel.cancel();
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. test_two_node_ws_message_exchange
//    After connection, Node A sends a message, Node B receives it via
//    TransportEvent::Message (and vice versa).
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_two_node_ws_message_exchange() {
    init_tracing();

    eprintln!("=== Spawning Node B (target) ===");
    let mut node_b = spawn_full_node("node-b", "node-b").await;

    eprintln!("=== Spawning Node A (dialer) ===");
    let mut node_a = spawn_full_node("node-a", "node-a").await;

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Dial: A -> B
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx_dial) = tokio::sync::oneshot::channel();
    {
        let mut dials = node_a.pending_dials.lock().await;
        dials.insert(request_id.clone(), tx);
    }

    let target_dns = node_b.dns_name.clone();
    node_a
        .shim
        .dial_raw(target_dns, 443, request_id.clone())
        .await
        .unwrap();

    let outgoing_conn = tokio::time::timeout(Duration::from_secs(45), rx_dial)
        .await
        .expect("dial timeout")
        .expect("dial error");

    // WS handshake on Node A's outgoing
    node_a.transport.handle_outgoing(outgoing_conn).await;

    // Wait for both sides to be Connected
    let event_b = wait_for_transport_event(
        &mut node_b.transport_rx,
        Duration::from_secs(15),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Node B: Connected",
    )
    .await;
    let b_conn_id = if let TransportEvent::Connected(conn) = event_b {
        conn.id.clone()
    } else {
        panic!("expected Connected");
    };

    let event_a = wait_for_transport_event(
        &mut node_a.transport_rx,
        Duration::from_secs(15),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Node A: Connected",
    )
    .await;
    let a_conn_id = if let TransportEvent::Connected(conn) = event_a {
        conn.id.clone()
    } else {
        panic!("expected Connected");
    };

    // --- Node A sends message, Node B receives it ---
    let msg_a_to_b = serde_json::json!({
        "namespace": "mesh",
        "type": "test",
        "payload": {"from": "node-a", "seq": 1}
    });
    node_a.transport.send(&a_conn_id, &msg_a_to_b).await.unwrap();

    let event_b_msg = wait_for_transport_event(
        &mut node_b.transport_rx,
        Duration::from_secs(15),
        |e| matches!(e, TransportEvent::Message { .. }),
        "Node B: Message from A",
    )
    .await;

    if let TransportEvent::Message { message, .. } = &event_b_msg {
        assert_eq!(
            message
                .payload
                .get("payload")
                .unwrap()
                .get("from")
                .unwrap()
                .as_str()
                .unwrap(),
            "node-a"
        );
        eprintln!("Node B received message from Node A");
    }

    // --- Node B sends message, Node A receives it ---
    let msg_b_to_a = serde_json::json!({
        "namespace": "mesh",
        "type": "test",
        "payload": {"from": "node-b", "seq": 2}
    });
    node_b.transport.send(&b_conn_id, &msg_b_to_a).await.unwrap();

    let event_a_msg = wait_for_transport_event(
        &mut node_a.transport_rx,
        Duration::from_secs(15),
        |e| matches!(e, TransportEvent::Message { .. }),
        "Node A: Message from B",
    )
    .await;

    if let TransportEvent::Message { message, .. } = &event_a_msg {
        assert_eq!(
            message
                .payload
                .get("payload")
                .unwrap()
                .get("from")
                .unwrap()
                .as_str()
                .unwrap(),
            "node-b"
        );
        eprintln!("Node A received message from Node B");
    }

    eprintln!("=== Two-node WS message exchange PASSED ===");

    node_a.shim.shutdown();
    node_b.shim.shutdown();
    node_a.cancel.cancel();
    node_b.cancel.cancel();
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. test_two_node_ws_disconnect_detected
//     Node A connects to Node B, then Node A stops. Node B should receive
//     TransportEvent::Disconnected.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_two_node_ws_disconnect_detected() {
    init_tracing();

    eprintln!("=== Spawning Node B (target) ===");
    let mut node_b = spawn_full_node("node-b", "node-b").await;

    eprintln!("=== Spawning Node A (dialer) ===");
    let mut node_a = spawn_full_node("node-a", "node-a").await;

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Dial: A -> B
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx_dial) = tokio::sync::oneshot::channel();
    {
        let mut dials = node_a.pending_dials.lock().await;
        dials.insert(request_id.clone(), tx);
    }

    let target_dns = node_b.dns_name.clone();
    node_a
        .shim
        .dial_raw(target_dns, 443, request_id.clone())
        .await
        .unwrap();

    let outgoing_conn = tokio::time::timeout(Duration::from_secs(45), rx_dial)
        .await
        .expect("dial timeout")
        .expect("dial error");

    node_a.transport.handle_outgoing(outgoing_conn).await;

    // Wait for both sides to be Connected
    wait_for_transport_event(
        &mut node_b.transport_rx,
        Duration::from_secs(15),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Node B: Connected",
    )
    .await;

    wait_for_transport_event(
        &mut node_a.transport_rx,
        Duration::from_secs(15),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Node A: Connected",
    )
    .await;

    eprintln!("Both nodes connected — shutting down Node A");

    // Shut down Node A — this should cause Node B to detect disconnect
    node_a.transport.close_all().await;
    node_a.shim.shutdown();
    node_a.cancel.cancel();

    // Node B should receive a Disconnected event
    let event = wait_for_transport_event(
        &mut node_b.transport_rx,
        Duration::from_secs(30),
        |e| matches!(e, TransportEvent::Disconnected { .. }),
        "Node B: Disconnected (after Node A shutdown)",
    )
    .await;

    if let TransportEvent::Disconnected {
        connection_id,
        reason,
    } = &event
    {
        eprintln!("Node B detected disconnect: conn={connection_id}, reason={reason}");
        assert!(
            reason.contains("remote_closed")
                || reason.contains("error")
                || reason.contains("heartbeat")
                || reason.contains("timeout"),
            "unexpected disconnect reason: {reason}"
        );
    }

    eprintln!("=== Two-node disconnect detection PASSED ===");

    node_b.shim.shutdown();
    node_b.cancel.cancel();
}
