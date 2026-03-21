//! `truffle ws` -- WebSocket REPL.
//!
//! Opens a WebSocket connection to a target node and provides an interactive
//! REPL where typed lines become WebSocket text frames and received frames
//! are printed to stdout.
//!
//! ```text
//! $ truffle ws server:8080/events
//! Connected to ws://server:8080/events
//! Type messages, one per line. Press Ctrl+C to disconnect.
//!
//! > {"type":"subscribe","channel":"deploy"}
//! < {"type":"subscribed","channel":"deploy"}
//! < {"type":"event","data":"build started"}
//! ```
//!
//! Input lines are prefixed with `>` (sent), output with `<` (received).

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::target::Target;

/// Open a WebSocket connection to a target.
///
/// - `json`: pretty-print JSON frames
/// - `binary`: send stdin as binary frames instead of text
pub async fn run(
    config: &TruffleConfig,
    target: &str,
    json: bool,
    binary: bool,
) -> Result<(), String> {
    // Parse the target address
    let resolved = config.resolve_alias(target);
    let parsed = Target::parse(resolved).map_err(|e| format!("Invalid target: {e}"))?;

    let node = &parsed.node;
    let port = parsed.port.ok_or_else(|| {
        format!("Which port? Usage: truffle ws {node}:<port>[/path]")
    })?;

    let path = parsed.path.as_deref().unwrap_or("/");
    let scheme = parsed
        .scheme
        .as_deref()
        .unwrap_or("ws");

    let ws_url = format!("{scheme}://{node}:{port}{path}");

    // Connect to the daemon
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    // Get a connection for the streaming session
    let stream = client.connect().await.map_err(|e| e.to_string())?;
    let (read_half, mut write_half) = stream.into_split();

    // Send the ws_connect request
    use tokio::io::AsyncWriteExt;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method::WS_CONNECT,
        "params": {
            "url": ws_url,
            "node": node,
            "port": port,
            "path": path,
            "binary": binary,
            "json": json,
        }
    });
    let mut req_json = serde_json::to_string(&request).map_err(|e| e.to_string())?;
    req_json.push('\n');
    write_half
        .write_all(req_json.as_bytes())
        .await
        .map_err(|e| format!("Failed to send request: {e}"))?;

    // Read the upgrade response
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
        return Err(format!(
            "{node}:{port}{path} didn't accept the WebSocket upgrade. {msg}"
        ));
    }

    eprintln!("Connected to {ws_url}");
    eprintln!("Type messages, one per line. Press Ctrl+C to disconnect.");

    // WebSocket REPL: lines from stdin become frames, received frames print to stdout
    let read_half = buf_reader.into_inner();
    ws_repl(read_half, write_half, json).await?;

    eprintln!("\nDisconnected.");
    Ok(())
}

/// WebSocket REPL loop.
///
/// Reads lines from stdin and sends them as text frames (via the Unix socket).
/// Reads lines from the socket (received frames) and prints them to stdout.
///
/// In the streaming protocol, each line from stdin becomes a WS text frame,
/// and each received WS frame is written as a line to the CLI.
async fn ws_repl(
    read_half: tokio::net::unix::OwnedReadHalf,
    mut write_half: tokio::net::unix::OwnedWriteHalf,
    pretty_json: bool,
) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let stdin = tokio::io::stdin();
    let stdin_reader = tokio::io::BufReader::new(stdin);
    let mut stdin_lines = stdin_reader.lines();

    let socket_reader = tokio::io::BufReader::new(read_half);
    let mut socket_lines = socket_reader.lines();

    loop {
        tokio::select! {
            // Read a line from stdin -> send to daemon
            line = stdin_lines.next_line() => {
                match line {
                    Ok(Some(text)) => {
                        if text.is_empty() {
                            continue;
                        }
                        eprintln!("> {text}");
                        let mut msg = text;
                        msg.push('\n');
                        if write_half.write_all(msg.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break, // EOF
                    Err(_) => break,
                }
            }
            // Read a line from socket -> print to stdout
            line = socket_lines.next_line() => {
                match line {
                    Ok(Some(text)) => {
                        if pretty_json {
                            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                                let pretty = serde_json::to_string_pretty(&parsed)
                                    .unwrap_or(text);
                                println!("< {pretty}");
                            } else {
                                println!("< {text}");
                            }
                        } else {
                            println!("< {text}");
                        }
                    }
                    Ok(None) => break, // Remote closed
                    Err(_) => break,
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::target::Target;

    #[test]
    fn test_ws_target_with_path() {
        let t = Target::parse("server:8080/events").unwrap();
        assert_eq!(t.node, "server");
        assert_eq!(t.port, Some(8080));
        assert_eq!(t.path, Some("/events".to_string()));
    }

    #[test]
    fn test_ws_target_with_scheme() {
        let t = Target::parse("ws://server:8080/events").unwrap();
        assert_eq!(t.node, "server");
        assert_eq!(t.port, Some(8080));
        assert_eq!(t.path, Some("/events".to_string()));
        assert_eq!(t.scheme, Some("ws".to_string()));
    }

    #[test]
    fn test_ws_target_wss_scheme() {
        let t = Target::parse("wss://server:443/secure").unwrap();
        assert_eq!(t.node, "server");
        assert_eq!(t.port, Some(443));
        assert_eq!(t.path, Some("/secure".to_string()));
        assert_eq!(t.scheme, Some("wss".to_string()));
    }

    #[test]
    fn test_ws_target_deep_path() {
        let t = Target::parse("server:3000/api/v2/ws").unwrap();
        assert_eq!(t.node, "server");
        assert_eq!(t.port, Some(3000));
        assert_eq!(t.path, Some("/api/v2/ws".to_string()));
    }

    #[test]
    fn test_ws_target_requires_port() {
        let t = Target::parse("server").unwrap();
        assert_eq!(t.port, None);
    }
}
