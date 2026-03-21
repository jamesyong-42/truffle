//! # Mesh Layer Integration Tests (Layer 3)
//!
//! These tests validate the **mesh networking layer** in truffle:
//! device discovery, primary election, STAR routing, and message delivery.
//!
//! ## What's proven at this layer
//!
//! 1. **Single-node mesh lifecycle** — MeshNode start/stop cycle emits correct
//!    events, auth state transitions work, and a lone node self-elects as primary
//!    after the discovery timeout.
//!
//! 2. **Two-node discovery** — Two TruffleRuntime nodes on the same Tailscale
//!    tailnet discover each other via device:announce messages after connection.
//!
//! 3. **Primary election** — With two nodes, exactly one becomes Primary and the
//!    other Secondary. Election follows the rules: user-designated > uptime >
//!    lexicographic device ID.
//!
//! 4. **Message routing** — Broadcast and targeted messages are delivered through
//!    the STAR topology (secondary -> primary -> secondary).
//!
//! 5. **Graceful shutdown** — When a node stops, it broadcasts device:goodbye and
//!    peers mark it offline. If the primary stops, the secondary undergoes failover.
//!
//! ## What's NOT tested at this layer
//!
//! - **Bridge protocol** — Covered by sidecar_lifecycle tests (Layer 1-2).
//! - **HTTP routing / PWA** — Covered by http module unit tests.
//! - **Store sync / file transfer** — Application-layer features above mesh.
//!
//! ## Running the tests
//!
//! ```bash
//! # Single-node tests (no Tailscale needed):
//! cargo test --test mesh_integration
//!
//! # Two-node tests (require Tailscale auth, run sequentially):
//! cargo test --test mesh_integration -- --ignored --test-threads=1 --nocapture
//!
//! # Run sidecar_lifecycle auth tests first if auth isn't cached:
//! cargo test --test sidecar_lifecycle sidecar_auth_flow_completes -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;

use tokio::sync::broadcast;

use truffle_core::mesh::node::{MeshNode, MeshNodeConfig, MeshNodeEvent, MeshTimingConfig};
use truffle_core::protocol::envelope::MeshEnvelope;
use truffle_core::runtime::{TruffleEvent, TruffleRuntime};
use truffle_core::transport::connection::{ConnectionManager, TransportConfig};

static INIT: Once = Once::new();

fn init_tracing() {
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("truffle_core=debug".parse().unwrap())
                    .add_directive("go_shim=debug".parse().unwrap()),
            )
            .with_test_writer()
            .try_init()
            .ok();
    });
}

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

// ═══════════════════════════════════════════════════════════════════════════
// Helper: wait for a MeshNodeEvent with predicate
// ═══════════════════════════════════════════════════════════════════════════

