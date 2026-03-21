//! `truffle tcp` -- raw TCP connection (like netcat).
//!
//! Opens a bidirectional TCP stream to a target node:port via the daemon.
//! For interactive use, stdin is piped to the remote and remote data is
//! written to stdout. Fully pipe-compatible:
//!
//! ```text
//! echo "SELECT 1;" | truffle tcp server:5432
//! ```
//!
//! # Streaming protocol
//!
//! 1. CLI sends a `tcp_connect` JSON-RPC request to the daemon.
//! 2. Daemon establishes the TCP connection and returns `{ "stream": true }`.
//! 3. The Unix socket becomes a raw byte pipe -- stdin -> socket, socket -> stdout.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::target::Target;

/// Open a raw TCP connection to a target.
///
/// When `check` is true, only tests connectivity (connect + disconnect)
/// without entering interactive mode.
pub async fn run(config: &TruffleConfig, target: &str, check: bool) -> Result<(), String> {
    // Parse the target address
    let resolved = config.resolve_alias(target);
    let parsed = Target::parse(resolved).map_err(|e| format!("Invalid target: {e}"))?;

    let node = &parsed.node;
    let port = parsed.port.ok_or_else(|| {
        format!("Which port? Usage: truffle tcp {node}:<port>")
    })?;

    let target_str = format!("{node}:{port}");

    // Connect to the daemon
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    // Send tcp_connect request
    let result = client
        .request(
            method::TCP_CONNECT,
            serde_json::json!({
                "target": target_str,
                "node": node,
                "port": port,
                "check": check,
            }),
        )
        .await
        .map_err(|e| format!("Failed to connect: {e}"))?;

    if check {
        // Check mode: just report connectivity
        let connected = result["connected"].as_bool().unwrap_or(false);
        if connected {
            eprintln!("Connection to {target_str} succeeded.");
        } else {
            let reason = result["reason"].as_str().unwrap_or("unknown error");
            return Err(format!(
                "Can't connect to {target_str}. {reason}\n\n  Try:\n    truffle ping {node}    check if the node is reachable"
            ));
        }
        return Ok(());
    }

    // Interactive mode: pipe stdin/stdout through the daemon socket
    let stream_active = result["stream"].as_bool().unwrap_or(false);
    if !stream_active {
        return Err(
            "Daemon did not upgrade to streaming mode. The TCP connection may have failed."
                .to_string(),
        );
    }

    eprintln!("Connected to {target_str}");
    eprintln!("Type to send data. Press Ctrl+C to disconnect.");

    // Get a fresh connection for the streaming session
    let stream = client.connect().await.map_err(|e| e.to_string())?;
    let (read_half, mut write_half) = stream.into_split();

    // Send the streaming request on this connection
    use tokio::io::AsyncWriteExt;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method::TCP_CONNECT,
        "params": {
            "target": target_str,
            "node": node,
            "port": port,
            "stream": true,
        }
    });
    let mut req_json = serde_json::to_string(&request).map_err(|e| e.to_string())?;
    req_json.push('\n');
    write_half
        .write_all(req_json.as_bytes())
        .await
        .map_err(|e| format!("Failed to send stream request: {e}"))?;

    // Read the upgrade response
    use tokio::io::AsyncBufReadExt;
    let mut buf_reader = tokio::io::BufReader::new(read_half);
    let mut response_line = String::new();
    buf_reader
        .read_line(&mut response_line)
        .await
        .map_err(|e| format!("Failed to read stream response: {e}"))?;

    let response: serde_json::Value = serde_json::from_str(response_line.trim())
        .map_err(|e| format!("Invalid stream response: {e}"))?;

    if let Some(err) = response.get("error") {
        return Err(format!(
            "Stream setup failed: {}",
            err["message"].as_str().unwrap_or("unknown")
        ));
    }

    // Now pipe stdin/stdout through the socket
    let read_half = buf_reader.into_inner();
    crate::stream::pipe_stdio(read_half, write_half)
        .await
        .map_err(|e| format!("Connection error: {e}"))?;

    eprintln!("\nDisconnected.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::target::Target;

    #[test]
    fn test_tcp_target_parsing() {
        let t = Target::parse("server:5432").unwrap();
        assert_eq!(t.node, "server");
        assert_eq!(t.port, Some(5432));

        let t = Target::parse("laptop:8080").unwrap();
        assert_eq!(t.node, "laptop");
        assert_eq!(t.port, Some(8080));
    }

    #[test]
    fn test_tcp_target_requires_port() {
        let t = Target::parse("server").unwrap();
        assert_eq!(t.port, None); // No port -> command should error
    }

    #[test]
    fn test_tcp_target_with_ip() {
        let t = Target::parse("100.64.0.3:5432").unwrap();
        assert_eq!(t.node, "100.64.0.3");
        assert_eq!(t.port, Some(5432));
    }

    #[test]
    fn test_tcp_target_with_scheme() {
        let t = Target::parse("tcp://db:5432").unwrap();
        assert_eq!(t.node, "db");
        assert_eq!(t.port, Some(5432));
        assert_eq!(t.scheme, Some("tcp".to_string()));
    }
}
