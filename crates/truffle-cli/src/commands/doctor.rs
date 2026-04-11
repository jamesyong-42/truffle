//! `truffle doctor` -- diagnose connectivity issues.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::exit_codes;
use crate::json_output;
use crate::output;

pub async fn run(config: &TruffleConfig, json: bool) -> Result<(), (i32, String)> {
    let _ = config;
    let client = DaemonClient::new();

    if !json {
        println!();
        println!("  {}", output::bold("truffle doctor"));
        println!("  {}", output::dim(&"\u{2500}".repeat(39)));
        println!();
    }

    // Collect checks for JSON output
    let mut checks_json: Vec<serde_json::Value> = Vec::new();

    // 1. Check daemon is running
    if !client.is_daemon_running() {
        if json {
            checks_json.push(serde_json::json!({
                "name": "daemon_running",
                "pass": false,
                "detail": "not running",
            }));
            let mut map = json_output::envelope(&config.node.device_name);
            map.insert("checks".to_string(), serde_json::json!(checks_json));
            json_output::print_json(&serde_json::Value::Object(map));
        } else {
            output::print_check(output::Indicator::Fail, "Daemon running", "not running");
            output::print_fix_suggestion("Run 'truffle up' to start the daemon");
            println!();
        }
        return Ok(());
    }

    if json {
        checks_json.push(serde_json::json!({
            "name": "daemon_running",
            "pass": true,
            "detail": "running",
        }));
    } else {
        output::print_check(output::Indicator::Pass, "Daemon running", "");
    }

    // 2. Get doctor info from daemon
    let result = client
        .request(method::DOCTOR, serde_json::json!({}))
        .await
        .map_err(|e| (exit_codes::ERROR, e.to_string()))?;

    let checks = &result["checks"];

    // 3. Node online
    let online = checks["node_online"].as_bool().unwrap_or(false);
    let state = checks["tailscale_state"].as_str().unwrap_or("unknown");
    if json {
        checks_json.push(serde_json::json!({
            "name": "network_connected",
            "pass": online,
            "detail": state,
        }));
    } else if online {
        output::print_check(output::Indicator::Pass, "Network connected", state);
    } else {
        output::print_check(output::Indicator::Fail, "Network connected", state);
        output::print_fix_suggestion("Check Tailscale status and network connectivity");
    }

    // 4. Peer count
    let peer_count = checks["peer_count"].as_u64().unwrap_or(0);
    if json {
        checks_json.push(serde_json::json!({
            "name": "mesh_reachable",
            "pass": peer_count > 0,
            "detail": format!("{} peers", peer_count),
        }));
    } else if peer_count > 0 {
        output::print_check(
            output::Indicator::Pass,
            "Mesh reachable",
            &format!("{} peers", peer_count),
        );
    } else {
        output::print_check(output::Indicator::Warn, "Mesh reachable", "no peers found");
        output::print_fix_suggestion("Start truffle on other devices in your tailnet");
    }

    // 5. Key expiry
    if let Some(expiry) = checks["key_expiry"].as_str() {
        if !expiry.is_empty() {
            if json {
                checks_json.push(serde_json::json!({
                    "name": "key_expiry",
                    "pass": true,
                    "detail": expiry,
                }));
            } else {
                output::print_check(output::Indicator::Pass, "Key expiry", expiry);
            }
        }
    }

    // 6. Warnings
    if let Some(warnings) = checks["warnings"].as_array() {
        for w in warnings {
            if let Some(s) = w.as_str() {
                if json {
                    checks_json.push(serde_json::json!({
                        "name": "health_warning",
                        "pass": false,
                        "detail": s,
                    }));
                } else {
                    output::print_check(output::Indicator::Warn, "Health warning", s);
                }
            }
        }
    }

    // 7. Config file
    let config_path = TruffleConfig::default_path();
    if json {
        checks_json.push(serde_json::json!({
            "name": "config_file",
            "pass": config_path.exists(),
            "detail": if config_path.exists() {
                config_path.display().to_string()
            } else {
                "using defaults".to_string()
            },
        }));
    } else if config_path.exists() {
        output::print_check(
            output::Indicator::Pass,
            "Config file",
            &config_path.display().to_string(),
        );
    } else {
        output::print_check(output::Indicator::Skip, "Config file", "using defaults");
    }

    if json {
        let mut map = json_output::envelope(&config.node.device_name);
        map.insert("checks".to_string(), serde_json::json!(checks_json));
        json_output::print_json(&serde_json::Value::Object(map));
    } else {
        println!();
    }

    Ok(())
}
