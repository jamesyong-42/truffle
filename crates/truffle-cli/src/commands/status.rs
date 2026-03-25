//! `truffle status` -- show node status and connectivity.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output;

pub async fn run(config: &TruffleConfig, json: bool, _watch: bool) -> Result<(), String> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    let result = client
        .request(method::STATUS, serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;

    if json {
        output::print_json(&result);
        return Ok(());
    }

    let name = result["name"].as_str().unwrap_or("-");
    let status = result["status"].as_str().unwrap_or("offline");
    let ip = result["ip"].as_str().unwrap_or("");
    let dns = result["dns_name"].as_str().unwrap_or("");
    let uptime = result["uptime_secs"].as_u64().unwrap_or(0);
    let peer_count = result["peer_count"].as_u64().unwrap_or(0);

    output::print_title("truffle", name);
    println!();
    println!(
        "  {:<12}{} {}",
        "Status",
        output::status_indicator(status),
        output::status_label(status),
    );

    if !ip.is_empty() {
        println!("  {:<12}{}", "IP", ip);
    }
    if !dns.is_empty() {
        println!("  {:<12}{}", "DNS", output::dim(dns));
    }

    println!("  {:<12}{}", "Uptime", output::format_uptime(uptime));
    println!("  {:<12}{}", "Peers", peer_count);

    // Health info
    if let Some(health) = result.get("health") {
        if let Some(expiry) = health["key_expiry"].as_str() {
            if !expiry.is_empty() {
                println!("  {:<12}{}", "Key expiry", output::dim(expiry));
            }
        }
        if let Some(warnings) = health["warnings"].as_array() {
            for w in warnings {
                if let Some(s) = w.as_str() {
                    output::print_warning(s);
                }
            }
        }
    }

    println!();

    Ok(())
}
