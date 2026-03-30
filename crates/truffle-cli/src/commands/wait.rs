//! `truffle wait <node>` -- block until a peer comes online.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::output;

/// Exit code for timeout (matches exit_codes::TIMEOUT in the spec).

pub async fn run(
    config: &TruffleConfig,
    node: &str,
    timeout: Option<u64>,
    json: bool,
) -> Result<(), (i32, String)> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| (crate::exit_codes::NOT_ONLINE, e.to_string()))?;

    let node_lower = node.to_lowercase();

    // First, check if the peer is already online via the peers list.
    if let Ok(result) = client
        .request(
            crate::daemon::protocol::method::PEERS,
            serde_json::json!({}),
        )
        .await
    {
        if let Some(peers) = result["peers"].as_array() {
            for p in peers {
                let name = p["name"].as_str().unwrap_or("");
                let online = p["online"].as_bool().unwrap_or(false);
                if name.to_lowercase().contains(&node_lower) && online {
                    // Already online!
                    if json {
                        output::print_json(p);
                    } else {
                        let ip = p["ip"].as_str().unwrap_or("");
                        output::print_success(&format!(
                            "{} is already online{}",
                            output::bold(name),
                            if ip.is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", ip)
                            },
                        ));
                    }
                    return Ok(());
                }
            }
        }
    }

    // Not online yet -- subscribe to peer events and wait.
    if !json {
        let timeout_msg = match timeout {
            Some(t) => format!(" (timeout: {}s)", t),
            None => String::new(),
        };
        println!(
            "  Waiting for {} to come online{}...",
            output::bold(node),
            timeout_msg,
        );
    }

    let params = serde_json::json!({
        "events": ["peer"],
        "filter": { "peer": node },
    });

    let timeout_duration = timeout.map(std::time::Duration::from_secs);
    let node_filter = node_lower.clone();

    let mut matched_peer: Option<serde_json::Value> = None;

    let result = client
        .subscribe(params, timeout_duration, |notif| {
            let event_type = notif.params["type"].as_str().unwrap_or("");
            let peer_name = notif.params["peer"].as_str().unwrap_or("");

            // Match on peer.joined or peer.connected events.
            if (event_type == "peer.joined" || event_type == "peer.connected")
                && peer_name.to_lowercase().contains(&node_filter)
            {
                matched_peer = Some(notif.params.clone());
                return true; // Stop subscribing
            }

            false
        })
        .await;

    match result {
        Ok(()) => {
            if let Some(peer_info) = matched_peer {
                if json {
                    output::print_json(&peer_info);
                } else {
                    let peer = peer_info["peer"].as_str().unwrap_or(node);
                    let ip = peer_info["ip"].as_str().unwrap_or("");
                    output::print_success(&format!(
                        "{} is online{}",
                        output::bold(peer),
                        if ip.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", ip)
                        },
                    ));
                }
                Ok(())
            } else {
                Ok(())
            }
        }
        Err(crate::daemon::client::ClientError::DaemonError { code: 3, .. }) => {
            if json {
                output::print_json(&serde_json::json!({
                    "error": "timeout",
                    "node": node,
                }));
            } else {
                output::print_error(
                    &format!("Timed out waiting for {}", node),
                    "The node did not come online within the timeout period.",
                    "truffle ls    see who's on the mesh",
                );
            }
            Err((crate::exit_codes::TIMEOUT, format!("Timed out waiting for {node}")))
        }
        Err(e) => Err((crate::exit_codes::ERROR, e.to_string())),
    }
}
