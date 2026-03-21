//! `truffle ping` -- connectivity check to a node.
//!
//! Resolves the target node name, then sends multiple ping requests via the
//! daemon and displays results with latency statistics.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output;
use crate::resolve::NameResolver;

/// Ping a node for connectivity.
///
/// 1. Resolves the node name using the name resolver
/// 2. Sends `count` ping requests via the daemon
/// 3. Displays each reply with latency and connection type
/// 4. Shows summary statistics (min/avg/max, loss%)
pub async fn run(
    config: &TruffleConfig,
    node: &str,
    count: u32,
) -> Result<(), String> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    // Get peers for name resolution
    let peers_result = client
        .request(method::PEERS, serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;

    let peers = peers_result["peers"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    // Resolve the node name
    let resolver = NameResolver::from_daemon_data(&config.aliases, &peers);
    let resolved = match resolver.resolve(node) {
        Ok(target) => target,
        Err(e) => {
            return Err(e.to_string());
        }
    };

    let display_name = &resolved.display_name;
    let target_ip = resolved.address.clone();

    // Header
    println!();
    println!(
        "  PING {} ({}) via Tailscale",
        output::bold(display_name),
        output::dim(&target_ip),
    );

    // Send pings
    let mut latencies: Vec<f64> = Vec::new();
    let mut received = 0u32;

    for seq in 0..count {
        if seq > 0 {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }

        let ping_start = std::time::Instant::now();
        let result = client
            .request(
                method::PING,
                serde_json::json!({
                    "node": node,
                    "device_id": &resolved.device_id,
                }),
            )
            .await;

        match result {
            Ok(resp) => {
                let reachable = resp["reachable"].as_bool().unwrap_or(false);
                if reachable {
                    // Use latency from the daemon if available, otherwise
                    // measure the round-trip time of the RPC call itself
                    let latency = resp["latency_ms"]
                        .as_f64()
                        .unwrap_or_else(|| ping_start.elapsed().as_secs_f64() * 1000.0);

                    let connection = resp["connection"]
                        .as_str()
                        .unwrap_or("unknown");

                    let connection_info = match connection {
                        "direct" => "direct".to_string(),
                        relay if relay.starts_with("relay:") => {
                            format!("via {relay}")
                        }
                        other => format!("({other})"),
                    };

                    println!(
                        "  reply from {}: time={} ({})",
                        display_name,
                        output::format_latency(Some(latency)),
                        connection_info,
                    );

                    latencies.push(latency);
                    received += 1;
                } else {
                    let reason = resp["reason"].as_str().unwrap_or("unknown");
                    println!(
                        "  {} from {}: {}",
                        output::red("no reply"),
                        display_name,
                        reason,
                    );
                }
            }
            Err(e) => {
                println!(
                    "  {} request failed: {}",
                    output::red("error"),
                    e,
                );
            }
        }
    }

    // Statistics
    let loss_pct = if count > 0 {
        ((count - received) as f64 / count as f64) * 100.0
    } else {
        0.0
    };

    println!();
    println!(
        "  {} {display_name} ping statistics {}",
        output::dim("---"),
        output::dim("---"),
    );
    println!(
        "  {} sent, {} received, {:.0}% loss",
        count, received, loss_pct,
    );

    if !latencies.is_empty() {
        let min = latencies
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);
        let max = latencies
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        let avg = latencies.iter().sum::<f64>() / latencies.len() as f64;

        println!(
            "  round-trip min/avg/max = {}/{}/{} ms",
            format_stat(min),
            format_stat(avg),
            format_stat(max),
        );
    }

    println!();

    if received == 0 && count > 0 {
        Err(format!("{display_name} is not responding."))
    } else {
        Ok(())
    }
}

/// Format a latency stat value consistently.
fn format_stat(ms: f64) -> String {
    if ms < 1.0 {
        format!("{ms:.2}")
    } else if ms < 10.0 {
        format!("{ms:.1}")
    } else {
        format!("{ms:.0}")
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_stat() {
        assert_eq!(format_stat(0.5), "0.50");
        assert_eq!(format_stat(2.1), "2.1");
        assert_eq!(format_stat(14.0), "14");
    }
}
