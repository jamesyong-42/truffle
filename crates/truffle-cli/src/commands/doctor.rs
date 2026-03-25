//! `truffle doctor` -- diagnose connectivity issues.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output;

pub async fn run(_config: &TruffleConfig) -> Result<(), String> {
    let client = DaemonClient::new();

    println!();
    println!("  {}", output::bold("truffle doctor"));
    println!("  {}", output::dim(&"\u{2500}".repeat(39)));
    println!();

    // 1. Check daemon is running
    if !client.is_daemon_running() {
        output::print_check(
            output::Indicator::Fail,
            "Daemon running",
            "not running",
        );
        output::print_fix_suggestion("Run 'truffle up' to start the daemon");
        println!();
        return Ok(());
    }
    output::print_check(output::Indicator::Pass, "Daemon running", "");

    // 2. Get doctor info from daemon
    let result = client
        .request(method::DOCTOR, serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;

    let checks = &result["checks"];

    // 3. Node online
    let online = checks["node_online"].as_bool().unwrap_or(false);
    let state = checks["tailscale_state"].as_str().unwrap_or("unknown");
    if online {
        output::print_check(output::Indicator::Pass, "Network connected", state);
    } else {
        output::print_check(output::Indicator::Fail, "Network connected", state);
        output::print_fix_suggestion("Check Tailscale status and network connectivity");
    }

    // 4. Peer count
    let peer_count = checks["peer_count"].as_u64().unwrap_or(0);
    if peer_count > 0 {
        output::print_check(
            output::Indicator::Pass,
            "Mesh reachable",
            &format!("{} peers", peer_count),
        );
    } else {
        output::print_check(
            output::Indicator::Warn,
            "Mesh reachable",
            "no peers found",
        );
        output::print_fix_suggestion("Start truffle on other devices in your tailnet");
    }

    // 5. Key expiry
    if let Some(expiry) = checks["key_expiry"].as_str() {
        if !expiry.is_empty() {
            output::print_check(output::Indicator::Pass, "Key expiry", expiry);
        }
    }

    // 6. Warnings
    if let Some(warnings) = checks["warnings"].as_array() {
        for w in warnings {
            if let Some(s) = w.as_str() {
                output::print_check(output::Indicator::Warn, "Health warning", s);
            }
        }
    }

    // 7. Config file
    let config_path = TruffleConfig::default_path();
    if config_path.exists() {
        output::print_check(
            output::Indicator::Pass,
            "Config file",
            &config_path.display().to_string(),
        );
    } else {
        output::print_check(
            output::Indicator::Skip,
            "Config file",
            "using defaults",
        );
    }

    println!();
    Ok(())
}
