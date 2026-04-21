//! Integration tests for SyncedStore convergence across a real Tailscale pair.
//!
//! These tests close the gap called out in RFC 019 §1: SyncedStore behaviour
//! over the wire is otherwise only tested against `MockNetworkProvider`.
//! Here, two ephemeral Nodes register on the real tailnet, create stores with
//! the same `store_id`, and we assert that writes on one side converge on the
//! other.
//!
//! Skipped automatically when `TRUFFLE_TEST_AUTHKEY` is not set.
//!
//! ## Running
//!
//! ```bash
//! cargo test -p truffle-core --test integration_synced_store -- --nocapture
//! ```

mod common;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use truffle_core::synced_store::StoreEvent;

/// Shape written to the test store. Simple enough to be obviously correct,
/// rich enough to catch serialisation regressions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Slice {
    owner: String,
    count: u32,
    note: String,
}

/// Longest we'll wait for a write on one side to appear on the other.
const CONVERGE_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Test 1: Simple one-shot convergence — alpha writes, beta reads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_pair_converges_on_simple_write() {
    let Some(authkey) = common::require_authkey("test_pair_converges_on_simple_write") else {
        return;
    };
    common::init_test_tracing();

    let pair = common::make_truffle_pair(&authkey).await;

    let alpha_store = pair.alpha.synced_store::<Slice>("convergence-simple");
    let beta_store = pair.beta.synced_store::<Slice>("convergence-simple");

    let value = Slice {
        owner: "alpha".to_string(),
        count: 1,
        note: "hello from alpha".to_string(),
    };
    alpha_store.set(value.clone()).await;

    let alpha_id = &pair.alpha_device_id;
    eprintln!("  waiting for beta to see alpha's slice ({alpha_id})...");
    let observed = common::wait_for(CONVERGE_TIMEOUT, || async {
        beta_store.get(alpha_id).await.map(|s| s.data)
    })
    .await;

    assert_eq!(
        observed.as_ref(),
        Some(&value),
        "beta should see alpha's slice within {CONVERGE_TIMEOUT:?}"
    );

    pair.stop().await;
}

// ---------------------------------------------------------------------------
// Test 2: Both write concurrently; each ends up with both slices
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_pair_both_write_both_see() {
    let Some(authkey) = common::require_authkey("test_pair_both_write_both_see") else {
        return;
    };
    common::init_test_tracing();

    let pair = common::make_truffle_pair(&authkey).await;

    let alpha_store = pair.alpha.synced_store::<Slice>("convergence-both");
    let beta_store = pair.beta.synced_store::<Slice>("convergence-both");

    let alpha_val = Slice {
        owner: "alpha".to_string(),
        count: 100,
        note: "alpha's slice".to_string(),
    };
    let beta_val = Slice {
        owner: "beta".to_string(),
        count: 200,
        note: "beta's slice".to_string(),
    };

    // Write both roughly concurrently — each owns their own slice so there's
    // no conflict, just independent propagation.
    tokio::join!(
        alpha_store.set(alpha_val.clone()),
        beta_store.set(beta_val.clone())
    );

    let alpha_id = pair.alpha_device_id.clone();
    let beta_id = pair.beta_device_id.clone();

    // Beta should see alpha's slice
    let beta_sees_alpha = common::wait_for(CONVERGE_TIMEOUT, || async {
        beta_store.get(&alpha_id).await.map(|s| s.data)
    })
    .await;
    assert_eq!(
        beta_sees_alpha.as_ref(),
        Some(&alpha_val),
        "beta should see alpha's slice"
    );

    // Alpha should see beta's slice
    let alpha_sees_beta = common::wait_for(CONVERGE_TIMEOUT, || async {
        alpha_store.get(&beta_id).await.map(|s| s.data)
    })
    .await;
    assert_eq!(
        alpha_sees_beta.as_ref(),
        Some(&beta_val),
        "alpha should see beta's slice"
    );

    // Each store should now know about both device_ids
    let alpha_devices = alpha_store.device_ids().await;
    let beta_devices = beta_store.device_ids().await;
    assert!(
        alpha_devices.contains(&alpha_id) && alpha_devices.contains(&beta_id),
        "alpha should track both device_ids, got {alpha_devices:?}"
    );
    assert!(
        beta_devices.contains(&alpha_id) && beta_devices.contains(&beta_id),
        "beta should track both device_ids, got {beta_devices:?}"
    );

    pair.stop().await;
}

// ---------------------------------------------------------------------------
// Test 3: Rapid successive updates — final value converges
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_pair_converges_after_many_updates() {
    let Some(authkey) = common::require_authkey("test_pair_converges_after_many_updates") else {
        return;
    };
    common::init_test_tracing();

    let pair = common::make_truffle_pair(&authkey).await;

    let alpha_store = pair.alpha.synced_store::<Slice>("convergence-rapid");
    let beta_store = pair.beta.synced_store::<Slice>("convergence-rapid");

    // Alpha makes 20 rapid updates. Each overwrites the previous (since
    // each device owns one slice). The final value must be `count = 20`.
    for i in 1..=20 {
        alpha_store
            .set(Slice {
                owner: "alpha".to_string(),
                count: i,
                note: format!("update {i}"),
            })
            .await;
        // Small yield to interleave with the sync task; not strictly required.
        tokio::task::yield_now().await;
    }

    let alpha_id = pair.alpha_device_id.clone();
    let observed = common::wait_for(CONVERGE_TIMEOUT, || async {
        let slice = beta_store.get(&alpha_id).await?;
        // Only accept the terminal value
        if slice.data.count == 20 {
            Some(slice.data)
        } else {
            None
        }
    })
    .await;

    assert_eq!(
        observed.as_ref().map(|s| s.count),
        Some(20),
        "beta should converge on alpha's final value (count=20) within {CONVERGE_TIMEOUT:?}"
    );

    pair.stop().await;
}

// ---------------------------------------------------------------------------
// Test 4: Subscribe — beta receives a StoreEvent for alpha's write
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_pair_subscribe_receives_remote_event() {
    let Some(authkey) = common::require_authkey("test_pair_subscribe_receives_remote_event") else {
        return;
    };
    common::init_test_tracing();

    let pair = common::make_truffle_pair(&authkey).await;

    let alpha_store = pair.alpha.synced_store::<Slice>("convergence-events");
    let beta_store = pair.beta.synced_store::<Slice>("convergence-events");

    // Beta subscribes BEFORE alpha writes.
    let mut beta_events = beta_store.subscribe();

    // Give the subscription a beat to register before we send the write.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let value = Slice {
        owner: "alpha".to_string(),
        count: 42,
        note: "eventful".to_string(),
    };
    alpha_store.set(value.clone()).await;

    let alpha_id = pair.alpha_device_id.clone();

    // Collect events for up to CONVERGE_TIMEOUT, looking for a PeerUpdated
    // from alpha with the expected payload.
    let found = timeout(CONVERGE_TIMEOUT, async {
        loop {
            match beta_events.recv().await {
                Ok(StoreEvent::PeerUpdated {
                    device_id, data, ..
                }) if device_id == alpha_id => {
                    if data == value {
                        break Some(data);
                    }
                }
                Ok(other) => {
                    eprintln!("  beta observed other event: {other:?}");
                }
                Err(e) => {
                    eprintln!("  beta event channel err: {e}");
                    break None;
                }
            }
        }
    })
    .await
    .unwrap_or(None);

    assert_eq!(
        found.as_ref(),
        Some(&value),
        "beta should receive a PeerUpdated event for alpha's write"
    );

    pair.stop().await;
}
