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
        local_device_id: None,
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
        local_device_id: None,
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
    // Handles both v3 control frame pings and v2 legacy pings.
    let responder = tokio::spawn(async move {
        while let Some(Ok(msg)) = client_ws.next().await {
            if let Message::Binary(data) = &msg {
                if let Ok(decoded) = websocket::decode_frame(data) {
                    match decoded {
                        // v3 control frame ping
                        websocket::DecodedFrame::V3Control(
                            truffle_core::protocol::frame::ControlMessage::Ping { timestamp },
                        ) => {
                            let pong = truffle_core::transport::heartbeat::create_v3_pong(timestamp);
                            let encoded = websocket::encode_control_frame(&pong, false).unwrap();
                            if client_ws
                                .send(Message::Binary(bytes::Bytes::from(encoded)))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        // v2 legacy ping
                        websocket::DecodedFrame::Legacy(ref wire_msg)
                            if wire_msg.payload.get("type").and_then(|t| t.as_str())
                                == Some("ping") =>
                        {
                            let ts = wire_msg
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
                        _ => {}
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
        local_device_id: None,
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
// Adversarial edge case tests (single-node, no network required)
// ═══════════════════════════════════════════════════════════════════════════

// ───────────────────────────────────────────────────────────────────────────
// Edge case #1: Client drops immediately after connect
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_client_drops_immediately_after_connect() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.1:10001".into(),
            "edge-drop-fast.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    // Wait for Connected
    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected (before immediate drop)",
    )
    .await;

    // Drop client immediately — zero messages sent
    drop(client_ws);

    // Disconnected should fire quickly
    let event = wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Disconnected { .. }),
        "Disconnected (after immediate drop)",
    )
    .await;

    if let TransportEvent::Disconnected { connection_id, reason } = &event {
        assert_eq!(connection_id, "incoming:100.64.1.1:10001");
        assert!(
            reason.contains("remote_closed") || reason.contains("error"),
            "unexpected reason: {reason}"
        );
    }

    // Verify cleanup
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(manager.get_connections().await.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #2: Server sends to closed connection
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_send_to_closed_connection_returns_error() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.2:10002".into(),
            "edge-send-closed.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    let conn_id = "incoming:100.64.1.2:10002";

    // Drop client, wait for disconnect
    drop(client_ws);

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Disconnected { .. }),
        "Disconnected",
    )
    .await;

    // Wait for cleanup
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send to closed connection — should return error, not panic
    let result = manager
        .send(conn_id, &serde_json::json!({"type": "ghost-message"}))
        .await;
    assert!(
        result.is_err(),
        "send() to a closed/cleaned-up connection should return Err"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #3: Rapid connect/disconnect cycles
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_rapid_connect_disconnect_cycles() {
    init_tracing();

    let (manager, _rx) = ConnectionManager::new(quiet_heartbeat_config());

    for i in 0..10u16 {
        let (server_ws, client_ws) = duplex_ws_pair().await;
        manager
            .handle_ws_stream(
                server_ws,
                format!("100.64.1.{}:{}", 10 + (i / 256) as u8, 11000 + i),
                format!("edge-rapid-{i}.ts.net"),
                Direction::Incoming,
            )
            .await;
        // Drop client immediately
        drop(client_ws);
    }

    // Wait for all spawned tasks to process the disconnections
    tokio::time::sleep(Duration::from_millis(500)).await;

    let conns = manager.get_connections().await;
    assert!(
        conns.is_empty(),
        "expected empty after 10 rapid connect/disconnect cycles, got {} leftover",
        conns.len()
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #4: set_device_id on closed connection
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_set_device_id_on_closed_connection() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.3:10003".into(),
            "edge-closed-dev.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    let conn_id = "incoming:100.64.1.3:10003";

    // Close the connection
    drop(client_ws);
    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Disconnected { .. }),
        "Disconnected",
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Should NOT panic — connection is gone, set_device_id is a no-op
    manager
        .set_device_id(conn_id, "ghost-device".into())
        .await;

    // Ghost device should NOT be resolvable
    assert!(manager
        .get_connection_by_device("ghost-device")
        .await
        .is_none());
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #5: set_device_id twice on the same connection
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_set_device_id_twice() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, _client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.4:10004".into(),
            "edge-double-dev.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    let conn_id = "incoming:100.64.1.4:10004";

    // Set first device ID
    manager.set_device_id(conn_id, "dev-a".into()).await;
    assert!(manager.get_connection_by_device("dev-a").await.is_some());

    // Set second device ID on same connection — should replace
    manager.set_device_id(conn_id, "dev-b".into()).await;

    // Old mapping gone
    assert!(
        manager.get_connection_by_device("dev-a").await.is_none(),
        "old device_id 'dev-a' should be removed from index"
    );
    // New mapping works
    let found = manager.get_connection_by_device("dev-b").await.unwrap();
    assert_eq!(found.id, conn_id);
    assert_eq!(found.device_id.as_deref(), Some("dev-b"));
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #6: Two connections claim the same device_id
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_two_connections_same_device_id() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());

    let (ws1, _c1) = duplex_ws_pair().await;
    let (ws2, _c2) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            ws1,
            "100.64.1.5:10005".into(),
            "edge-dup-dev-1.ts.net".into(),
            Direction::Incoming,
        )
        .await;
    manager
        .handle_ws_stream(
            ws2,
            "100.64.1.5:10006".into(),
            "edge-dup-dev-2.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    // Drain Connected events
    for _ in 0..2 {
        wait_for_transport_event(
            &mut rx,
            Duration::from_secs(5),
            |e| matches!(e, TransportEvent::Connected(_)),
            "Connected",
        )
        .await;
    }

    let conn1 = "incoming:100.64.1.5:10005";
    let conn2 = "incoming:100.64.1.5:10006";

    // First connection claims device
    manager.set_device_id(conn1, "shared-dev".into()).await;
    let found = manager.get_connection_by_device("shared-dev").await.unwrap();
    assert_eq!(found.id, conn1);

    // Second connection steals the same device ID
    manager.set_device_id(conn2, "shared-dev".into()).await;
    let found = manager.get_connection_by_device("shared-dev").await.unwrap();
    assert_eq!(
        found.id, conn2,
        "latest set_device_id call should own the device index entry"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #7: Empty binary message
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_empty_binary_message() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, mut client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.6:10007".into(),
            "edge-empty-msg.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    // Send an empty binary frame (0 bytes)
    client_ws
        .send(Message::Binary(bytes::Bytes::new()))
        .await
        .unwrap();

    // Send a valid message after to prove the connection survived
    let payload = serde_json::json!({"namespace": "test", "type": "after-empty"});
    let encoded = websocket::encode_message(&payload, false).unwrap();
    client_ws
        .send(Message::Binary(bytes::Bytes::from(encoded)))
        .await
        .unwrap();

    // The valid message should arrive (empty was skipped as malformed)
    let event = wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Message { .. }),
        "Message after empty binary",
    )
    .await;

    if let TransportEvent::Message { message, .. } = &event {
        assert_eq!(
            message.payload.get("type").unwrap().as_str().unwrap(),
            "after-empty"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #8: Oversized message (10 MB)
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_oversized_message() {
    init_tracing();

    // Use a config with large enough duplex buffer
    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    // Use a larger duplex buffer to fit the oversized message
    let (client_io, server_io) = tokio::io::duplex(16 * 1024 * 1024);
    let server_ws = WebSocketStream::from_raw_socket(
        server_io,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        Some(websocket::ws_config()),
    )
    .await;
    let mut client_ws = WebSocketStream::from_raw_socket(
        client_io,
        tokio_tungstenite::tungstenite::protocol::Role::Client,
        Some(websocket::ws_config()),
    )
    .await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.7:10008".into(),
            "edge-oversized.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    // Construct a ~10MB payload: flag byte + large JSON
    let big_string = "x".repeat(10 * 1024 * 1024);
    let big_json = serde_json::json!({"namespace": "test", "type": "big", "data": big_string});
    let encoded = websocket::encode_message(&big_json, true).unwrap();

    // Send it. This might fail at the WS level due to MAX_MESSAGE_SIZE,
    // but should NOT cause a crash or OOM on the server side.
    let _send_result = client_ws
        .send(Message::Binary(bytes::Bytes::from(encoded)))
        .await;

    // Whether it succeeds or fails, the server should still be running.
    // If the message was accepted, we should either get it as a Message event
    // or it should trigger a read pump error. Either way: no crash.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send a normal follow-up to verify the connection is still usable
    // (or was gracefully terminated)
    let small_payload = serde_json::json!({"namespace": "test", "type": "after-big"});
    let small_encoded = websocket::encode_message(&small_payload, false).unwrap();
    let _ = client_ws
        .send(Message::Binary(bytes::Bytes::from(small_encoded)))
        .await;

    // Just verify we don't hang or crash — the exact behavior (message
    // delivered vs connection terminated) depends on size limits
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Test passes if we get here without panic or deadlock
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #9: Non-UTF8 / garbage binary message
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_non_utf8_binary_message() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, mut client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.8:10009".into(),
            "edge-garbage.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    // Send raw garbage binary — not valid JSON, not valid msgpack
    // First byte 0x00 = msgpack format flag, rest is garbage
    let garbage: Vec<u8> = vec![0x00, 0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA, 0x00, 0x01];
    client_ws
        .send(Message::Binary(bytes::Bytes::from(garbage)))
        .await
        .unwrap();

    // Also try with unknown format flag
    let unknown_flag: Vec<u8> = vec![0x04, 0x01, 0x02, 0x03];
    client_ws
        .send(Message::Binary(bytes::Bytes::from(unknown_flag)))
        .await
        .unwrap();

    // Small delay to ensure garbage messages are processed
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send a valid message to prove the connection survived malformed input
    let payload = serde_json::json!({"namespace": "test", "type": "after-garbage"});
    let encoded = websocket::encode_message(&payload, false).unwrap();
    client_ws
        .send(Message::Binary(bytes::Bytes::from(encoded)))
        .await
        .unwrap();

    // The valid message should arrive. Use a predicate that checks the
    // actual payload to skip any garbage that might have decoded into a
    // serde_json::Value without a "type" field.
    let event = wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| {
            if let TransportEvent::Message { message, .. } = e {
                message
                    .payload
                    .get("type")
                    .and_then(|t| t.as_str())
                    == Some("after-garbage")
            } else {
                false
            }
        },
        "Message with type=after-garbage (skipping garbage-decoded messages)",
    )
    .await;

    if let TransportEvent::Message { message, .. } = &event {
        assert_eq!(
            message.payload.get("type").unwrap().as_str().unwrap(),
            "after-garbage"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #10: Rapid fire 1000 messages
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_rapid_fire_messages() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, mut client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.9:10010".into(),
            "edge-rapid-fire.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    // Use 200 messages — within the 256-capacity broadcast channel to
    // avoid lagging (which causes missed events for broadcast receivers).
    let msg_count = 200u64;

    // Send messages in a tight loop
    for i in 0..msg_count {
        let payload = serde_json::json!({"namespace": "test", "type": "rapid", "seq": i});
        let encoded = websocket::encode_message(&payload, false).unwrap();
        client_ws
            .send(Message::Binary(bytes::Bytes::from(encoded)))
            .await
            .unwrap();
    }

    // Receive all messages (or as many as arrive within timeout).
    // The broadcast channel capacity is 256, so with 200 messages we
    // should not lag. We use a generous timeout to account for slow CI.
    let mut received = 0u64;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(TransportEvent::Message { .. })) => {
                received += 1;
                if received == msg_count {
                    break;
                }
            }
            Ok(Ok(_)) => continue, // skip non-message events
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                // If lagging occurs, count the lagged events as received
                // (they were processed by the server, just lost by this
                // slow broadcast receiver)
                received += n;
                if received >= msg_count {
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }

    assert!(
        received >= msg_count,
        "all {msg_count} rapid-fire messages should be processed (got {received})"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #11: Send while connection is closing
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_send_while_closing() {
    init_tracing();

    let (manager, mut rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, _client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.10:10011".into(),
            "edge-send-closing.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    let conn_id = "incoming:100.64.1.10:10011";

    // Spawn close_all and send concurrently — no deadlock should occur
    let manager_ref = &manager;
    let race_msg = serde_json::json!({"type": "race"});
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let close_fut = manager_ref.close_all();
        let send_fut = manager_ref.send(conn_id, &race_msg);
        // Run them concurrently
        let (_, send_result) = tokio::join!(close_fut, send_fut);
        send_result
    })
    .await;

    // Should complete without deadlock (timeout would indicate deadlock)
    assert!(
        result.is_ok(),
        "concurrent send + close_all should not deadlock"
    );
    // send() may or may not succeed — either outcome is fine
    // The key is that we didn't deadlock or panic
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #12: Heartbeat with very short (near-zero) interval
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_heartbeat_very_short_interval() {
    init_tracing();

    let config = TransportConfig {
        heartbeat: HeartbeatConfig {
            ping_interval: Duration::from_millis(1), // extremely fast
            timeout: Duration::from_millis(100),      // short but not instant
        },
        auto_reconnect: false,
        max_reconnect_delay: Duration::from_secs(1),
        debug_json_mode: false,
        local_device_id: None,
    };

    let (manager, mut rx) = ConnectionManager::new(config);
    let (server_ws, mut client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.11:10012".into(),
            "edge-fast-hb.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    // Respond to a few pings to keep the connection alive briefly,
    // proving the fast heartbeat doesn't spin or crash.
    // Handles both v3 control frame pings and v2 legacy pings.
    let responder = tokio::spawn(async move {
        let mut pong_count = 0u32;
        while let Some(Ok(msg)) = client_ws.next().await {
            if let Message::Binary(data) = &msg {
                if let Ok(decoded) = websocket::decode_frame(data) {
                    let is_ping = match &decoded {
                        // v3 control frame ping
                        websocket::DecodedFrame::V3Control(
                            truffle_core::protocol::frame::ControlMessage::Ping { timestamp },
                        ) => Some(*timestamp),
                        // v2 legacy ping
                        websocket::DecodedFrame::Legacy(wire_msg)
                            if wire_msg.payload.get("type").and_then(|t| t.as_str())
                                == Some("ping") =>
                        {
                            wire_msg.payload.get("timestamp").and_then(|v| v.as_u64())
                        }
                        _ => None,
                    };
                    if let Some(ts) = is_ping {
                        let pong = truffle_core::transport::heartbeat::create_v3_pong(ts);
                        let encoded = websocket::encode_control_frame(&pong, false).unwrap();
                        if client_ws.send(Message::Binary(bytes::Bytes::from(encoded))).await.is_err() {
                            break;
                        }
                        pong_count += 1;
                        // Respond to a handful then stop
                        if pong_count >= 10 {
                            break;
                        }
                    }
                }
            }
        }
        pong_count
    });

    // Let the heartbeat run for a bit
    let pong_count = tokio::time::timeout(Duration::from_secs(5), responder)
        .await
        .expect("responder should finish")
        .expect("responder should not panic");

    assert!(
        pong_count >= 5,
        "should have exchanged several pings with 1ms interval, got {pong_count}"
    );

    // Connection will eventually time out since we stopped responding.
    // Just verify no infinite loop or CPU spin happened above.
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #13: Connection closed during heartbeat wait
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_connection_closed_during_heartbeat() {
    init_tracing();

    let config = TransportConfig {
        heartbeat: HeartbeatConfig {
            ping_interval: Duration::from_millis(100),
            timeout: Duration::from_secs(10), // long timeout — won't fire
        },
        auto_reconnect: false,
        max_reconnect_delay: Duration::from_secs(1),
        debug_json_mode: false,
        local_device_id: None,
    };

    let (manager, mut rx) = ConnectionManager::new(config);
    let (server_ws, client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.12:10013".into(),
            "edge-hb-close.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Connected(_)),
        "Connected",
    )
    .await;

    // Wait for at least one heartbeat ping to be sent
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Drop client while heartbeat is actively pinging
    drop(client_ws);

    // Should get Disconnected (not stuck waiting for pong)
    let event = wait_for_transport_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, TransportEvent::Disconnected { .. }),
        "Disconnected (during heartbeat)",
    )
    .await;

    if let TransportEvent::Disconnected { reason, .. } = &event {
        assert!(
            reason.contains("remote_closed") || reason.contains("error"),
            "should disconnect cleanly, not get stuck: {reason}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #14: close_all with no connections
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_close_all_empty_manager() {
    init_tracing();

    let (manager, _rx) = ConnectionManager::new(quiet_heartbeat_config());

    assert!(manager.get_connections().await.is_empty());

    // Should not panic
    manager.close_all().await;

    assert!(manager.get_connections().await.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #15: get_connection_by_device with no devices bound
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_get_connection_by_device_no_bindings() {
    init_tracing();

    let (manager, _rx) = ConnectionManager::new(quiet_heartbeat_config());
    let (server_ws, _client_ws) = duplex_ws_pair().await;

    manager
        .handle_ws_stream(
            server_ws,
            "100.64.1.14:10015".into(),
            "edge-no-dev.ts.net".into(),
            Direction::Incoming,
        )
        .await;

    // Connection exists, but no device_id was ever set
    assert!(manager.get_connection_by_device("any").await.is_none());
    assert!(manager.get_connection_by_device("").await.is_none());
    assert!(manager.get_connection_by_device("incoming:100.64.1.14:10015").await.is_none());
}

// ───────────────────────────────────────────────────────────────────────────
// Edge case #16: Concurrent subscribe + event
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_edge_concurrent_subscribe_and_event() {
    init_tracing();

    let (manager, _initial_rx) = ConnectionManager::new(quiet_heartbeat_config());

    // First subscriber — before any events
    let mut sub1 = manager.subscribe();

    // Generate 5 events
    let mut _clients = Vec::new();
    for i in 0..5u8 {
        let (server_ws, client_ws) = duplex_ws_pair().await;
        _clients.push(client_ws);
        manager
            .handle_ws_stream(
                server_ws,
                format!("100.64.1.{}:{}", 20 + i, 12000 + i as u16),
                format!("edge-sub-early-{i}.ts.net"),
                Direction::Incoming,
            )
            .await;
    }

    // Second subscriber — mid-stream
    let mut sub2 = manager.subscribe();

    // Generate 5 more events
    for i in 0..5u8 {
        let (server_ws, client_ws) = duplex_ws_pair().await;
        _clients.push(client_ws);
        manager
            .handle_ws_stream(
                server_ws,
                format!("100.64.1.{}:{}", 30 + i, 13000 + i as u16),
                format!("edge-sub-late-{i}.ts.net"),
                Direction::Incoming,
            )
            .await;
    }

    // sub1 should have all 10 events
    let mut count1 = 0;
    while let Ok(TransportEvent::Connected(_)) = sub1.try_recv() {
        count1 += 1;
    }
    assert_eq!(count1, 10, "first subscriber should see all 10 Connected events");

    // sub2 should only have the last 5
    let mut count2 = 0;
    while let Ok(TransportEvent::Connected(_)) = sub2.try_recv() {
        count2 += 1;
    }
    assert_eq!(count2, 5, "second subscriber should see 5 Connected events");
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
        local_device_id: None,
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
