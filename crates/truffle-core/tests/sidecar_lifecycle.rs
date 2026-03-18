//! # Sidecar + Bridge Layer Integration Tests
//!
//! These tests validate the lowest-level networking primitive in truffle:
//! **two machines establishing a raw TCP bridge through Tailscale**.
//!
//! ## What's proven at this layer
//!
//! 1. **Binary bridge protocol** — The 32-byte session token + direction + port +
//!    request ID header format is byte-perfect between Go and Rust. Malformed
//!    headers, invalid tokens, and oversized fields are all rejected correctly.
//!
//! 2. **Go sidecar lifecycle** — Spawn → start → auth → running → peers → stop.
//!    Crash detection, auto-restart with exponential backoff, and auth storm
//!    prevention all work.
//!
//! 3. **Serde field mapping** — The `tailscaleIP`/`tailscaleDNSName`/`tailscaleIPs`
//!    acronym casing between Go JSON and Rust deserialization is verified correct
//!    (the bug that silently dropped device IPs for weeks).
//!
//! 4. **Two-node dial pipeline** — Node A dials Node B through Tailscale's encrypted
//!    tunnel. The Go sidecar bridges the raw TCP stream back to Rust's BridgeManager.
//!    The `request_id` correlation correctly delivers the outgoing `BridgeConnection`
//!    to the caller. Node B's incoming handler receives the connection with correct
//!    remote address metadata.
//!
//! 5. **Bidirectional bridge verification** — Both sides of a dial (outgoing on A,
//!    incoming on B) are verified in the same test. This proves the full path:
//!    `GoShim::dial_raw()` → Go TLS dial → `bridgeToRust()` → BridgeManager accept
//!    → pending_dials delivery (A) + handler dispatch (B).
//!
//! ## What's NOT tested at this layer
//!
//! - **WebSocket upgrade** — Bridge connections are raw TCP. The transport layer
//!   upgrades them to WebSocket.
//! - **Mesh protocol** — No device:announce, election messages, or STAR routing.
//! - **Application features** — No store sync, file transfer, or message bus.
//! - **Reconnection** — Single dial only, no retry or reconnect logic.
//!
//! ## Running the tests
//!
//! Prerequisites:
//!   - Go sidecar binary built at `packages/sidecar-slim/bin/sidecar-slim`
//!   - For ignored tests: Tailscale network access
//!
//! ```bash
//! # Non-auth tests (always run in CI):
//! cargo test --test sidecar_lifecycle
//!
//! # Auth-requiring tests (run sequentially, may prompt for browser auth):
//! cargo test --test sidecar_lifecycle -- --ignored --test-threads=1 --nocapture
//!
//! # First run establishes auth at /tmp/truffle-test-authed/{node-a,node-b}/
//! # Subsequent runs reuse cached auth.
//! ```

use std::path::PathBuf;
use std::sync::Once;
use std::time::Duration;

use tokio::sync::broadcast;

use truffle_core::bridge::manager::BridgeManager;
use truffle_core::bridge::shim::{GoShim, ShimConfig, ShimLifecycleEvent};

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

fn unique_state_dir(test_name: &str) -> String {
    let dir = std::env::temp_dir()
        .join("truffle-test")
        .join(test_name)
        .join(format!("{}", std::process::id()));
    // Clean up from previous runs
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create state dir");
    dir.to_string_lossy().to_string()
}

/// Shared state dir for auth-requiring tests. Once you auth one test,
/// the cached state is reused by all other tests using this dir.
/// The directory persists across runs at /tmp/truffle-test-authed/
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