async fn wait_for_mesh_event<F>(
    rx: &mut broadcast::Receiver<MeshNodeEvent>,
    timeout: Duration,
    predicate: F,
    description: &str,
) -> MeshNodeEvent
where
    F: Fn(&MeshNodeEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for MeshNodeEvent: {description}");
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
                panic!("MeshNodeEvent channel closed while waiting for: {description}");
            }
            Err(_) => {
                panic!("timed out waiting for MeshNodeEvent: {description}");
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helper: wait for a TruffleEvent with predicate
// ═══════════════════════════════════════════════════════════════════════════

async fn wait_for_truffle_event<F>(
    rx: &mut broadcast::Receiver<TruffleEvent>,
    timeout: Duration,
    predicate: F,
    description: &str,
) -> TruffleEvent
where
    F: Fn(&TruffleEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for TruffleEvent: {description}");
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
                panic!("TruffleEvent channel closed while waiting for: {description}");
            }
            Err(_) => {
                panic!("timed out waiting for TruffleEvent: {description}");
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helper: create a MeshNode config with fast timings for tests
// ═══════════════════════════════════════════════════════════════════════════

fn fast_mesh_config(device_id: &str, device_name: &str) -> MeshNodeConfig {
    MeshNodeConfig {
        device_id: device_id.to_string(),
        device_name: device_name.to_string(),
        device_type: "desktop".to_string(),
        hostname_prefix: "test".to_string(),
        capabilities: vec![],
        metadata: None,
        timing: MeshTimingConfig {
            announce_interval: Duration::from_secs(5),
            discovery_timeout: Duration::from_millis(500),
        },
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helper: spawn a full TruffleRuntime node (for two-node tests)
// ═══════════════════════════════════════════════════════════════════════════

struct RuntimeNode {
    runtime: TruffleRuntime,
    truffle_rx: broadcast::Receiver<TruffleEvent>,
}

async fn spawn_runtime_node(name: &str) -> RuntimeNode {
    let sidecar_path = sidecar_binary_path();
    // Use mesh-specific state dirs to avoid TLS cert conflicts with
    // sidecar_lifecycle tests (which use different hostnames).
    let state_dir = shared_authed_state_dir(&format!("mesh-{name}"));

    // CRITICAL: device_id must be STABLE across runs because it's part of the
    // Tailscale hostname ("test-desktop-{device_id}"). If it changes, the TLS
    // cert (cached from the previous run) won't match and dials fail with:
    //   "getCertPEM: invalid domain ... must be one of [old hostname]"
    //
    // Hostname prefix "test" is shared so the peer filter matches all test nodes.
    let stable_device_id = format!("mesh-integ-{name}");
    let (runtime, _mesh_rx) = TruffleRuntime::builder()
        .hostname("test")
        .device_id(stable_device_id)
        .device_name(format!("Test Node {name}"))
        .state_dir(state_dir)
        .sidecar_path(sidecar_path)
        .auth_key(std::env::var("TS_AUTHKEY").ok().unwrap_or_default())
        .announce_interval(Duration::from_secs(5))
        .discovery_timeout(Duration::from_secs(2))
        .build()
        .expect("failed to build TruffleRuntime");

    let mut truffle_rx = runtime.start().await.expect("failed to start TruffleRuntime");

    // Wait for either Online or AuthRequired
    let event = wait_for_truffle_event(
        &mut truffle_rx,
        Duration::from_secs(45),
        |e| {
            matches!(e, TruffleEvent::Online { .. })
                || matches!(e, TruffleEvent::AuthRequired { .. })
        },
        &format!("Node {name}: Online or AuthRequired"),
    )
    .await;

    match &event {
        TruffleEvent::AuthRequired { auth_url } => {
            eprintln!("\n======================================================");
            eprintln!("  Auth needed for node '{name}':");
            eprintln!("  {auth_url}");
            eprintln!("======================================================\n");

            wait_for_truffle_event(
                &mut truffle_rx,
                Duration::from_secs(120),
                |e| matches!(e, TruffleEvent::Online { .. }),
                &format!("Node {name}: Online after auth"),
            )
            .await;
            eprintln!("Node {name}: online after auth");
        }
        TruffleEvent::Online { ip, dns_name } => {
            eprintln!("Node {name}: online (cached auth) ip={ip}, dns={dns_name}");
        }
        _ => panic!("unexpected event: {event:?}"),
    }

    RuntimeNode { runtime, truffle_rx }
}

// ═══════════════════════════════════════════════════════════════════════════
// 3a. Single-node mesh tests (no Tailscale)
// ═══════════════════════════════════════════════════════════════════════════

/// A lone MeshNode with no peers starts and handles an empty peer list.
///
/// Election system has been removed (RFC 010). This test verifies the node
/// starts correctly and handles empty peer discovery without crashing.
#[tokio::test]
async fn test_single_node_handles_empty_peers() {
    init_tracing();

    let config = fast_mesh_config("solo-node-1", "Solo Node");
    let transport_config = TransportConfig::default();
    let (conn_mgr, _transport_rx) = ConnectionManager::new(transport_config);
    let conn_mgr = Arc::new(conn_mgr);

    let (node, mut event_rx) = MeshNode::new(config, conn_mgr);

    node.start().await;

    // Consume Started event
    let started = wait_for_mesh_event(
        &mut event_rx,
        Duration::from_secs(5),
        |e| matches!(e, MeshNodeEvent::Started),
        "Started",
    )
    .await;
    assert!(matches!(started, MeshNodeEvent::Started));

    // Simulate: sidecar reported no relevant peers
    node.handle_tailnet_peers(&[]).await;

    // Node should be running without issues
    assert!(node.is_running().await);

    node.stop().await;
}

/// Start -> Started, Stop -> Stopped, Start again -> Started.
/// Proves the mesh node supports a full restart cycle.
#[tokio::test]
async fn test_mesh_node_start_stop_cycle() {
    init_tracing();

    let config = fast_mesh_config("cycle-node-1", "Cycle Node");
    let transport_config = TransportConfig::default();
    let (conn_mgr, _transport_rx) = ConnectionManager::new(transport_config);
    let conn_mgr = Arc::new(conn_mgr);

    let (node, mut event_rx) = MeshNode::new(config, conn_mgr);

    // First start
    node.start().await;
    let event = wait_for_mesh_event(
        &mut event_rx,
        Duration::from_secs(5),
        |e| matches!(e, MeshNodeEvent::Started),
        "Started (first)",
    )
    .await;
    assert!(matches!(event, MeshNodeEvent::Started));
    assert!(node.is_running().await);

    // Stop
    node.stop().await;
    let event = wait_for_mesh_event(
        &mut event_rx,
        Duration::from_secs(5),
        |e| matches!(e, MeshNodeEvent::Stopped),
        "Stopped",
    )
    .await;
    assert!(matches!(event, MeshNodeEvent::Stopped));
    assert!(!node.is_running().await);

    // Second start
    node.start().await;
    let event = wait_for_mesh_event(
        &mut event_rx,
        Duration::from_secs(5),
        |e| matches!(e, MeshNodeEvent::Started),
        "Started (second)",
    )
    .await;
    assert!(matches!(event, MeshNodeEvent::Started));
    assert!(node.is_running().await);

    node.stop().await;
}

/// set_auth_required(url) emits AuthRequired, set_auth_authenticated() emits AuthComplete.
#[tokio::test]
async fn test_mesh_node_emits_auth_events() {
    init_tracing();

    let config = fast_mesh_config("auth-node-1", "Auth Node");
    let transport_config = TransportConfig::default();
    let (conn_mgr, _transport_rx) = ConnectionManager::new(transport_config);
    let conn_mgr = Arc::new(conn_mgr);

    let (node, mut event_rx) = MeshNode::new(config, conn_mgr);

    node.start().await;
    wait_for_mesh_event(
        &mut event_rx,
        Duration::from_secs(5),
        |e| matches!(e, MeshNodeEvent::Started),
        "Started",
    )
    .await;

    // Simulate auth required
    let test_url = "https://login.tailscale.com/a/test123";
    node.set_auth_required(test_url).await;

    let event = wait_for_mesh_event(
        &mut event_rx,
        Duration::from_secs(5),
        |e| matches!(e, MeshNodeEvent::AuthRequired(_)),
        "AuthRequired",
    )
    .await;
    match event {
        MeshNodeEvent::AuthRequired(url) => {
            assert_eq!(url, test_url);
        }
        other => panic!("Expected AuthRequired, got: {other:?}"),
    }

    // Simulate auth completed
    node.set_auth_authenticated().await;

    let event = wait_for_mesh_event(
        &mut event_rx,
        Duration::from_secs(5),
        |e| matches!(e, MeshNodeEvent::AuthComplete),
        "AuthComplete",
    )
    .await;
    assert!(matches!(event, MeshNodeEvent::AuthComplete));

    // Calling set_auth_authenticated again should be idempotent (no duplicate event)
    node.set_auth_authenticated().await;
    // Give it a moment to process
    tokio::time::sleep(Duration::from_millis(100)).await;
    // The channel should have no new AuthComplete events
    match event_rx.try_recv() {
        Ok(MeshNodeEvent::AuthComplete) => panic!("set_auth_authenticated should be idempotent"),
        _ => {} // Expected: either no event or a different event
    }

    node.stop().await;
}

// ═══════════════════════════════════════════════════════════════════════════
// 3b. Two-node mesh tests (require Tailscale)
// ═══════════════════════════════════════════════════════════════════════════

/// Spawn 2 full TruffleRuntime nodes. After connection, both should emit
/// PeerDiscovered for the other node.
#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_two_nodes_discover_each_other() {
    init_tracing();

    eprintln!("=== Spawning Node B ===");
    let mut node_b = spawn_runtime_node("node-b").await;

    eprintln!("=== Spawning Node A ===");
    let mut node_a = spawn_runtime_node("node-a").await;

    // Wait for Node A to discover Node B
    let event_a = wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::PeerDiscovered(_)),
        "Node A: PeerDiscovered",
    )
    .await;

    if let TruffleEvent::PeerDiscovered(device) = &event_a {
        eprintln!("Node A discovered: {} ({})", device.name, device.id);
        assert!(!device.id.is_empty());
    }

    // Wait for Node B to discover Node A
    let event_b = wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::PeerDiscovered(_)),
        "Node B: PeerDiscovered",
    )
    .await;

    if let TruffleEvent::PeerDiscovered(device) = &event_b {
        eprintln!("Node B discovered: {} ({})", device.name, device.id);
        assert!(!device.id.is_empty());
    }

    eprintln!("=== Two-node discovery test PASSED ===");

    node_a.runtime.stop().await;
    node_b.runtime.stop().await;
}

