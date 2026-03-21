//! `truffle ls` -- list peers on the mesh.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;

/// List peers.
pub async fn run(config: &TruffleConfig, _all: bool, _json: bool) -> Result<(), String> {
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

    if peers.is_empty() {
        println!("No peers discovered.");
    } else {
        println!(
            "{:<16}  {:<10}  {:<16}  {}",
            "NAME", "STATUS", "IP", "LATENCY"
        );
        println!("{}", "-".repeat(60));

        for peer in &peers {
            let name = peer["name"].as_str().unwrap_or("-");
            let status = peer["status"].as_str().unwrap_or("-");
            let ip = peer["tailscale_ip"]
                .as_str()
                .unwrap_or("-");
            let latency = peer["latency_ms"]
                .as_f64()
                .map(|ms| format!("{ms:.1}ms"))
                .unwrap_or_else(|| "-".to_string());

            println!("{:<16}  {:<10}  {:<16}  {}", name, status, ip, latency);
        }
    }

    Ok(())
}
