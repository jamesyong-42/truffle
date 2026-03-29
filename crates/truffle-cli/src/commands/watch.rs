//! `truffle watch` -- stream mesh events as JSONL.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::output;

pub async fn run(
    config: &TruffleConfig,
    json: bool,
    filters: &[String],
    timeout: Option<u64>,
) -> Result<(), (i32, String)> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| (crate::exit_codes::NOT_ONLINE, e.to_string()))?;

    // Build the subscribe params.
    let events: Vec<serde_json::Value> = if filters.is_empty() {
        // Subscribe to all event types by default.
        vec![]
    } else {
        filters
            .iter()
            .map(|f| serde_json::Value::String(f.clone()))
            .collect()
    };

    let params = if events.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::json!({ "events": events })
    };

    let timeout_duration = timeout.map(std::time::Duration::from_secs);

    if !json {
        output::print_section("Watching mesh events");
        println!("  Press Ctrl+C to stop.\n");
    }

    let result = client
        .subscribe(params, timeout_duration, |notif| {
            if json {
                // Output the notification params as a single JSON line.
                if let Ok(line) = serde_json::to_string(&notif.params) {
                    println!("{line}");
                }
            } else {
                print_human_event(notif);
            }
            // Never stop on its own -- run until Ctrl+C or timeout.
            false
        })
        .await;

    match result {
        Ok(()) => Ok(()),
        Err(crate::daemon::client::ClientError::DaemonError { code: 3, .. }) => {
            // Timeout exit code
            if !json {
                println!();
                output::print_warning("Timed out.");
            }
            Err((crate::exit_codes::TIMEOUT, "Watch timed out".to_string()))
        }
        Err(e) => Err((crate::exit_codes::ERROR, e.to_string())),
    }
}

fn print_human_event(notif: &crate::daemon::protocol::DaemonNotification) {
    let params = &notif.params;
    let event_type = params["type"].as_str().unwrap_or(&notif.method);
    let time = params["time"]
        .as_str()
        .map(|t| {
            // Try to parse and display just the time portion.
            chrono::DateTime::parse_from_rfc3339(t)
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_else(|_| t.to_string())
        })
        .unwrap_or_default();

    let time_str = if time.is_empty() {
        String::new()
    } else {
        output::dim(&format!("[{time}] "))
    };

    match event_type {
        "peer.joined" => {
            let peer = params["peer"].as_str().unwrap_or("?");
            let ip = params["ip"].as_str().unwrap_or("");
            println!(
                "  {}{} {} joined{}",
                time_str,
                output::Indicator::Online,
                output::bold(peer),
                if ip.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", output::dim(ip))
                },
            );
        }
        "peer.left" => {
            let peer = params["peer"].as_str().unwrap_or("?");
            println!(
                "  {}{} {} left",
                time_str,
                output::Indicator::Offline,
                output::bold(peer),
            );
        }
        "peer.updated" => {
            let peer = params["peer"].as_str().unwrap_or("?");
            let online = params["online"].as_bool().unwrap_or(false);
            let indicator = if online {
                output::Indicator::Online
            } else {
                output::Indicator::Offline
            };
            println!(
                "  {}{} {} updated",
                time_str, indicator,
                output::bold(peer),
            );
        }
        "peer.connected" => {
            let peer = params["peer"].as_str().unwrap_or("?");
            println!(
                "  {}{} {} connected",
                time_str,
                output::Indicator::Pass,
                output::bold(peer),
            );
        }
        "peer.disconnected" => {
            let peer = params["peer"].as_str().unwrap_or("?");
            println!(
                "  {}{} {} disconnected",
                time_str,
                output::Indicator::Warn,
                output::bold(peer),
            );
        }
        "message.received" => {
            let from = params["from"].as_str().unwrap_or("?");
            let payload = &params["payload"];
            let text = if let Some(s) = payload.as_str() {
                s.to_string()
            } else {
                payload.to_string()
            };
            println!(
                "  {}{} {}: {}",
                time_str,
                output::cyan("\u{2709}"),
                output::bold(from),
                text,
            );
        }
        "transfer.event" => {
            let from = params["from"].as_str().unwrap_or("?");
            let msg_type = params["msg_type"].as_str().unwrap_or("event");
            println!(
                "  {}{} transfer from {} ({})",
                time_str,
                output::cyan("\u{21c6}"),
                output::bold(from),
                msg_type,
            );
        }
        _ => {
            // Generic fallback
            println!(
                "  {}{} {}",
                time_str,
                output::dim("\u{2022}"),
                event_type,
            );
        }
    }
}