// test_two_nodes_elect_primary removed -- election system deleted (RFC 010)

/// After connection, Node A should have Node B in its device list and vice versa.
#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_two_nodes_exchange_device_announce() {
    init_tracing();

    eprintln!("=== Spawning Node B ===");
    let mut node_b = spawn_runtime_node("node-b").await;

    eprintln!("=== Spawning Node A ===");
    let mut node_a = spawn_runtime_node("node-a").await;

    // Wait for discovery events on both sides
    wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::PeerDiscovered(_) | TruffleEvent::PeersChanged(_)),
        "Node A: peer discovered/changed",
    )
    .await;

    wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::PeerDiscovered(_) | TruffleEvent::PeersChanged(_)),
        "Node B: peer discovered/changed",
    )
    .await;

    // Give a moment for announce exchange to propagate
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check device lists
    let a_devices = node_a.runtime.devices().await;
    let b_devices = node_b.runtime.devices().await;

    let b_device_id = node_b.runtime.device_id().await;
    let a_device_id = node_a.runtime.device_id().await;

    eprintln!("Node A sees {} devices: {:?}", a_devices.len(),
        a_devices.iter().map(|d| &d.id).collect::<Vec<_>>());
    eprintln!("Node B sees {} devices: {:?}", b_devices.len(),
        b_devices.iter().map(|d| &d.id).collect::<Vec<_>>());

    assert!(
        a_devices.iter().any(|d| d.id == b_device_id),
        "Node A should have Node B in its device list"
    );
    assert!(
        b_devices.iter().any(|d| d.id == a_device_id),
        "Node B should have Node A in its device list"
    );

    eprintln!("=== Device announce exchange test PASSED ===");

    node_a.runtime.stop().await;
    node_b.runtime.stop().await;
}

