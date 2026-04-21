//! Pair harness for real-network benchmarks.
//!
//! NOTE: this is a slimmed-down copy of the pair-building logic that lives in
//! `truffle-core/tests/common/mod.rs`. A future cleanup (tracked alongside
//! RFC 019) will unify both behind a `test-harness` feature on truffle-core.
//! For now, duplication keeps truffle-bench independent of the test binaries
//! so `cargo run -p truffle-bench` doesn't need dev-dependencies.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::{Node, NodeBuilder};

pub const TEST_APP_ID: &str = "bench-integ";
pub const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(60);

#[allow(dead_code)] // alpha_device_id + device_name fields are handy for future workloads
pub struct BenchPair {
    pub alpha: Arc<Node<TailscaleProvider>>,
    pub beta: Arc<Node<TailscaleProvider>>,
    pub alpha_device_id: String,
    pub beta_device_id: String,
    pub alpha_device_name: String,
    pub beta_device_name: String,
    _alpha_state: tempfile::TempDir,
    _beta_state: tempfile::TempDir,
}

impl BenchPair {
    pub async fn stop(self) {
        self.alpha.stop().await;
        self.beta.stop().await;
    }
}

pub async fn make_bench_pair(authkey: &str) -> Result<BenchPair, String> {
    let sidecar = default_sidecar_path();
    if !sidecar.exists() {
        return Err(format!(
            "test-sidecar not found at {}. Run `cargo build -p truffle-core` to auto-build it \
             (requires Go 1.22+).",
            sidecar.display()
        ));
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let short: String = run_id.chars().take(8).collect();

    let alpha_state =
        tempfile::TempDir::with_prefix("truffle-bench-alpha-").map_err(|e| e.to_string())?;
    let beta_state =
        tempfile::TempDir::with_prefix("truffle-bench-beta-").map_err(|e| e.to_string())?;

    let alpha_name = format!("alpha-{short}");
    let beta_name = format!("beta-{short}");

    eprintln!("[bench-pair] building alpha={alpha_name} beta={beta_name}");

    // Both sides use the same ws_port: WS transport dials `peer_ip:self_port`
    // so a mismatch = closed port. Each node is on its own tailnet IP.
    let alpha_fut = NodeBuilder::default()
        .app_id(TEST_APP_ID)
        .map_err(|e| e.to_string())?
        .device_name(alpha_name.clone())
        .sidecar_path(sidecar.clone())
        .state_dir(alpha_state.path().to_str().ok_or("utf8 alpha path")?)
        .auth_key(authkey)
        .ephemeral(true)
        .ws_port(9417)
        .build();
    let beta_fut = NodeBuilder::default()
        .app_id(TEST_APP_ID)
        .map_err(|e| e.to_string())?
        .device_name(beta_name.clone())
        .sidecar_path(sidecar)
        .state_dir(beta_state.path().to_str().ok_or("utf8 beta path")?)
        .auth_key(authkey)
        .ephemeral(true)
        .ws_port(9417)
        .build();

    let (alpha_res, beta_res) = tokio::join!(alpha_fut, beta_fut);
    let alpha = Arc::new(alpha_res.map_err(|e| format!("alpha build: {e}"))?);
    let beta = Arc::new(beta_res.map_err(|e| format!("beta build: {e}"))?);

    let alpha_id = alpha.local_info().device_id;
    let beta_id = beta.local_info().device_id;

    rendezvous(&alpha, &beta, &alpha_name, &beta_name, RENDEZVOUS_TIMEOUT).await?;

    // Layer-3 peers visible, but the hello handshake hasn't run yet; force
    // WS connect so `resolve_peer_id(<ULID>)` can find the other side.
    warm_up(&alpha, &beta, &alpha_name, &beta_name, RENDEZVOUS_TIMEOUT).await?;

    eprintln!("[bench-pair] ready — alpha={alpha_id} beta={beta_id}");

    Ok(BenchPair {
        alpha,
        beta,
        alpha_device_id: alpha_id,
        beta_device_id: beta_id,
        alpha_device_name: alpha_name,
        beta_device_name: beta_name,
        _alpha_state: alpha_state,
        _beta_state: beta_state,
    })
}

fn default_sidecar_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/truffle-bench; test-sidecar lives in
    // crates/truffle-core/tests/test-sidecar.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../truffle-core/tests/test-sidecar")
}

async fn warm_up(
    alpha: &Arc<Node<TailscaleProvider>>,
    beta: &Arc<Node<TailscaleProvider>>,
    alpha_name: &str,
    beta_name: &str,
    timeout: Duration,
) -> Result<(), String> {
    let beta_ts_id = alpha
        .peers()
        .await
        .into_iter()
        .find(|p| p.name.contains(beta_name) || p.device_name.contains(beta_name))
        .map(|p| p.tailscale_id)
        .ok_or("beta missing from alpha's peer list")?;
    let alpha_ts_id = beta
        .peers()
        .await
        .into_iter()
        .find(|p| p.name.contains(alpha_name) || p.device_name.contains(alpha_name))
        .map(|p| p.tailscale_id)
        .ok_or("alpha missing from beta's peer list")?;

    let _ = alpha
        .send_typed(
            &beta_ts_id,
            "_pair_warmup",
            "ping",
            &serde_json::Value::Null,
        )
        .await;
    let _ = beta
        .send_typed(
            &alpha_ts_id,
            "_pair_warmup",
            "ping",
            &serde_json::Value::Null,
        )
        .await;

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let a_ok = alpha
            .peers()
            .await
            .iter()
            .any(|p| p.tailscale_id == beta_ts_id && p.ws_connected);
        let b_ok = beta
            .peers()
            .await
            .iter()
            .any(|p| p.tailscale_id == alpha_ts_id && p.ws_connected);
        if a_ok && b_ok {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "warm-up: ws not connected within {timeout:?} (a_ok={a_ok} b_ok={b_ok})"
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn rendezvous(
    alpha: &Arc<Node<TailscaleProvider>>,
    beta: &Arc<Node<TailscaleProvider>>,
    alpha_name: &str,
    beta_name: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let alpha_peers = alpha.peers().await;
        let beta_peers = beta.peers().await;

        let a_ok = alpha_peers
            .iter()
            .any(|p| p.name.contains(beta_name) || p.device_name.contains(beta_name));
        let b_ok = beta_peers
            .iter()
            .any(|p| p.name.contains(alpha_name) || p.device_name.contains(alpha_name));
        if a_ok && b_ok {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "bench-pair rendezvous timeout after {timeout:?}: \
                 alpha_sees_beta={a_ok} beta_sees_alpha={b_ok}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
