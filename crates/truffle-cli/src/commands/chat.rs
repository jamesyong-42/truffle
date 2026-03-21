//! `truffle chat` -- live chat with another node.
//!
//! Opens an interactive chat session with a target node (or broadcasts to
//! all nodes if no target is specified). Messages are sent and received
//! via the daemon's mesh message bus.
//!
//! ```text
//! $ truffle chat laptop
//!
//!   Connected to laptop (james-macbook -> laptop)
//!   Type your message and press Enter. Ctrl+C to exit.
//!   --------------------------------------------------
//!
//!   [14:23] you: hey, is the deploy done?
//!   [14:23] laptop: yep, just finished. all green.
//!   [14:24] you: nice, shipping to prod now
//! ```

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;

/// Start a chat session with a node (or broadcast if `node` is None).
///
/// Uses the daemon's message bus to send and receive chat messages.
/// The streaming protocol over Unix socket carries newline-delimited
/// JSON events in both directions.
pub async fn run(config: &TruffleConfig, node: Option<&str>) -> Result<(), String> {
    // Connect to the daemon
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    // Get a connection for the streaming chat session
    let stream = client.connect().await.map_err(|e| e.to_string())?;
    let (read_half, mut write_half) = stream.into_split();

    // Send chat_start request to set up the streaming session
    use tokio::io::AsyncWriteExt;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "chat_start",
        "params": {
            "node": node,
        }
    });
    let mut req_json = serde_json::to_string(&request).map_err(|e| e.to_string())?;
    req_json.push('\n');
    write_half
        .write_all(req_json.as_bytes())
        .await
        .map_err(|e| format!("Failed to send chat request: {e}"))?;

    // Read the handshake response
    use tokio::io::AsyncBufReadExt;
    let mut buf_reader = tokio::io::BufReader::new(read_half);
    let mut response_line = String::new();
    buf_reader
        .read_line(&mut response_line)
        .await
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let response: serde_json::Value = serde_json::from_str(response_line.trim())
        .map_err(|e| format!("Invalid response: {e}"))?;

    if let Some(err) = response.get("error") {
        let msg = err["message"].as_str().unwrap_or("unknown");
        return Err(format!("Chat setup failed: {msg}"));
    }

    let local_name = response["result"]["local_name"]
        .as_str()
        .unwrap_or("you")
        .to_string();
    let remote_name = node.unwrap_or("everyone");

    // Display chat header
    if let Some(target) = node {
        eprintln!();
        eprintln!("  Connected to {target} ({local_name} -> {target})");
        eprintln!("  Type your message and press Enter. Ctrl+C to exit.");
        eprintln!("  {}", "-".repeat(50));
        eprintln!();
    } else {
        // Get peer count from daemon for group chat header
        let peer_count = match client
            .request(method::PEERS, serde_json::json!({}))
            .await
        {
            Ok(result) => result["peers"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0),
            Err(_) => 0,
        };
        eprintln!();
        eprintln!("  Mesh chat ({peer_count} nodes online). Ctrl+C to exit.");
        eprintln!("  {}", "-".repeat(50));
        eprintln!();
    }

    // Enter chat loop
    let read_half = buf_reader.into_inner();
    chat_loop(read_half, write_half, &local_name, remote_name).await?;

    eprintln!("\nChat ended.");
    Ok(())
}

