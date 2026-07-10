//! Integration test for RFC 022 §8 EAGER IDENTITY over a real Tailscale pair.
//!
//! Proves the core promise of eager identity (§8, default on): two online
//! peers learn each other's durable ULID `device_id` **without any application
//! `send`**. When a peer comes online at Layer 3, the session dials the
//! envelope-bus WebSocket once, purely to exchange the RFC 017 hello, so
//! `Peer.device_id` transitions `None → Some(ulid)` on its own.
//!
//! This closes the gap flagged in RFC 022 §12 Phase C / §14.3: the unit tests
//! (`session::tests::test_rfc022_eager_identity_without_app_send`) cover the
//! on/off semantics against a loopback registry, but nothing proved it end to
//! end against real tsnet. The shared pair harness normally SENDS
//! `_pair_warmup` traffic (`common::warm_up_pair`), which would mask
//! eagerness — so this test uses `common::make_truffle_pair_no_warmup`, which
//! runs rendezvous but sends nothing at the application layer.
//!
//! Skipped automatically when `TRUFFLE_TEST_AUTHKEY` is not set.
//!
//! ## Running
//!
//! ```bash
//! cargo test -p truffle-core --test integration_eager_identity -- --nocapture
//! ```

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::broadcast::error::RecvError;
use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::session::PeerEvent;
use truffle_core::Node;

/// How long to wait, after a warm-up-free rendezvous, for eager identity to
/// populate both sides' `device_id`. Generous: the eager dial has to open a
/// fresh bus WS (TCP + TLS over WireGuard) and round-trip the hello, and it
/// runs behind a concurrency semaphore. Real dials complete in a few seconds
/// in practice; 45s leaves ample head-room without hanging CI forever.
const EAGER_TIMEOUT: Duration = Duration::from_secs(45);

