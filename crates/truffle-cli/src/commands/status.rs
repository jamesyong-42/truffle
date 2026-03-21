//! `truffle status` -- show local node status.
//!
//! Queries the daemon via JSON-RPC and displays a formatted dashboard.
//! If the daemon is not running, shows an offline status with a hint.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output;

/// Show the node's status.
///
/// Connects to the daemon and displays node info, mesh stats, and services.
/// Supports `--json` for machine output and `--watch` for live updates.
pub async fn run(
    config: &TruffleConfig,
    json: bool,
    watch: bool,
) -> Result<(), String> {
    if watch {
        run_watch(config, json).await
    } else {
        run_once(config, json).await
    }
}

/// Show status once and exit.
async fn run_once(config: &TruffleConfig, json: bool) -> Result<(), String> {
    let client = DaemonClient::new();

    if !client.is_daemon_running() {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "status": "offline",
                    "name": config.node.name,
                })
            );
        } else {
            println!();
            println!(
                "  {} {}",
                output::bold("truffle"),
                output::dim(&format!("\u{00b7} {}", config.node.name)),
            );
            println!(
                "  {}",
                output::dim(&"\u{2500}".repeat(39)),
            );
            println!();
            println!(
                "  {:<12}{} {}",
                "Status",
                output::status_indicator("offline"),
                output::status_label("offline"),
            );
            println!();
            println!(
                "  Run '{}' to start your node.",
                output::bold("truffle up"),
            );
            println!();
        }
        return Ok(());
    }

    // Get status from daemon
    let status_result = client
        .request(method::STATUS, serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;

    // Get peers for mesh stats
    let peers_result = client
        .request(method::PEERS, serde_json::json!({}))
        .await
        .ok();

    if json {
        let mut output = status_result.clone();
        if let Some(peers) = &peers_result {
            output["peers"] = peers["peers"].clone();
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&output)
                .unwrap_or_else(|_| output.to_string()),
        );
        return Ok(());
    }

    print_status_dashboard(&status_result, peers_result.as_ref());
    Ok(())
}

/// Show status with periodic updates.
async fn run_watch(config: &TruffleConfig, json: bool) -> Result<(), String> {
    loop {
        // Clear screen for fresh display
        print!("\x1b[2J\x1b[H");
        run_once(config, json).await?;

        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(2)) => {}
            _ = tokio::signal::ctrl_c() => {
                return Ok(());
            }
        }
    }
}

/// Print the beautiful status dashboard.
fn print_status_dashboard(
    status: &serde_json::Value,
    peers: Option<&serde_json::Value>,
) {
    let name = status["name"].as_str().unwrap_or("-");
    let status_str = status["status"].as_str().unwrap_or("Online");
    let uptime_secs = status["uptime_secs"].as_u64().unwrap_or(0);
    let ip = status["tailscale_ip"].as_str();
    let dns = status["tailscale_dns_name"].as_str();

    println!();
    println!(
        "  {} {}",
        output::bold("truffle"),
        output::dim(&format!("\u{00b7} {name}")),
    );
    println!(
        "  {}",
        output::dim(&"\u{2500}".repeat(39)),
    );
    println!();

    let kw = 12; // key width

    println!(
        "  {:<kw$}{} {}",
        "Status",
        output::status_indicator(status_str),
        output::status_label(status_str),
        kw = kw,
    );

    if let Some(ip) = ip {
        output::print_kv("IP", ip, kw);
    }

    if let Some(dns) = dns {
        output::print_kv("DNS", &output::dim(dns), kw);
    }

    output::print_kv(
        "Uptime",
        &output::format_uptime(uptime_secs),
        kw,
    );

    // Mesh section
    if let Some(peers_data) = peers {
        if let Some(peer_list) = peers_data["peers"].as_array() {
            let online_count = peer_list
                .iter()
                .filter(|p| p["status"].as_str() == Some("Online"))
                .count();
            let offline_count = peer_list.len() - online_count;

            output::print_section("Mesh");

            let mesh_summary = if offline_count > 0 {
                format!(
                    "{} online, {} offline",
                    online_count, offline_count,
                )
            } else {
                format!("{} online", online_count)
            };
            output::print_kv("Nodes", &mesh_summary, kw);
        }
    }

    // Device info section
    let device_type = status["device_type"].as_str();
    let hostname = status["hostname"].as_str();

    if device_type.is_some() || hostname.is_some() {
        output::print_section("Device");
        if let Some(dt) = device_type {
            output::print_kv("Type", dt, kw);
        }
        if let Some(hn) = hostname {
            output::print_kv("Hostname", &output::dim(hn), kw);
        }
        if let Some(id) = status["device_id"].as_str() {
            output::print_kv("ID", &output::dim(id), kw);
        }
    }

    println!();
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_print_status_dashboard_does_not_panic() {
        let status = serde_json::json!({
            "device_id": "abc-123",
            "name": "test-node",
            "device_type": "cli",
            "hostname": "truffle-cli-abc123",
            "status": "Online",
            "tailscale_ip": "100.64.0.3",
            "tailscale_dns_name": "truffle-cli-abc123.tailnet.ts.net",
            "uptime_secs": 3661,
        });

        let peers = serde_json::json!({
            "peers": [
                {
                    "name": "laptop",
                    "status": "Online",
                    "tailscale_ip": "100.64.0.1",
                },
                {
                    "name": "server",
                    "status": "Offline",
                },
            ]
        });

        // Just verify it doesn't panic
        print_status_dashboard(&status, Some(&peers));
    }

    #[test]
    fn test_print_status_dashboard_minimal() {
        let status = serde_json::json!({
            "name": "test",
            "status": "Online",
        });

        // No peers, no crash
        print_status_dashboard(&status, None);
    }
}