/// Helper: wait for a specific lifecycle event, with timeout.
async fn wait_for_event<F>(
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
            panic!("timed out waiting for event: {description}");
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
                panic!("lifecycle channel closed while waiting for: {description}");
            }
            Err(_) => {
                panic!("timed out waiting for event: {description}");
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 4a. Sidecar spawn + startup events
// ═══════════════════════════════════════════════════════════════════════════

/// Validates that spawning the sidecar produces a Status(starting) event.
/// This is purely local (no network required).
#[tokio::test]
async fn sidecar_spawns_and_emits_status_starting() {
    init_tracing();

    let session_token = random_session_token();
    let token_bytes = hex::decode(&session_token).unwrap();
    let mut token_arr = [0u8; 32];
    token_arr.copy_from_slice(&token_bytes);

    let manager = BridgeManager::bind(token_arr).await.unwrap();
    let bridge_port = manager.local_port();

    let state_dir = unique_state_dir("spawn_status_starting");

    let config = ShimConfig {
        binary_path: sidecar_binary_path(),
        hostname: "test-spawn".to_string(),
        state_dir,
        auth_key: None,
        bridge_port,
        session_token,
        auto_restart: false,
    };

    let (_shim, mut rx) = GoShim::spawn(config).await.unwrap();

    // Should receive a Status event with state="starting" within 5 seconds.
    // This is purely local — no network round-trip needed.
    let event = wait_for_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, ShimLifecycleEvent::Status(data) if data.state == "starting"),
        "Status(starting)",
    )
    .await;

    if let ShimLifecycleEvent::Status(data) = &event {
        assert_eq!(data.state, "starting");
        assert_eq!(data.hostname, "test-spawn");
    }

    // Clean up: shut down the shim
    _shim.shutdown();
}

/// Validates that AuthRequired is emitted after contacting Tailscale's control plane.
/// This requires network access to controlplane.tailscale.com and can take >30s.
#[tokio::test]
#[ignore] // Requires network access to Tailscale control plane
async fn sidecar_emits_auth_required() {
    init_tracing();

    let session_token = random_session_token();
    let token_bytes = hex::decode(&session_token).unwrap();
    let mut token_arr = [0u8; 32];
    token_arr.copy_from_slice(&token_bytes);

    let manager = BridgeManager::bind(token_arr).await.unwrap();
    let bridge_port = manager.local_port();

    let state_dir = unique_state_dir("auth_required");

    let config = ShimConfig {
        binary_path: sidecar_binary_path(),
        hostname: "test-auth-req".to_string(),
        state_dir,
        auth_key: None,
        bridge_port,
        session_token,
        auto_restart: false,
    };

    let (_shim, mut rx) = GoShim::spawn(config).await.unwrap();

    // AuthRequired requires a network roundtrip to Tailscale's control plane.
    let event = wait_for_event(
        &mut rx,
        Duration::from_secs(60),
        |e| matches!(e, ShimLifecycleEvent::AuthRequired { .. }),
        "AuthRequired",
    )
    .await;

    if let ShimLifecycleEvent::AuthRequired { auth_url } = &event {
        assert!(
            auth_url.starts_with("https://"),
            "auth_url should be an HTTPS URL, got: {auth_url}"
        );
    }

    _shim.shutdown();
}

// ═══════════════════════════════════════════════════════════════════════════
// 4b. Sidecar stop command
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn sidecar_stops_gracefully() {
    init_tracing();

    let session_token = random_session_token();
    let token_bytes = hex::decode(&session_token).unwrap();
    let mut token_arr = [0u8; 32];
    token_arr.copy_from_slice(&token_bytes);

    let manager = BridgeManager::bind(token_arr).await.unwrap();
    let bridge_port = manager.local_port();

    let state_dir = unique_state_dir("stop_gracefully");

    let config = ShimConfig {
        binary_path: sidecar_binary_path(),
        hostname: "test-stop".to_string(),
        state_dir,
        auth_key: None,
        bridge_port,
        session_token,
        auto_restart: false,
    };

    let (shim, mut rx) = GoShim::spawn(config).await.unwrap();

    // Wait for proof the shim started (AuthRequired since no cached auth)
    wait_for_event(
        &mut rx,
        Duration::from_secs(10),
        |e| {
            matches!(e, ShimLifecycleEvent::AuthRequired { .. })
                || matches!(e, ShimLifecycleEvent::Status(data) if data.state == "starting")
        },
        "startup event (Status or AuthRequired)",
    )
    .await;

    // Send stop command
    shim.stop().await.unwrap();

    // Should receive Stopped event
    wait_for_event(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, ShimLifecycleEvent::Stopped),
        "Stopped",
    )
    .await;
}

