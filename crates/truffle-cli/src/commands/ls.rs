//! `truffle ls` -- list peers on the mesh.
//!
//! Queries the daemon for the peer list and displays a formatted table.
//! Supports `--all` (include offline), `--long` (extra columns), `--json`.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output;

/// List peers on the mesh.
///
/// Connects to the daemon and displays a table of discovered peers.
/// - Default: online peers only, compact format
/// - `--all`: include offline peers
/// - `--long`: add IP, OS, capabilities columns
/// - `--json`: output as JSON array
pub async fn run(
    config: &TruffleConfig,
    all: bool,
    long: bool,
    json: bool,
) -> Result<(), String> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    let result = client
        .request(method::PEERS, serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;

    let peers = result["peers"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    // Filter peers based on --all flag
    let filtered: Vec<&serde_json::Value> = peers
        .iter()
        .filter(|p| {
            if all {
                true
            } else {
                let status = p["status"].as_str().unwrap_or("");
                status == "Online" || status == "Connecting"
            }
        })
        .collect();

    // JSON output mode
    if json {
        let json_output: Vec<serde_json::Value> = filtered
            .iter()
            .map(|p| {
                let mut obj = serde_json::json!({
                    "name": p["name"].as_str().unwrap_or("-"),
                    "status": status_to_lower(p["status"].as_str().unwrap_or("offline")),
                    "ip": p["tailscale_ip"].as_str(),
                    "latency_ms": p["latency_ms"].as_f64(),
                });
                if long {
                    obj["id"] = p["id"].clone();
                    obj["device_type"] = p["device_type"].clone();
                    obj["hostname"] = serde_json::Value::String(
                        p["tailscale_dns_name"]
                            .as_str()
                            .or_else(|| p["hostname"].as_str())
                            .unwrap_or("-")
                            .to_string(),
                    );
                    obj["os"] = p["os"].clone();
                }
                obj
            })
            .collect();

        println!(
            "{}",
            serde_json::to_string_pretty(&json_output)
                .unwrap_or_else(|_| "[]".to_string()),
        );
        return Ok(());
    }

    // Human-readable output
    if filtered.is_empty() {
        if all {
            println!();
            println!(
                "  No other nodes found. Is Tailscale connected? Run '{}' to check.",
                output::bold("truffle doctor"),
            );
            println!();
        } else {
            println!();
            println!(
                "  No peers online. Use '{}' to include offline nodes.",
                output::bold("truffle ls -a"),
            );
            println!();
        }
        return Ok(());
    }

    println!();

    if long {
        print_long_table(&filtered);
    } else {
        print_compact_table(&filtered);
    }

    // Summary line
    let online_count = peers
        .iter()
        .filter(|p| p["status"].as_str() == Some("Online"))
        .count();
    let offline_count = peers.len() - online_count;

    let summary = if all && offline_count > 0 {
        format!(
            "{} node{} online, {} offline",
            online_count,
            if online_count == 1 { "" } else { "s" },
            offline_count,
        )
    } else {
        format!(
            "{} node{} online",
            online_count,
            if online_count == 1 { "" } else { "s" },
        )
    };

    println!();
    println!("  {}", output::dim(&summary));
    println!();

    Ok(())
}

/// Print a compact table (NODE, STATUS, CONNECTION, LATENCY).
fn print_compact_table(peers: &[&serde_json::Value]) {
    let headers = &["NODE", "STATUS", "CONNECTION", "LATENCY"];

    let rows: Vec<Vec<String>> = peers
        .iter()
        .map(|p| {
            let name = p["name"].as_str().unwrap_or("-").to_string();
            let status_str = p["status"].as_str().unwrap_or("offline");
            let status = format!(
                "{} {}",
                output::status_indicator(status_str),
                output::status_label(status_str),
            );
            let connection = connection_type(p);
            let latency = output::format_latency(p["latency_ms"].as_f64());

            vec![name, status, connection, latency]
        })
        .collect();

    output::print_table(headers, &rows);
}

/// Print a long table (NODE, STATUS, CONNECTION, IP, OS, LATENCY).
fn print_long_table(peers: &[&serde_json::Value]) {
    let headers = &["NODE", "STATUS", "CONNECTION", "IP", "OS", "LATENCY"];

    let rows: Vec<Vec<String>> = peers
        .iter()
        .map(|p| {
            let name = p["name"].as_str().unwrap_or("-").to_string();
            let status_str = p["status"].as_str().unwrap_or("offline");
            let status = format!(
                "{} {}",
                output::status_indicator(status_str),
                output::status_label(status_str),
            );
            let connection = connection_type(p);
            let ip = p["tailscale_ip"]
                .as_str()
                .unwrap_or("\u{2014}")
                .to_string();
            let os = p["os"]
                .as_str()
                .unwrap_or("\u{2014}")
                .to_string();
            let latency = output::format_latency(p["latency_ms"].as_f64());

            vec![name, status, connection, ip, os, latency]
        })
        .collect();

    output::print_table(headers, &rows);
}

/// Determine the connection type for a peer.
///
/// Uses `cur_addr` (direct) and `relay` (relayed) fields if available.
fn connection_type(peer: &serde_json::Value) -> String {
    let is_online = peer["status"].as_str() == Some("Online");
    if !is_online {
        return output::dim("\u{2014}").to_string();
    }

    // Check for direct/relay info
    if let Some(relay) = peer["relay"].as_str() {
        if !relay.is_empty() {
            return output::yellow(&format!("via relay: {relay}"));
        }
    }

    if peer["cur_addr"].as_str().is_some() {
        return "direct".to_string();
    }

    // Default for online peers without connection info
    "\u{2014}".to_string()
}

/// Convert status string to lowercase for JSON output.
fn status_to_lower(status: &str) -> String {
    match status {
        "Online" => "online".to_string(),
        "Offline" => "offline".to_string(),
        "Connecting" => "connecting".to_string(),
        other => other.to_lowercase(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_type_offline() {
        let peer = serde_json::json!({ "status": "Offline" });
        let result = connection_type(&peer);
        // Should contain the em-dash
        assert!(result.contains("\u{2014}") || result.contains("2014"));
    }

    #[test]
    fn test_connection_type_direct() {
        let peer = serde_json::json!({
            "status": "Online",
            "cur_addr": "192.168.1.5:41641",
        });
        assert_eq!(connection_type(&peer), "direct");
    }

    #[test]
    fn test_connection_type_relayed() {
        let peer = serde_json::json!({
            "status": "Online",
            "relay": "sea",
        });
        let result = connection_type(&peer);
        assert!(result.contains("relay"));
        assert!(result.contains("sea"));
    }

    #[test]
    fn test_status_to_lower() {
        assert_eq!(status_to_lower("Online"), "online");
        assert_eq!(status_to_lower("Offline"), "offline");
        assert_eq!(status_to_lower("Connecting"), "connecting");
        assert_eq!(status_to_lower("Unknown"), "unknown");
    }

    #[test]
    fn test_compact_table_does_not_panic() {
        let peers_data = vec![
            serde_json::json!({
                "name": "laptop",
                "status": "Online",
                "latency_ms": 2.1,
            }),
            serde_json::json!({
                "name": "server",
                "status": "Offline",
            }),
        ];
        let refs: Vec<&serde_json::Value> = peers_data.iter().collect();
        print_compact_table(&refs);
    }

    #[test]
    fn test_long_table_does_not_panic() {
        let peers_data = vec![serde_json::json!({
            "name": "laptop",
            "status": "Online",
            "tailscale_ip": "100.64.0.3",
            "os": "darwin",
            "latency_ms": 2.1,
            "cur_addr": "192.168.1.5:41641",
        })];
        let refs: Vec<&serde_json::Value> = peers_data.iter().collect();
        print_long_table(&refs);
    }
}
