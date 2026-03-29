//! `truffle ping` -- connectivity check with latency measurement.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::exit_codes;
use crate::json_output;
use crate::output;

pub async fn run(
    config: &TruffleConfig,
    node: &str,
    count: u32,
    json: bool,
) -> Result<(), (i32, String)> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| (exit_codes::NOT_ONLINE, e.to_string()))?;

    if !json {
        println!("PING {} ({} pings):", output::bold(node), count);
        println!();
    }

    let mut sent = 0u32;
    let mut received = 0u32;
    let mut min_ms = f64::MAX;
    let mut max_ms = 0.0f64;
    let mut total_ms = 0.0f64;
    let mut results_json: Vec<serde_json::Value> = Vec::new();

    for i in 0..count {
        sent += 1;

        let result = client
            .request(
                method::PING,
                serde_json::json!({ "node": node }),
            )
            .await;

        match result {
            Ok(resp) => {
                let latency_ms = resp["latency_ms"].as_f64().unwrap_or(0.0);
                let connection = resp["connection"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();

                received += 1;
                min_ms = min_ms.min(latency_ms);
                max_ms = max_ms.max(latency_ms);
                total_ms += latency_ms;

                if json {
                    results_json.push(serde_json::json!({
                        "seq": sent,
                        "ms": latency_ms,
                        "connection": connection,
                    }));
                } else {
                    println!(
                        "  reply from {}: time={} connection={}",
                        output::bold(node),
                        output::format_latency(Some(latency_ms)),
                        connection,
                    );
                }
            }
            Err(e) => {
                if json {
                    results_json.push(serde_json::json!({
                        "seq": sent,
                        "error": e.to_string(),
                    }));
                } else {
                    println!(
                        "  {} Request timeout: {}",
                        output::red("x"),
                        e,
                    );
                }
            }
        }

        // Wait between pings (except the last one)
        if i + 1 < count {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }

    let loss_pct = if sent > 0 {
        ((sent - received) as f64 / sent as f64) * 100.0
    } else {
        0.0
    };
    let avg_ms = if received > 0 {
        total_ms / received as f64
    } else {
        0.0
    };

    if json {
        let mut map = json_output::envelope(&config.node.name);
        map.insert("target".to_string(), serde_json::json!(node));
        map.insert("results".to_string(), serde_json::json!(results_json));
        map.insert(
            "stats".to_string(),
            serde_json::json!({
                "sent": sent,
                "received": received,
                "loss_pct": loss_pct,
                "min_ms": if received > 0 { Some(min_ms) } else { None },
                "avg_ms": if received > 0 { Some(avg_ms) } else { None },
                "max_ms": if received > 0 { Some(max_ms) } else { None },
            }),
        );
        json_output::print_json(&serde_json::Value::Object(map));
    } else {
        // Print statistics
        println!();
        println!(
            "  --- {} ping statistics ---",
            output::bold(node),
        );
        println!(
            "  {} transmitted, {} received, {:.0}% packet loss",
            sent, received, loss_pct,
        );
        if received > 0 {
            println!(
                "  rtt min/avg/max = {}/{}/{}",
                output::format_latency(Some(min_ms)),
                output::format_latency(Some(avg_ms)),
                output::format_latency(Some(max_ms)),
            );
        }
        println!();
    }

    if received == 0 {
        Err((
            exit_codes::TIMEOUT,
            format!("All {} pings to {} failed", sent, node),
        ))
    } else {
        Ok(())
    }
}