/// Poll cadence while waiting for eager identity to converge.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Test: two nodes, zero app sends, both observe a non-null ULID device_id.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_eager_identity_populates_device_id_without_app_send() {
    let Some(authkey) =
        common::require_authkey("test_eager_identity_populates_device_id_without_app_send")
    else {
        return;
    };
    common::init_test_tracing();

    // Warm-up OFF: rendezvous runs (peers are mutually visible at Layer 3),
    // but no `_pair_warmup` — or any other — application message is ever sent.
    let pair = common::make_truffle_pair_no_warmup(&authkey).await;

    // Subscribe to peer-change events *before* any further await so we have the
    // earliest possible chance of catching the single `Identity` edge. This is
    // best-effort: eager dial can fire during rendezvous (inside the harness,
    // before it returns), in which case the first `identity` event predates our
    // subscription and the counters below stay at 0. That is fine — we assert
    // "at most once" (a flip/duplicate regression would show up as ≥2 over the
    // whole window), and the device_id polling is the load-bearing proof.
    let alpha_rx = pair.alpha.on_peer_change();
    let beta_rx = pair.beta.on_peer_change();

    // Identify each side's counterpart by the device-name substring the harness
    // bakes into the Tailscale hostname (identity — and thus `device_name` — is
    // not known yet, so we cannot key on ULID). Rendezvous guarantees mutual
    // visibility, so these must resolve.
    let beta_ts_id = counterpart_ts_id(&pair.alpha, &pair.beta_device_name)
        .await
        .expect("alpha must see beta at Layer 3 after rendezvous");
    let alpha_ts_id = counterpart_ts_id(&pair.beta, &pair.alpha_device_name)
        .await
        .expect("beta must see alpha at Layer 3 after rendezvous");

    eprintln!(
        "[eager] rendezvous done, no app traffic sent. \
         alpha_ts={alpha_ts_id} beta_ts={beta_ts_id}. \
         waiting up to {EAGER_TIMEOUT:?} for eager identity..."
    );

    // Count `Identity` events for the counterpart on each side (RFC 022 §7.4:
    // identity fires at most once per entry — no flip, no duplicate).
    let alpha_identity_count = Arc::new(AtomicUsize::new(0));
    let beta_identity_count = Arc::new(AtomicUsize::new(0));
    let alpha_counter =
        spawn_identity_counter(alpha_rx, beta_ts_id.clone(), alpha_identity_count.clone());
    let beta_counter =
        spawn_identity_counter(beta_rx, alpha_ts_id.clone(), beta_identity_count.clone());

    // Poll both sides until each has learned the other's ULID device_id — with
    // zero application sends, this can only be eager identity at work.
    let start = Instant::now();
    let deadline = start + EAGER_TIMEOUT;
    let (alpha_view_of_beta, beta_view_of_alpha) = loop {
        let alpha_sees = observed_device_id(&pair.alpha, &beta_ts_id).await;
        let beta_sees = observed_device_id(&pair.beta, &alpha_ts_id).await;

        if is_learned_ulid(alpha_sees.as_deref(), &beta_ts_id)
            && is_learned_ulid(beta_sees.as_deref(), &alpha_ts_id)
        {
            break (alpha_sees.unwrap(), beta_sees.unwrap());
        }

        if Instant::now() >= deadline {
            panic!(
                "eager identity did NOT converge within {EAGER_TIMEOUT:?} \
                 (elapsed {:?}) with zero application sends.\n  \
                 alpha's view of beta: {}\n  \
                 beta's view of alpha: {}\n\
                 Rendezvous succeeded (peers are visible at Layer 3), so this is \
                 a PRODUCT bug, not a harness bug: the session never eagerly \
                 dialed the bus to exchange hello (RFC 022 §8).",
                start.elapsed(),
                describe_counterpart(&pair.alpha, &beta_ts_id).await,
                describe_counterpart(&pair.beta, &alpha_ts_id).await,
            );
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    };
    let elapsed = start.elapsed();

    eprintln!(
        "[eager] converged in {elapsed:?} with NO app sends: \
         alpha sees beta.device_id={alpha_view_of_beta}, \
         beta sees alpha.device_id={beta_view_of_alpha}"
    );

    // Honest-projection invariants (RFC 022 I1): device_id is a real, non-empty
    // ULID and never the Tailscale routing key.
    assert!(
        !alpha_view_of_beta.is_empty(),
        "alpha learned an empty device_id for beta"
    );
    assert_ne!(
        alpha_view_of_beta, beta_ts_id,
        "device_id must never be the Tailscale id (RFC 022 I1)"
    );
    assert!(
        !beta_view_of_alpha.is_empty(),
        "beta learned an empty device_id for alpha"
    );
    assert_ne!(
        beta_view_of_alpha, alpha_ts_id,
        "device_id must never be the Tailscale id (RFC 022 I1)"
    );

    // Stronger: the ULID learned over the eager hello must be the counterpart's
    // real local device_id — not just "some" ULID.
    assert_eq!(
        alpha_view_of_beta, pair.beta_device_id,
        "alpha should learn beta's real ULID via the eager hello"
    );
    assert_eq!(
        beta_view_of_alpha, pair.alpha_device_id,
        "beta should learn alpha's real ULID via the eager hello"
    );

    // Give any stray late/duplicate identity events a beat to arrive, then stop
    // counting and assert at-most-once.
    tokio::time::sleep(Duration::from_millis(500)).await;
    alpha_counter.abort();
    beta_counter.abort();
    let a_count = alpha_identity_count.load(Ordering::SeqCst);
    let b_count = beta_identity_count.load(Ordering::SeqCst);
    eprintln!(
        "[eager] identity events observed post-subscribe: \
         alpha_for_beta={a_count} beta_for_alpha={b_count} \
         (0 means the single edge fired before we subscribed — expected under a race)"
    );
    assert!(
        a_count <= 1,
        "alpha saw {a_count} identity events for beta; RFC 022 §7.4 requires at \
         most one per entry (no flip, no duplicate)"
    );
    assert!(
        b_count <= 1,
        "beta saw {b_count} identity events for alpha; RFC 022 §7.4 requires at \
         most one per entry (no flip, no duplicate)"
    );

    pair.stop().await;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the counterpart's Tailscale id in `node`'s Layer-3 peer view by
/// matching the device-name substring the harness bakes into the hostname.
/// (Pre-identity, `device_name` is `None`, so hostname is the reliable key.)
async fn counterpart_ts_id(node: &Node<TailscaleProvider>, device_name: &str) -> Option<String> {
    node.peers()
        .await
        .into_iter()
        .find(|p| {
            p.hostname.contains(device_name)
                || p.device_name
                    .as_deref()
                    .is_some_and(|n| n.contains(device_name))
        })
        .map(|p| p.tailscale_id)
}

/// Current `device_id` that `node` projects for the peer with `ts_id`, if any.
async fn observed_device_id(node: &Node<TailscaleProvider>, ts_id: &str) -> Option<String> {
    node.peers()
        .await
        .into_iter()
        .find(|p| p.tailscale_id == ts_id)
        .and_then(|p| p.device_id)
}

/// True when `device_id` is a learned ULID: present, non-empty, and — per
/// RFC 022 I1 — never a Tailscale-id fallback.
fn is_learned_ulid(device_id: Option<&str>, ts_id: &str) -> bool {
    matches!(device_id, Some(d) if !d.is_empty() && d != ts_id)
}

/// One-line diagnostic snapshot of `node`'s view of the peer with `ts_id`.
async fn describe_counterpart(node: &Node<TailscaleProvider>, ts_id: &str) -> String {
    match node
        .peers()
        .await
        .into_iter()
        .find(|p| p.tailscale_id == ts_id)
    {
        Some(p) => format!(
            "device_id={:?} online={} ws_connected={} display_name={:?}",
            p.device_id, p.online, p.ws_connected, p.display_name
        ),
        None => format!("<no peer with tailscale_id={ts_id} in view>"),
    }
}

/// Spawn a task that drains `rx` and counts `Identity` events whose subject is
/// `target_ts_id`. Tolerant of broadcast lag (keeps counting); exits when the
/// channel closes or the handle is aborted.
fn spawn_identity_counter(
    mut rx: tokio::sync::broadcast::Receiver<PeerEvent>,
    target_ts_id: String,
    count: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(PeerEvent::Identity(state)) if state.id == target_ts_id => {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    })
}