// test_primary_sends_device_list_on_connect removed -- election system deleted (RFC 010)

/// Node A stops. Node B should receive PeerOffline event for Node A.
#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_node_stop_triggers_goodbye() {
    init_tracing();

    eprintln!("=== Spawning Node B ===");
    let mut node_b = spawn_runtime_node("node-b").await;

    eprintln!("=== Spawning Node A ===");
    let mut node_a = spawn_runtime_node("node-a").await;

    // Wait for both to discover each other
    wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::PeerDiscovered(_)),
        "Node A: PeerDiscovered",
    )
    .await;

    wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::PeerDiscovered(_)),
        "Node B: PeerDiscovered",
    )
    .await;

    // Stop Node A
    let id = node_a.runtime.device_id().await;
    eprintln!("Stopping Node A (id={id})");
    node_a.runtime.stop().await;

    // Node B should see PeerOffline
    let event = wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(15),
        |e| matches!(e, TruffleEvent::PeerOffline(_)),
        "Node B: PeerOffline after goodbye",
    )
    .await;

    if let TruffleEvent::PeerOffline(offline_id) = &event {
        eprintln!("Device went offline: {offline_id}");
    }

    eprintln!("=== Goodbye test PASSED ===");

    node_b.runtime.stop().await;
}

// test_primary_failover removed -- election system deleted (RFC 010)

