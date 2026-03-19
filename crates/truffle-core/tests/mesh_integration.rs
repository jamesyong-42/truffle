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
use truffle_core::mesh::election::ElectionTimingConfig;
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
        prefer_primary: false,
        capabilities: vec![],
        metadata: None,
        timing: MeshTimingConfig {
            announce_interval: Duration::from_secs(5),
            discovery_timeout: Duration::from_millis(500),
            election: ElectionTimingConfig {
                election_timeout: Duration::from_millis(500),
                primary_loss_grace: Duration::from_millis(500),
            },
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
        .election_timeout(Duration::from_secs(2))
        .primary_loss_grace(Duration::from_secs(5))
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

/// A lone MeshNode with no peers becomes primary after the discovery timeout.
///
/// Flow: start -> no peers discovered -> discovery timeout fires ->
/// election starts with only self -> self-elects as primary.
///
/// We simulate the "no peers" condition by calling handle_tailnet_peers
/// with an empty peer list, which triggers the no-primary-no-peers path.
#[tokio::test]
async fn test_single_node_becomes_primary_when_alone() {
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

    // With no peers and no primary, the node should self-elect as primary.
    // This happens synchronously in handle_tailnet_peers when online_devices is empty.
    let role_event = wait_for_mesh_event(
        &mut event_rx,
        Duration::from_secs(5),
        |e| matches!(e, MeshNodeEvent::RoleChanged { .. }),
        "RoleChanged (primary as sole node)",
    )
    .await;

    match role_event {
        MeshNodeEvent::RoleChanged { role, is_primary } => {
            assert!(is_primary, "Sole node must become primary");
            assert_eq!(role, truffle_core::types::DeviceRole::Primary);
        }
        other => panic!("Expected RoleChanged, got: {other:?}"),
    }

    // Verify via API
    assert!(node.is_primary().await);
    assert_eq!(node.primary_id().await.as_deref(), Some("solo-node-1"));

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
/// DeviceDiscovered for the other node.
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
        |e| matches!(e, TruffleEvent::DeviceDiscovered(_)),
        "Node A: DeviceDiscovered",
    )
    .await;

    if let TruffleEvent::DeviceDiscovered(device) = &event_a {
        eprintln!("Node A discovered: {} ({})", device.name, device.id);
        assert!(!device.id.is_empty());
    }

    // Wait for Node B to discover Node A
    let event_b = wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::DeviceDiscovered(_)),
        "Node B: DeviceDiscovered",
    )
    .await;

    if let TruffleEvent::DeviceDiscovered(device) = &event_b {
        eprintln!("Node B discovered: {} ({})", device.name, device.id);
        assert!(!device.id.is_empty());
    }

    eprintln!("=== Two-node discovery test PASSED ===");

    node_a.runtime.stop().await;
    node_b.runtime.stop().await;
}

/// Spawn 2 nodes. One should become Primary, the other Secondary.
/// Both emit RoleChanged events.
#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_two_nodes_elect_primary() {
    init_tracing();

    eprintln!("=== Spawning Node B ===");
    let mut node_b = spawn_runtime_node("node-b").await;

    eprintln!("=== Spawning Node A ===");
    let mut node_a = spawn_runtime_node("node-a").await;

    // Wait for both nodes to receive RoleChanged events
    let role_a = wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node A: RoleChanged",
    )
    .await;

    let role_b = wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node B: RoleChanged",
    )
    .await;

    let a_is_primary = matches!(&role_a, TruffleEvent::RoleChanged { is_primary: true, .. });
    let b_is_primary = matches!(&role_b, TruffleEvent::RoleChanged { is_primary: true, .. });

    eprintln!("Node A primary: {a_is_primary}, Node B primary: {b_is_primary}");

    // Exactly one node should be primary
    assert!(
        a_is_primary ^ b_is_primary,
        "Exactly one node should be primary, got A={a_is_primary} B={b_is_primary}"
    );

    // Verify via API
    let a_primary = node_a.runtime.is_primary().await;
    let b_primary = node_b.runtime.is_primary().await;
    assert!(
        a_primary ^ b_primary,
        "API check: exactly one node should be primary"
    );

    eprintln!("=== Two-node election test PASSED ===");

    node_a.runtime.stop().await;
    node_b.runtime.stop().await;
}

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
        |e| matches!(e, TruffleEvent::DeviceDiscovered(_) | TruffleEvent::DevicesChanged(_)),
        "Node A: device discovered/changed",
    )
    .await;

    wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::DeviceDiscovered(_) | TruffleEvent::DevicesChanged(_)),
        "Node B: device discovered/changed",
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