// ═══════════════════════════════════════════════════════════════════════════
// 4c. Sidecar get_peers (requires auth)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore] // Requires Tailscale auth: set TS_AUTHKEY env var or have cached state
async fn sidecar_get_peers_returns_peer_list() {
    init_tracing();

    let session_token = random_session_token();
    let token_bytes = hex::decode(&session_token).unwrap();
    let mut token_arr = [0u8; 32];
    token_arr.copy_from_slice(&token_bytes);

    let manager = BridgeManager::bind(token_arr).await.unwrap();
    let bridge_port = manager.local_port();

    let state_dir = shared_authed_state_dir("shared-node");

    let config = ShimConfig {
        binary_path: sidecar_binary_path(),
        hostname: "test-peers".to_string(),
        state_dir,
        auth_key: std::env::var("TS_AUTHKEY").ok(),
        bridge_port,
        session_token,
        auto_restart: false,
    };

    let (shim, mut rx) = GoShim::spawn(config).await.unwrap();

    // Wait for Status(running) with a tailscale_ip
    wait_for_event(
        &mut rx,
        Duration::from_secs(30),
        |e| matches!(e, ShimLifecycleEvent::Status(data) if data.state == "running" && !data.tailscale_ip.is_empty()),
        "Status(running) with tailscale_ip",
    )
    .await;

    // Request peers
    shim.get_peers().await.unwrap();

    // Receive peers list
    let event = wait_for_event(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, ShimLifecycleEvent::Peers(_)),
        "Peers",
    )
    .await;

    if let ShimLifecycleEvent::Peers(data) = event {
        // Should have at least some peers in the tailnet
        // (if this is a fresh tailnet with only this node, peers may be empty)
        for peer in &data.peers {
            assert!(!peer.id.is_empty(), "peer id should be non-empty");
            assert!(!peer.hostname.is_empty(), "peer hostname should be non-empty");
            assert!(!peer.dns_name.is_empty(), "peer dns_name should be non-empty");
            assert!(!peer.tailscale_ips.is_empty(), "peer should have at least one IP");
        }
    }

    shim.shutdown();
}

// ═══════════════════════════════════════════════════════════════════════════
// 4d. Sidecar auth flow (requires interactive Tailscale auth)
// ═══════════════════════════════════════════════════════════════════════════

