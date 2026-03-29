//! `truffle down` -- stop the daemon.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::exit_codes;
use crate::json_output;
use crate::output;

pub async fn run(config: &TruffleConfig, force: bool, json: bool) -> Result<(), (i32, String)> {
    let _ = force; // TODO: force-kill if transfers in progress

    let client = DaemonClient::new();

    if !client.is_daemon_running() {
        if json {
            let mut map = json_output::envelope(&config.node.name);
            map.insert("status".to_string(), serde_json::json!("not_running"));
            json_output::print_json(&serde_json::Value::Object(map));
        } else {
            println!("  Daemon is not running.");
        }
        return Ok(());
    }

    match client.request(method::SHUTDOWN, serde_json::json!({})).await {
        Ok(_) => {
            if json {
                let mut map = json_output::envelope(&config.node.name);
                map.insert("status".to_string(), serde_json::json!("stopped"));
                json_output::print_json(&serde_json::Value::Object(map));
            } else {
                output::print_success("Daemon stopped.");
            }
        }
        Err(e) => {
            // Connection may close before we get the response (shutdown is immediate)
            let err_str = e.to_string();
            if err_str.contains("closed connection") || err_str.contains("I/O error") {
                if json {
                    let mut map = json_output::envelope(&config.node.name);
                    map.insert("status".to_string(), serde_json::json!("stopped"));
                    json_output::print_json(&serde_json::Value::Object(map));
                } else {
                    output::print_success("Daemon stopped.");
                }
            } else {
                return Err((
                    exit_codes::ERROR,
                    format!("Failed to stop daemon: {e}"),
                ));
            }
        }
    }

    Ok(())
}