/// Node A broadcasts a message on a custom namespace.
/// Node B should receive it as TruffleEvent::Message.
#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_broadcast_message_reaches_all_nodes() {
    init_tracing();

    eprintln!("=== Spawning Node B ===");
    let mut node_b = spawn_runtime_node("node-b").await;

    eprintln!("=== Spawning Node A ===");
    let mut node_a = spawn_runtime_node("node-a").await;

    // Wait for discovery
    wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::PeerDiscovered(_)),
        "Node A: PeerDiscovered",
    )
    .await;

    wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::PeerDiscovered(_)),
        "Node B: PeerDiscovered",
    )
    .await;

    // Give connections time to stabilize
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Node A broadcasts a custom message
    let envelope = MeshEnvelope::new(
        "test-ns",
        "broadcast-test",
        serde_json::json!({"msg": "hello from node-a"}),
    );

    node_a.runtime.broadcast_envelope(&envelope).await;

    // Node B should receive it as a Message event
    let event = wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(15),
        |e| {
            matches!(e, TruffleEvent::Message(msg) if msg.namespace == "test-ns")
        },
        "Node B: Message in test-ns namespace",
    )
    .await;

    if let TruffleEvent::Message(msg) = &event {
        assert_eq!(msg.namespace, "test-ns");
        assert_eq!(msg.msg_type, "broadcast-test");
        assert_eq!(msg.payload["msg"], "hello from node-a");
        eprintln!("Broadcast message received: {:?}", msg.payload);
    }

    eprintln!("=== Broadcast message test PASSED ===");

    node_a.runtime.stop().await;
    node_b.runtime.stop().await;
}

/// Node A sends a message to Node B's device_id. Node B receives it.
/// Node A does NOT receive its own message.
#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_send_targeted_message() {
    init_tracing();

    eprintln!("=== Spawning Node B ===");
    let mut node_b = spawn_runtime_node("node-b").await;

    eprintln!("=== Spawning Node A ===");
    let mut node_a = spawn_runtime_node("node-a").await;

    // Wait for discovery
    wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::PeerDiscovered(_)),
        "Node A: PeerDiscovered",
    )
    .await;

    wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::PeerDiscovered(_)),
        "Node B: PeerDiscovered",
    )
    .await;

    // Give connections time to stabilize
    tokio::time::sleep(Duration::from_secs(2)).await;

    let b_device_id = node_b.runtime.device_id().await;
    eprintln!("Sending targeted message to Node B (id={})", b_device_id);

    let envelope = MeshEnvelope::new(
        "test-targeted",
        "direct",
        serde_json::json!({"for": "node-b-only"}),
    );

    let sent = node_a.runtime.send_envelope(&b_device_id, &envelope).await;
    eprintln!("send_envelope result: {sent}");

    // Node B should receive it
    let event = wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(15),
        |e| {
            matches!(e, TruffleEvent::Message(msg) if msg.namespace == "test-targeted")
        },
        "Node B: targeted Message",
    )
    .await;

    if let TruffleEvent::Message(msg) = &event {
        assert_eq!(msg.namespace, "test-targeted");
        assert_eq!(msg.msg_type, "direct");
        assert_eq!(msg.payload["for"], "node-b-only");
        eprintln!("Node B received targeted message: {:?}", msg.payload);
    }

    // Node A should NOT receive the message (give a brief window to check)
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut a_got_targeted = false;
    loop {
        match node_a.truffle_rx.try_recv() {
            Ok(TruffleEvent::Message(msg)) if msg.namespace == "test-targeted" => {
                a_got_targeted = true;
                break;
            }
            Ok(_) => continue, // Other events are fine
            Err(_) => break,   // No more events
        }
    }
    assert!(
        !a_got_targeted,
        "Node A should NOT receive a message targeted at Node B"
    );

    eprintln!("=== Targeted message test PASSED ===");

    node_a.runtime.stop().await;
    node_b.runtime.stop().await;
}