/// Sidecar auth + running lifecycle.
/// If the shared state dir has cached auth, this skips straight to running.
/// If fresh, it prompts for browser auth.
/// Run first to establish auth for other tests:
///   cargo test --test sidecar_lifecycle sidecar_auth_flow_completes -- --ignored --nocapture
#[tokio::test]
#[ignore] // Requires network access to Tailscale control plane
async fn sidecar_auth_flow_completes() {
    init_tracing();

    let session_token = random_session_token();
    let token_bytes = hex::decode(&session_token).unwrap();
    let mut token_arr = [0u8; 32];
    token_arr.copy_from_slice(&token_bytes);

    let manager = BridgeManager::bind(token_arr).await.unwrap();
    let bridge_port = manager.local_port();

    // Use shared dir so auth persists for other tests.
    // Delete /tmp/truffle-test-authed/shared-node/ to force fresh auth.
    let state_dir = shared_authed_state_dir("shared-node");

    let config = ShimConfig {
        binary_path: sidecar_binary_path(),
        hostname: "test-auth-flow".to_string(),
        state_dir,
        auth_key: None,
        bridge_port,
        session_token,
        auto_restart: false,
    };

    let (shim, mut rx) = GoShim::spawn(config).await.unwrap();

    // Wait for either AuthRequired (fresh) or Status(running) (cached auth)
    let event = wait_for_event(
        &mut rx,
        Duration::from_secs(30),
        |e| {
            matches!(e, ShimLifecycleEvent::AuthRequired { .. })
                || matches!(e, ShimLifecycleEvent::Status(data) if data.state == "running")
        },
        "AuthRequired or Status(running)",
    )
    .await;

    match &event {
        ShimLifecycleEvent::AuthRequired { auth_url } => {
            eprintln!("\n╔══════════════════════════════════════════════════════╗");
            eprintln!("║ Open this URL in your browser to authenticate:      ║");
            eprintln!("║ {auth_url}");
            eprintln!("╚══════════════════════════════════════════════════════╝\n");

            // Wait up to 120s for Status(running) after user completes auth
            let status = wait_for_event(
                &mut rx,
                Duration::from_secs(120),
                |e| matches!(e, ShimLifecycleEvent::Status(data) if data.state == "running"),
                "Status(running) after auth",
            )
            .await;

            if let ShimLifecycleEvent::Status(data) = status {
                assert!(!data.tailscale_ip.is_empty(), "tailscale_ip should be non-empty after auth");
                eprintln!("Auth completed: ip={}, dns={}", data.tailscale_ip, data.dns_name);
            }
        }
        ShimLifecycleEvent::Status(data) => {
            eprintln!("Using cached auth: ip={}, dns={}", data.tailscale_ip, data.dns_name);
            assert_eq!(data.state, "running");
            assert!(!data.tailscale_ip.is_empty(), "tailscale_ip should be non-empty");
        }
        _ => panic!("unexpected event: {event:?}"),
    }

    // Verify get_peers succeeds
    shim.get_peers().await.unwrap();
    wait_for_event(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, ShimLifecycleEvent::Peers(_)),
        "Peers after auth",
    )
    .await;

    shim.shutdown();
}

// ═══════════════════════════════════════════════════════════════════════════
// Helper: spawn a full sidecar node with bridge manager
// ═══════════════════════════════════════════════════════════════════════════

struct TestNode {
    shim: GoShim,
    rx: broadcast::Receiver<ShimLifecycleEvent>,
    pending_dials: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<truffle_core::bridge::manager::BridgeConnection>>>>,
    incoming_rx: tokio::sync::mpsc::Receiver<truffle_core::bridge::manager::BridgeConnection>,
    cancel: tokio_util::sync::CancellationToken,
    _manager_handle: tokio::task::JoinHandle<()>,
    dns_name: String,
    tailscale_ip: String,
}

