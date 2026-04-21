//! Request/reply RTT macro benchmark (RFC 019 §10.2.3).
//!
//! Alpha fires N requests to beta, which echoes each payload. Measures p50,
//! p95, p99, p99.9 RTT via hdrhistogram.

use std::time::{Duration, Instant};

use clap::Args as ClapArgs;
use hdrhistogram::Histogram;
use serde::Serialize;
use serde_json::json;

use crate::anyhow_lite;
use crate::harness::make_bench_pair;
use crate::report;

#[derive(ClapArgs)]
pub struct Args {
    /// Number of request/reply round trips.
    #[arg(long, default_value_t = 500)]
    count: u32,

    /// Per-request payload size in bytes.
    #[arg(long, default_value_t = 1024)]
    payload_bytes: usize,

    /// Per-request timeout in seconds.
    #[arg(long, default_value_t = 10)]
    timeout_secs: u64,
}

#[derive(Serialize)]
struct Params {
    count: u32,
    payload_bytes: usize,
    timeout_secs: u64,
}

#[derive(Serialize)]
struct Summary {
    successes: u32,
    timeouts: u32,
    total_elapsed_secs: f64,
    rtt_ms: Percentiles,
}

#[derive(Serialize)]
struct Percentiles {
    p50: f64,
    p95: f64,
    p99: f64,
    p999: f64,
    min: f64,
    max: f64,
    mean: f64,
}

pub async fn run(args: Args, authkey: &str, out_dir: &str) -> anyhow_lite::Result {
    eprintln!(
        "[request-reply] count={} payload_bytes={} timeout={}s",
        args.count, args.payload_bytes, args.timeout_secs
    );

    let pair = make_bench_pair(authkey)
        .await
        .map_err(|e| anyhow_lite::Error(format!("failed to build bench pair: {e}")))?;

    // Beta: echo handler. Must live for the duration of the benchmark.
    let beta = pair.beta.clone();
    let responder = tokio::spawn(async move {
        let mut rx = beta.subscribe("bench-rr");
        while let Ok(msg) = rx.recv().await {
            if msg.msg_type == "ping" {
                let _ = beta
                    .send_typed(&msg.from, "bench-rr", "pong", &msg.payload)
                    .await;
            }
        }
    });

    // Give the subscription a moment to register.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let payload = json!({ "blob": "x".repeat(args.payload_bytes) });
    let peer_id = pair.beta_device_id.clone();
    let mut hist: Histogram<u64> =
        Histogram::new_with_bounds(1, 60_000_000, 3).expect("create rtt hdrhistogram");
    let mut successes = 0u32;
    let mut timeouts = 0u32;

    let t0 = Instant::now();
    for i in 0..args.count {
        let start = Instant::now();
        let res = truffle_core::request_reply::send_and_wait(
            &pair.alpha,
            &peer_id,
            "bench-rr",
            "ping",
            &payload,
            Duration::from_secs(args.timeout_secs),
            |msg| {
                if msg.msg_type == "pong" {
                    Some(())
                } else {
                    None
                }
            },
        )
        .await;
        let elapsed = start.elapsed();
        match res {
            Ok(()) => {
                successes += 1;
                let _ = hist.record(elapsed.as_micros() as u64);
            }
            Err(_) => {
                timeouts += 1;
            }
        }
        if i % 50 == 0 && i > 0 {
            eprintln!(
                "  [request-reply] progress {i}/{}: p50={:.2}ms p99={:.2}ms (so far)",
                args.count,
                hist.value_at_quantile(0.50) as f64 / 1000.0,
                hist.value_at_quantile(0.99) as f64 / 1000.0
            );
        }
    }
    let total = t0.elapsed();

    responder.abort();

    let us_to_ms = |us: f64| us / 1000.0;
    let pct = Percentiles {
        p50: us_to_ms(hist.value_at_quantile(0.50) as f64),
        p95: us_to_ms(hist.value_at_quantile(0.95) as f64),
        p99: us_to_ms(hist.value_at_quantile(0.99) as f64),
        p999: us_to_ms(hist.value_at_quantile(0.999) as f64),
        min: us_to_ms(hist.min() as f64),
        max: us_to_ms(hist.max() as f64),
        mean: us_to_ms(hist.mean()),
    };

    eprintln!(
        "\n[request-reply] done.  {} ok / {} timeouts  p50={:.2}ms p95={:.2}ms p99={:.2}ms p99.9={:.2}ms",
        successes, timeouts, pct.p50, pct.p95, pct.p99, pct.p999
    );

    let summary = Summary {
        successes,
        timeouts,
        total_elapsed_secs: total.as_secs_f64(),
        rtt_ms: pct,
    };
    let params = Params {
        count: args.count,
        payload_bytes: args.payload_bytes,
        timeout_secs: args.timeout_secs,
    };
    let path = report::write(out_dir, "request-reply", params, summary)?;
    eprintln!("[request-reply] report written to {}", path.display());

    pair.stop().await;
    Ok(())
}