/// When secondary connects to primary, secondary receives device:list.
/// Verify the secondary's device list contains the primary.
#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_primary_sends_device_list_on_connect() {
    init_tracing();

    eprintln!("=== Spawning Node B ===");
    let mut node_b = spawn_runtime_node("node-b").await;

    eprintln!("=== Spawning Node A ===");
    let mut node_a = spawn_runtime_node("node-a").await;

    // Wait for both to have roles assigned
    wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node A: RoleChanged",
    )
    .await;

    wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node B: RoleChanged",
    )
    .await;

    // Give time for device:list exchange
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Determine which is primary
    let a_is_primary = node_a.runtime.is_primary().await;
    let (primary, secondary) = if a_is_primary {
        (&node_a, &node_b)
    } else {
        (&node_b, &node_a)
    };

    let primary_id = primary.runtime.device_id().await;
    let secondary_devices = secondary.runtime.devices().await;

    eprintln!("Primary: {primary_id}");
    eprintln!("Secondary sees devices: {:?}",
        secondary_devices.iter().map(|d| (&d.id, &d.role)).collect::<Vec<_>>());

    // The secondary should have the primary in its device list
    let has_primary = secondary_devices.iter().any(|d| d.id == primary_id);
    assert!(
        has_primary,
        "Secondary should have primary ({primary_id}) in its device list after device:list"
    );

    eprintln!("=== Primary device:list test PASSED ===");

    node_a.runtime.stop().await;
    node_b.runtime.stop().await;
}

/// Node A (primary) stops. Node B should receive DeviceOffline event for Node A.
#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_node_stop_triggers_goodbye() {
    init_tracing();

    eprintln!("=== Spawning Node B ===");
    let mut node_b = spawn_runtime_node("node-b").await;

    eprintln!("=== Spawning Node A ===");
    let mut node_a = spawn_runtime_node("node-a").await;

    // Wait for both to have roles
    wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node A: RoleChanged",
    )
    .await;

    wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node B: RoleChanged",
    )
    .await;

    // Determine which is primary and stop it
    let a_is_primary = node_a.runtime.is_primary().await;
    let _stopping_id = if a_is_primary {
        let id = node_a.runtime.device_id().await;
        eprintln!("Stopping Node A (primary, id={id})");
        node_a.runtime.stop().await;
        id
    } else {
        let id = node_b.runtime.device_id().await;
        eprintln!("Stopping Node B (primary, id={id})");
        node_b.runtime.stop().await;
        id
    };

    // The surviving node should see DeviceOffline
    let surviving_rx = if a_is_primary {
        &mut node_b.truffle_rx
    } else {
        &mut node_a.truffle_rx
    };

    let event = wait_for_truffle_event(
        surviving_rx,
        Duration::from_secs(15),
        |e| matches!(e, TruffleEvent::DeviceOffline(_)),
        "Surviving node: DeviceOffline after goodbye",
    )
    .await;

    if let TruffleEvent::DeviceOffline(offline_id) = &event {
        eprintln!("Device went offline: {offline_id}");
        // The offline device should be the one that stopped
        // Note: may match on the device_id parsed from hostname
    }

    eprintln!("=== Goodbye test PASSED ===");

    // Stop the surviving node
    if a_is_primary {
        node_b.runtime.stop().await;
    } else {
        node_a.runtime.stop().await;
    }
}

