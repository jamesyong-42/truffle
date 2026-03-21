//! `truffle ping` -- connectivity check.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;

/// Ping a node.
pub async fn run(config: &TruffleConfig, node: &str, _count: u32) -> Result<(), String> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    let result = client
        .request(
            method::PING,
            serde_json::json!({ "node": node }),
        )
        .await
        .map_err(|e| e.to_string())?;

    let reachable = result["reachable"].as_bool().unwrap_or(false);
    if reachable {
        println!("{node} is reachable.");
    } else {
        let reason = result["reason"].as_str().unwrap_or("unknown");
        println!("{node} is not reachable: {reason}");
    }

    Ok(())
}