/// Interactive chat loop.
///
/// Reads lines from stdin (user messages) and sends them as JSON events
/// over the Unix socket. Reads JSON events from the socket (incoming
/// messages from remote nodes) and displays them.
async fn chat_loop(
    read_half: tokio::net::unix::OwnedReadHalf,
    mut write_half: tokio::net::unix::OwnedWriteHalf,
    local_name: &str,
    _remote_name: &str,
) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let stdin = tokio::io::stdin();
    let stdin_reader = tokio::io::BufReader::new(stdin);
    let mut stdin_lines = stdin_reader.lines();

    let socket_reader = tokio::io::BufReader::new(read_half);
    let mut socket_lines = socket_reader.lines();

    loop {
        tokio::select! {
            // Read a line from stdin -> send as chat message
            line = stdin_lines.next_line() => {
                match line {
                    Ok(Some(text)) => {
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            continue; // Don't send empty messages
                        }

                        // Handle /file command (stub)
                        if text.starts_with("/file ") {
                            let path = text.strip_prefix("/file ").unwrap_or("");
                            eprintln!("  File sharing not yet implemented: {path}");
                            continue;
                        }

                        // Display the sent message locally
                        let timestamp = format_timestamp();
                        println!("  [{timestamp}] {local_name}: {text}");

                        // Send as JSON event over the socket
                        let event = serde_json::json!({
                            "type": "message",
                            "text": text,
                        });
                        let mut event_json = serde_json::to_string(&event)
                            .map_err(|e| e.to_string())?;
                        event_json.push('\n');
                        if write_half.write_all(event_json.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break, // stdin EOF
                    Err(_) => break,
                }
            }
            // Read a JSON event from the socket -> display
            line = socket_lines.next_line() => {
                match line {
                    Ok(Some(text)) => {
                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&text) {
                            match event["type"].as_str() {
                                Some("message") => {
                                    let from = event["from"].as_str().unwrap_or("?");
                                    let msg = event["text"].as_str().unwrap_or("");
                                    let ts = event["ts"].as_str()
                                        .map(|s| s.to_string())
                                        .unwrap_or_else(format_timestamp);
                                    println!("  [{ts}] {from}: {msg}");
                                }
                                Some("presence") => {
                                    let node = event["node"].as_str().unwrap_or("?");
                                    let status = event["status"].as_str().unwrap_or("?");
                                    eprintln!("  * {node} is {status}");
                                }
                                Some("error") => {
                                    let msg = event["message"].as_str().unwrap_or("unknown error");
                                    eprintln!("  Error: {msg}");
                                }
                                _ => {
                                    // Unknown event type, display raw
                                    eprintln!("  ? {text}");
                                }
                            }
                        }
                    }
                    Ok(None) => break, // Socket closed
                    Err(_) => break,
                }
            }
        }
    }

    Ok(())
}

/// Format the current time as `HH:MM` for chat timestamps.
fn format_timestamp() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let hours = (now % 86400) / 3600;
    let minutes = (now % 3600) / 60;
    format!("{hours:02}:{minutes:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_message_format() {
        // Verify the JSON event format for outgoing messages
        let event = serde_json::json!({
            "type": "message",
            "text": "hello world",
        });
        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains("\"type\":\"message\""));
        assert!(json_str.contains("\"text\":\"hello world\""));
    }

    #[test]
    fn test_chat_incoming_message_parse() {
        // Verify parsing of incoming message events
        let json = r#"{"type":"message","from":"laptop","text":"hello","ts":"14:23"}"#;
        let event: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(event["type"], "message");
        assert_eq!(event["from"], "laptop");
        assert_eq!(event["text"], "hello");
        assert_eq!(event["ts"], "14:23");
    }

    #[test]
    fn test_chat_presence_event_parse() {
        let json = r#"{"type":"presence","node":"laptop","status":"typing"}"#;
        let event: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(event["type"], "presence");
        assert_eq!(event["node"], "laptop");
        assert_eq!(event["status"], "typing");
    }

    #[test]
    fn test_timestamp_format() {
        let ts = format_timestamp();
        // Should be HH:MM format
        assert_eq!(ts.len(), 5);
        assert_eq!(&ts[2..3], ":");
    }

    #[test]
    fn test_empty_message_skipped() {
        // Empty strings and whitespace-only should not be sent
        let text = "   ".trim();
        assert!(text.is_empty());
    }

    #[test]
    fn test_file_command_detection() {
        let text = "/file screenshot.png";
        assert!(text.starts_with("/file "));
        let path = text.strip_prefix("/file ").unwrap();
        assert_eq!(path, "screenshot.png");
    }
}