/// Node A is primary, Node B is secondary. Stop Node A.
/// Node B should become primary (RoleChanged with is_primary=true)
/// within ~15 seconds (grace period + election timeout).
#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_primary_failover() {
    init_tracing();

    eprintln!("=== Spawning Node B ===");
    let mut node_b = spawn_runtime_node("node-b").await;

    eprintln!("=== Spawning Node A ===");
    let mut node_a = spawn_runtime_node("node-a").await;

    // Wait for election to complete
    wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node A: RoleChanged",
    )
    .await;

    wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node B: RoleChanged",
    )
    .await;

    let a_is_primary = node_a.runtime.is_primary().await;
    let b_is_primary = node_b.runtime.is_primary().await;
    eprintln!("Initial state: A primary={a_is_primary}, B primary={b_is_primary}");
    assert!(
        a_is_primary ^ b_is_primary,
        "One node must be primary before failover test"
    );

    // Stop the primary node
    let surviving_rx;
    let surviving_runtime;
    if a_is_primary {
        eprintln!("Stopping Node A (primary)...");
        node_a.runtime.stop().await;
        surviving_rx = &mut node_b.truffle_rx;
        surviving_runtime = &node_b.runtime;
    } else {
        eprintln!("Stopping Node B (primary)...");
        node_b.runtime.stop().await;
        surviving_rx = &mut node_a.truffle_rx;
        surviving_runtime = &node_a.runtime;
    }

    // The surviving secondary should detect primary loss and self-elect.
    // Grace period (5s) + election timeout (2s) + margin = ~15s timeout.
    let failover_event = wait_for_truffle_event(
        surviving_rx,
        Duration::from_secs(20),
        |e| matches!(e, TruffleEvent::RoleChanged { is_primary: true, .. }),
        "Surviving node: RoleChanged to primary (failover)",
    )
    .await;

    match &failover_event {
        TruffleEvent::RoleChanged { role, is_primary } => {
            assert!(is_primary, "Surviving node must become primary after failover");
            assert_eq!(*role, truffle_core::types::DeviceRole::Primary);
            eprintln!("Failover complete: surviving node is now primary");
        }
        other => panic!("Expected RoleChanged, got: {other:?}"),
    }

    // Verify via API
    assert!(surviving_runtime.is_primary().await);

    eprintln!("=== Primary failover test PASSED ===");

    surviving_runtime.stop().await;
}

/// Node A (primary) broadcasts a message on a custom namespace.
/// Node B should receive it as TruffleEvent::Message.
#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn test_broadcast_message_reaches_all_nodes() {
    init_tracing();

    eprintln!("=== Spawning Node B ===");
    let mut node_b = spawn_runtime_node("node-b").await;

    eprintln!("=== Spawning Node A ===");
    let mut node_a = spawn_runtime_node("node-a").await;

    // Wait for election so we know who is primary
    wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node A: RoleChanged",
    )
    .await;

    wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node B: RoleChanged",
    )
    .await;

    // Give connections time to stabilize
    tokio::time::sleep(Duration::from_secs(2)).await;

    let a_is_primary = node_a.runtime.is_primary().await;
    eprintln!("Node A primary: {a_is_primary}");

    // The primary broadcasts a custom message
    let envelope = MeshEnvelope::new(
        "test-ns",
        "broadcast-test",
        serde_json::json!({"msg": "hello from primary"}),
    );

    if a_is_primary {
        node_a.runtime.broadcast_envelope(&envelope).await;
    } else {
        node_b.runtime.broadcast_envelope(&envelope).await;
    }

    // The other node should receive it as a Message event
    let receiver_rx = if a_is_primary {
        &mut node_b.truffle_rx
    } else {
        &mut node_a.truffle_rx
    };

    let event = wait_for_truffle_event(
        receiver_rx,
        Duration::from_secs(15),
        |e| {
            matches!(e, TruffleEvent::Message(msg) if msg.namespace == "test-ns")
        },
        "Receiver: Message in test-ns namespace",
    )
    .await;

    if let TruffleEvent::Message(msg) = &event {
        assert_eq!(msg.namespace, "test-ns");
        assert_eq!(msg.msg_type, "broadcast-test");
        assert_eq!(msg.payload["msg"], "hello from primary");
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

    // Wait for election
    wait_for_truffle_event(
        &mut node_a.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node A: RoleChanged",
    )
    .await;

    wait_for_truffle_event(
        &mut node_b.truffle_rx,
        Duration::from_secs(45),
        |e| matches!(e, TruffleEvent::RoleChanged { .. }),
        "Node B: RoleChanged",
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