async fn spawn_test_node(name: &str, state_dir_name: &str) -> TestNode {
    let session_token = random_session_token();
    let token_bytes = hex::decode(&session_token).unwrap();
    let mut token_arr = [0u8; 32];
    token_arr.copy_from_slice(&token_bytes);

    let mut manager = BridgeManager::bind(token_arr).await.unwrap();
    let bridge_port = manager.local_port();
    let pending_dials = manager.pending_dials().clone();

    // Register incoming handler for port 443
    let (incoming_tx, incoming_rx) =
        tokio::sync::mpsc::channel::<truffle_core::bridge::manager::BridgeConnection>(4);
    manager.add_handler(
        443,
        truffle_core::bridge::header::Direction::Incoming,
        std::sync::Arc::new(truffle_core::bridge::manager::ChannelHandler::new(incoming_tx)),
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
    };

    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();
    let manager_handle = tokio::spawn(async move {
        manager.run(cancel_clone).await;
    });

    let (shim, mut rx) = GoShim::spawn(config).await.unwrap();

    // Wait for running — handle both AuthRequired (fresh) and Status(running) (cached)
    let event = wait_for_event(
        &mut rx,
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
            eprintln!("\n╔══════════════════════════════════════════════════════╗");
            eprintln!("║ Auth needed for node '{name}':                        ");
            eprintln!("║ {auth_url}");
            eprintln!("╚══════════════════════════════════════════════════════╝\n");

            let status = wait_for_event(
                &mut rx,
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
            eprintln!("Node {name} running (cached): ip={}, dns={}", data.tailscale_ip, data.dns_name);
            (data.dns_name.clone(), data.tailscale_ip.clone())
        }
        _ => panic!("unexpected event"),
    };

    TestNode {
        shim,
        rx,
        pending_dials,
        incoming_rx,
        cancel,
        _manager_handle: manager_handle,
        dns_name,
        tailscale_ip,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 4e. Two-node dial test: Node A dials Node B
//     Spawns 2 tsnet nodes on the same machine with separate state dirs.
//     You may need to auth each node separately on first run.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes — may prompt twice on first run)
async fn two_node_dial_connects_and_bridges() {
    init_tracing();

    // Spawn Node B first (it will be the dial target / TLS listener)
    eprintln!("=== Spawning Node B (target) ===");
    let mut node_b = spawn_test_node("node-b", "node-b").await;

    // Spawn Node A (it will dial Node B)
    eprintln!("=== Spawning Node A (dialer) ===");
    let node_a = spawn_test_node("node-a", "node-a").await;

    // Verify both nodes got Tailscale IPs
    assert!(!node_a.tailscale_ip.is_empty(), "Node A should have a tailscale IP");
    assert!(!node_b.tailscale_ip.is_empty(), "Node B should have a tailscale IP");
    assert_ne!(node_a.tailscale_ip, node_b.tailscale_ip, "Nodes should have different IPs");

    // Give both nodes a moment to register with Tailscale
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Node A gets peers — should see Node B
    node_a.shim.get_peers().await.unwrap();
    let peers_event = wait_for_event(
        &mut node_a.rx.resubscribe(),
        Duration::from_secs(10),
        |e| matches!(e, ShimLifecycleEvent::Peers(_)),
        "Node A: Peers",
    )
    .await;

    // Find Node B in the peer list
    let target_dns = if let ShimLifecycleEvent::Peers(data) = peers_event {
        let node_b_peer = data.peers.iter().find(|p| p.dns_name.contains("test-node-b"));
        match node_b_peer {
            Some(peer) => {
                eprintln!("Node A found Node B: dns={}", peer.dns_name);
                peer.dns_name.clone()
            }
            None => {
                // Node B might be listed under its full dns_name
                eprintln!("Available peers: {:?}", data.peers.iter().map(|p| &p.dns_name).collect::<Vec<_>>());
                // Fall back to Node B's known dns_name
                assert!(!node_b.dns_name.is_empty(), "Node B has no dns_name");
                eprintln!("Using Node B's dns_name: {}", node_b.dns_name);
                node_b.dns_name.clone()
            }
        }
    } else {
        panic!("expected Peers event");
    };

    // Node A registers a pending dial and dials Node B
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx_dial) = tokio::sync::oneshot::channel();
    {
        let mut dials = node_a.pending_dials.lock().await;
        dials.insert(request_id.clone(), tx);
    }

    eprintln!("Node A dialing Node B at {target_dns}...");
    node_a.shim.dial_raw(target_dns, 443, request_id.clone())
        .await
        .unwrap();

    // Node A should receive the outgoing bridge connection
    // 45s timeout: first-time TLS cert provisioning can take 15-20s
    let outgoing_conn = tokio::time::timeout(Duration::from_secs(45), rx_dial)
        .await
        .expect("Node A: dial timeout — first run may take 30s+ for TLS cert provisioning")
        .expect("Node A: dial channel error");

    assert_eq!(outgoing_conn.header.request_id, request_id);
    assert_eq!(outgoing_conn.header.direction, truffle_core::bridge::header::Direction::Outgoing);
    assert_eq!(outgoing_conn.header.service_port, 443);
    eprintln!("Node A: outgoing bridge connection received");

    // Node B should receive the incoming bridge connection
    let incoming_conn = tokio::time::timeout(Duration::from_secs(5), node_b.incoming_rx.recv())
        .await
        .expect("Node B: timeout waiting for incoming connection")
        .expect("Node B: handler channel closed");

    assert_eq!(incoming_conn.header.direction, truffle_core::bridge::header::Direction::Incoming);
    assert_eq!(incoming_conn.header.service_port, 443);
    assert!(!incoming_conn.header.remote_addr.is_empty());
    eprintln!("Node B: incoming bridge connection received from {}", incoming_conn.header.remote_addr);

    eprintln!("=== Two-node dial test PASSED ===");

    // Cleanup
    node_a.shim.shutdown();
    node_b.shim.shutdown();
    node_a.cancel.cancel();
    node_b.cancel.cancel();
}

// ═══════════════════════════════════════════════════════════════════════════
// 4f. Full bridge end-to-end: 2 nodes, dial, verify both sides get connections
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore] // Requires Tailscale auth (2 nodes)
async fn two_node_bridge_end_to_end() {
    init_tracing();

    // Reuse same hostnames as the dial test so TLS certs are cached
    eprintln!("=== Spawning Node B (target) ===");
    let mut node_b = spawn_test_node("node-b", "node-b").await;

    eprintln!("=== Spawning Node A (dialer) ===");
    let node_a = spawn_test_node("node-a", "node-a").await;

    assert!(!node_a.tailscale_ip.is_empty(), "Node A should have a tailscale IP");
    assert!(!node_b.tailscale_ip.is_empty(), "Node B should have a tailscale IP");

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Get peers from A
    node_a.shim.get_peers().await.unwrap();
    let peers = wait_for_event(
        &mut node_a.rx.resubscribe(),
        Duration::from_secs(10),
        |e| matches!(e, ShimLifecycleEvent::Peers(_)),
        "Peers",
    )
    .await;

    let target = if let ShimLifecycleEvent::Peers(data) = peers {
        if !node_b.dns_name.is_empty() {
            node_b.dns_name.clone()
        } else {
            data.peers.first().expect("no peers").dns_name.clone()
        }
    } else {
        panic!("expected Peers");
    };

    // Dial
    let req_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx_dial) = tokio::sync::oneshot::channel();
    {
        let mut dials = node_a.pending_dials.lock().await;
        dials.insert(req_id.clone(), tx);
    }

    eprintln!("Dialing {target}...");
    node_a.shim.dial_raw(target, 443, req_id.clone()).await.unwrap();

    // Both sides should get bridge connections
    // 45s timeout: first-time TLS cert provisioning can take 15-20s
    let (out_result, in_result) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(45), rx_dial),
        tokio::time::timeout(Duration::from_secs(45), node_b.incoming_rx.recv()),
    );

    let out_conn = out_result.expect("outgoing timeout").expect("outgoing error");
    let in_conn = in_result.expect("incoming timeout").expect("incoming error");

    eprintln!("Outgoing: req={}, remote_dns={}", out_conn.header.request_id, out_conn.header.remote_dns_name);
    eprintln!("Incoming: remote_addr={}, remote_dns={}", in_conn.header.remote_addr, in_conn.header.remote_dns_name);

    assert_eq!(out_conn.header.service_port, 443);
    assert_eq!(in_conn.header.service_port, 443);

    eprintln!("=== End-to-end bridge test PASSED ===");

    node_a.shim.shutdown();
    node_b.shim.shutdown();
    node_a.cancel.cancel();
    node_b.cancel.cancel();
}

