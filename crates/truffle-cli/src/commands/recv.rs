//! `truffle recv` -- block for the next incoming message.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::output;

/// Exit code for timeout.

pub async fn run(
    config: &TruffleConfig,
    from: Option<&str>,
    timeout: Option<u64>,
    json: bool,
) -> Result<(), (i32, String)> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| (crate::exit_codes::NOT_ONLINE, e.to_string()))?;

    let params = if let Some(from_node) = from {
        serde_json::json!({
            "events": ["message"],
            "filter": { "peer": from_node },
        })
    } else {
        serde_json::json!({
            "events": ["message"],
        })
    };

    let timeout_duration = timeout.map(std::time::Duration::from_secs);

    if !json {
        let filter_msg = match from {
            Some(f) => format!(" from {}", output::bold(f)),
            None => String::new(),
        };
        let timeout_msg = match timeout {
            Some(t) => format!(" (timeout: {}s)", t),
            None => String::new(),
        };
        println!("  Waiting for message{}{}...", filter_msg, timeout_msg,);
    }

    let mut received_msg: Option<serde_json::Value> = None;

    let result = client
        .subscribe(params, timeout_duration, |notif| {
            let event_type = notif.params["type"].as_str().unwrap_or("");
            if event_type == "message.received" {
                received_msg = Some(notif.params.clone());
                return true; // Got our message, stop
            }
            false
        })
        .await;

    match result {
        Ok(()) => {
            if let Some(msg) = received_msg {
                if json {
                    output::print_json(&msg);
                } else {
                    let from_peer = msg["from"].as_str().unwrap_or("?");
                    let payload = &msg["payload"];
                    let text = if let Some(s) = payload.as_str() {
                        s.to_string()
                    } else {
                        payload.to_string()
                    };
                    println!();
                    output::print_success(&format!(
                        "Message from {}: {}",
                        output::bold(from_peer),
                        text,
                    ));
                    println!();
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
                }));
            } else {
                output::print_error(
                    "Timed out waiting for a message",
                    "No message was received within the timeout period.",
                    "truffle send <node> <msg>    send a message first",
                );
            }
            Err((
                crate::exit_codes::TIMEOUT,
                "Timed out waiting for a message".to_string(),
            ))
        }
        Err(e) => Err((crate::exit_codes::ERROR, e.to_string())),
    }
}
