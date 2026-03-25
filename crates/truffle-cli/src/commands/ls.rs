//! `truffle ls` -- list peers on the mesh.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output;

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

    if json {
        output::print_json(&serde_json::json!({ "peers": peers }));
        return Ok(());
    }

    // Filter offline peers unless --all
    let filtered: Vec<&serde_json::Value> = peers
        .iter()
        .filter(|p| all || p["online"].as_bool().unwrap_or(false))
        .collect();

    if filtered.is_empty() {
        println!();
        if all {
            println!("  No peers discovered yet.");
        } else {
            println!("  No peers online. Use {} to include offline peers.", output::bold("--all"));
        }
        println!();
        return Ok(());
    }

    println!();

    if long {
        let headers = &["NODE", "STATUS", "IP", "CONNECTION", "CONNECTED", "OS"];
        let rows: Vec<Vec<String>> = filtered
            .iter()
            .map(|p| {
                let name = p["name"].as_str().unwrap_or("-");
                let online = p["online"].as_bool().unwrap_or(false);
                let ip = p["ip"].as_str().unwrap_or("-");
                let conn_type = p["connection_type"].as_str().unwrap_or("-");
                let connected = p["connected"].as_bool().unwrap_or(false);
                let os = p["os"].as_str().unwrap_or("-");

                vec![
                    output::bold(name),
                    if online {
                        format!("{} {}", output::status_indicator("online"), output::status_label("online"))
                    } else {
                        format!("{} {}", output::status_indicator("offline"), output::status_label("offline"))
                    },
                    ip.to_string(),
                    output::format_connection(Some(conn_type)),
                    if connected { "yes".to_string() } else { "no".to_string() },
                    os.to_string(),
                ]
            })
            .collect();

        output::print_table(headers, &rows);
    } else {
        let headers = &["NODE", "STATUS", "CONNECTION"];
        let rows: Vec<Vec<String>> = filtered
            .iter()
            .map(|p| {
                let name = p["name"].as_str().unwrap_or("-");
                let online = p["online"].as_bool().unwrap_or(false);
                let conn_type = p["connection_type"].as_str().unwrap_or("-");

                vec![
                    output::bold(name),
                    if online {
                        format!("{} {}", output::status_indicator("online"), output::status_label("online"))
                    } else {
                        format!("{} {}", output::status_indicator("offline"), output::status_label("offline"))
                    },
                    output::format_connection(Some(conn_type)),
                ]
            })
            .collect();

        output::print_table(headers, &rows);
    }

    println!();
    Ok(())
}
