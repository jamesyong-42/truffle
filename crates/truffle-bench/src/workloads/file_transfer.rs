//! File transfer macro benchmark (RFC 019 §10.2.1).
//!
//! Sends a deterministic payload from alpha → beta repeatedly and reports
//! throughput + latency percentiles across iterations.

use std::time::Instant;

use clap::Args as ClapArgs;
use hdrhistogram::Histogram;
use serde::Serialize;

use crate::anyhow_lite;
use crate::harness::make_bench_pair;
use crate::report;

#[derive(ClapArgs)]
pub struct Args {
    /// Payload size per transfer. Accepts plain bytes or a suffix: K, M, G (binary).
    /// Examples: `1048576`, `1M`, `100M`, `1G`.
    #[arg(long, default_value = "10M")]
    size: String,

    /// Number of transfers to run.
    #[arg(long, default_value_t = 5)]
    iterations: u32,

    /// Warm-up iterations that don't count toward results.
    #[arg(long, default_value_t = 1)]
    warmup: u32,
}

#[derive(Serialize)]
struct Params {
    size_bytes: u64,
    iterations: u32,
    warmup: u32,
}

#[derive(Serialize)]
struct IterationResult {
    index: u32,
    bytes: u64,
    elapsed_secs: f64,
    throughput_mb_per_sec: f64,
    sha256: String,
}

#[derive(Serialize)]
struct Percentiles {
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    min_ms: f64,
    max_ms: f64,
}

#[derive(Serialize)]
struct Summary {
    iterations: Vec<IterationResult>,
    latency: Percentiles,
    throughput_mb_per_sec: Percentiles,
    mean_throughput_mb_per_sec: f64,
}

pub async fn run(args: Args, authkey: &str, out_dir: &str) -> anyhow_lite::Result {
    let size = parse_size(&args.size)?;

    eprintln!(
        "[file-transfer] size={} bytes | iterations={} | warmup={}",
        size, args.iterations, args.warmup
    );

    let pair = make_bench_pair(authkey)
        .await
        .map_err(|e| anyhow_lite::Error(format!("failed to build bench pair: {e}")))?;

    // Prepare source content once.
    let src_dir = tempfile::TempDir::new()?;
    let dst_dir = tempfile::TempDir::new()?;
    let src_path = src_dir.path().join("payload.bin");
    let content = make_payload(size as usize);
    tokio::fs::write(&src_path, &content).await?;

    // Beta: auto-accept into a persistent dir for the duration of this run.
    pair.beta
        .file_transfer()
        .auto_accept(
            pair.beta.clone(),
            dst_dir.path().to_str().expect("utf8 path"),
        )
        .await;

    let mut latency_hist: Histogram<u64> =
        Histogram::new_with_bounds(1, 60 * 60 * 1000, 3).expect("create hdrhistogram");
    let mut throughput_hist: Histogram<u64> =
        Histogram::new_with_bounds(1, 100_000, 3).expect("create throughput hdrhistogram");
    let mut iterations = Vec::with_capacity(args.iterations as usize);

    let total_runs = args.warmup + args.iterations;
    for i in 0..total_runs {
        let is_warmup = i < args.warmup;
        let label = if is_warmup { "warmup" } else { "measure" };
        let remote_name = format!("transfer-{i}.bin");

        let t0 = Instant::now();
        let r = pair
            .alpha
            .file_transfer()
            .send_file(
                &pair.beta_device_id,
                src_path.to_str().expect("utf8 path"),
                &remote_name,
            )
            .await
            .map_err(|e| anyhow_lite::Error(format!("send_file failed: {e}")))?;
        let elapsed = t0.elapsed();
        let secs = elapsed.as_secs_f64().max(1e-6);
        let mb_per_sec = (r.bytes_transferred as f64 / 1_000_000.0) / secs;

        eprintln!(
            "  [{label}] iter {i:>2}: {} bytes in {:.3}s → {:.2} MB/s",
            r.bytes_transferred, secs, mb_per_sec
        );

        if !is_warmup {
            let _ = latency_hist.record(elapsed.as_millis() as u64);
            let _ = throughput_hist.record(mb_per_sec as u64);
            iterations.push(IterationResult {
                index: i - args.warmup,
                bytes: r.bytes_transferred,
                elapsed_secs: secs,
                throughput_mb_per_sec: mb_per_sec,
                sha256: r.sha256.clone(),
            });
        }
    }

    let latency = Percentiles {
        p50_ms: latency_hist.value_at_quantile(0.50) as f64,
        p95_ms: latency_hist.value_at_quantile(0.95) as f64,
        p99_ms: latency_hist.value_at_quantile(0.99) as f64,
        min_ms: latency_hist.min() as f64,
        max_ms: latency_hist.max() as f64,
    };
    let tp = Percentiles {
        p50_ms: throughput_hist.value_at_quantile(0.50) as f64,
        p95_ms: throughput_hist.value_at_quantile(0.95) as f64,
        p99_ms: throughput_hist.value_at_quantile(0.99) as f64,
        min_ms: throughput_hist.min() as f64,
        max_ms: throughput_hist.max() as f64,
    };
    let mean = if iterations.is_empty() {
        0.0
    } else {
        iterations
            .iter()
            .map(|r| r.throughput_mb_per_sec)
            .sum::<f64>()
            / iterations.len() as f64
    };

    eprintln!(
        "\n[file-transfer] done.  latency p50={}ms p95={}ms p99={}ms  throughput mean={:.2} MB/s",
        latency.p50_ms, latency.p95_ms, latency.p99_ms, mean
    );

    let summary = Summary {
        iterations,
        latency,
        throughput_mb_per_sec: tp,
        mean_throughput_mb_per_sec: mean,
    };
    let params = Params {
        size_bytes: size,
        iterations: args.iterations,
        warmup: args.warmup,
    };

    let path = report::write(out_dir, "file-transfer", params, summary)?;
    eprintln!("[file-transfer] report written to {}", path.display());

    pair.stop().await;
    Ok(())
}

fn make_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| ((i * 7 + 13) % 256) as u8).collect()
}

/// Parse a byte-size string: plain number OR trailing K/M/G (binary).
fn parse_size(s: &str) -> anyhow_lite::Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        anyhow_lite::bail!("size is empty");
    }
    let (num_part, mult): (&str, u64) = match s.chars().last().unwrap() {
        'K' | 'k' => (&s[..s.len() - 1], 1024),
        'M' | 'm' => (&s[..s.len() - 1], 1024 * 1024),
        'G' | 'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    let n: u64 = num_part
        .parse()
        .map_err(|e| anyhow_lite::Error(format!("invalid size `{s}`: {e}")))?;
    Ok(n * mult)
}