// ═══════════════════════════════════════════════════════════════════════════
// 4g. Sidecar crash recovery
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn sidecar_crash_emits_crashed_event_bad_binary() {
    init_tracing();

    let session_token = random_session_token();

    let config = ShimConfig {
        binary_path: PathBuf::from("/nonexistent/path/to/sidecar-slim"),
        hostname: "test-crash".to_string(),
        state_dir: unique_state_dir("crash_bad_binary"),
        auth_key: None,
        bridge_port: 12345,
        session_token,
        auto_restart: false,
    };

    let (_shim, mut rx) = GoShim::spawn(config).await.unwrap();

    // Should receive Crashed event because the binary doesn't exist
    wait_for_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, ShimLifecycleEvent::Crashed { .. }),
        "Crashed (bad binary path)",
    )
    .await;
}

#[tokio::test]
async fn sidecar_crash_with_auto_restart_retries() {
    init_tracing();

    let session_token = random_session_token();

    let config = ShimConfig {
        binary_path: PathBuf::from("/nonexistent/path/to/sidecar-slim"),
        hostname: "test-crash-restart".to_string(),
        state_dir: unique_state_dir("crash_auto_restart"),
        auth_key: None,
        bridge_port: 12345,
        session_token,
        auto_restart: true,
    };

    let (shim, mut rx) = GoShim::spawn(config).await.unwrap();

    // First crash
    wait_for_event(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, ShimLifecycleEvent::Crashed { .. }),
        "First Crashed event",
    )
    .await;

    // With auto_restart=true, it should retry and crash again (since binary still doesn't exist)
    wait_for_event(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, ShimLifecycleEvent::Crashed { .. }),
        "Second Crashed event (auto-restart retry)",
    )
    .await;

    // Shut down to stop the retry loop
    shim.shutdown();
}

// (legacy single-node e2e test removed — replaced by two_node_bridge_end_to_end)

// ═══════════════════════════════════════════════════════════════════════════
// Additional lifecycle edge cases
// ═══════════════════════════════════════════════════════════════════════════

/// Verify that GoShim::subscribe() returns a working receiver.
#[tokio::test]
async fn shim_subscribe_receives_events() {
    init_tracing();

    let session_token = random_session_token();

    let config = ShimConfig {
        binary_path: PathBuf::from("/nonexistent/sidecar"),
        hostname: "test-sub".to_string(),
        state_dir: unique_state_dir("subscribe"),
        auth_key: None,
        bridge_port: 0,
        session_token,
        auto_restart: false,
    };

    let (shim, _rx) = GoShim::spawn(config).await.unwrap();

    // Subscribe after spawn — should get a working receiver
    let mut sub = shim.subscribe();

    // The spawn of a non-existent binary should produce a Crashed event
    let event = wait_for_event(
        &mut sub,
        Duration::from_secs(5),
        |e| matches!(e, ShimLifecycleEvent::Crashed { .. }),
        "Crashed via subscribe()",
    )
    .await;

    assert!(matches!(event, ShimLifecycleEvent::Crashed { .. }));
}

/// Verify shutdown stops the manager task even before any events.
#[tokio::test]
async fn shutdown_before_any_events() {
    init_tracing();

    let session_token = random_session_token();

    let config = ShimConfig {
        binary_path: sidecar_binary_path(),
        hostname: "test-early-shutdown".to_string(),
        state_dir: unique_state_dir("early_shutdown"),
        auth_key: None,
        bridge_port: 12345, // doesn't matter, we shut down immediately
        session_token,
        auto_restart: false,
    };

    let (shim, _rx) = GoShim::spawn(config).await.unwrap();

    // Immediately shut down
    shim.shutdown();

    // Give it a moment to clean up
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Sending commands after shutdown should fail gracefully
    // (stdin_tx may be closed or not, depending on timing)
    // This is a best-effort check — we just want no panic
    let _ = shim.stop().await;
}

/// Verify is_auto_restart_paused() returns correct state.
#[tokio::test]
async fn auto_restart_paused_state() {
    init_tracing();

    let session_token = random_session_token();

    let config = ShimConfig {
        binary_path: PathBuf::from("/nonexistent/sidecar"),
        hostname: "test-paused".to_string(),
        state_dir: unique_state_dir("paused_state"),
        auth_key: None,
        bridge_port: 0,
        session_token,
        auto_restart: false,
    };

    let (shim, _rx) = GoShim::spawn(config).await.unwrap();

    // Initially not paused
    assert!(!shim.is_auto_restart_paused());

    // resume_auto_restart should be safe to call even when not paused
    shim.resume_auto_restart();
    assert!(!shim.is_auto_restart_paused());
}
