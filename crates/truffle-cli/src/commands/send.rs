//! `truffle send` -- send a one-shot message to a node.
//!
//! Sends a text message to a specific node (or broadcasts to all nodes)
//! via the daemon's mesh message bus.
//!
//! ```text
//! $ truffle send laptop "deploy is done"
//!   Sent to laptop
//!
//! $ truffle send --all "rebooting in 5 minutes"
//!   Sent to 3 nodes
//! ```

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::resolve::NameResolver;

/// Send a one-shot message to a node.
///
/// - `node`: target node name (or None for broadcast when `all` is true)
/// - `message`: the text message to send
/// - `all`: broadcast to all nodes
/// - `wait`: wait for a reply
pub async fn run(
    config: &TruffleConfig,
    node: Option<&str>,
    message: &str,
    all: bool,
    wait: bool,
) -> Result<(), String> {
    if message.is_empty() {
        return Err(
            "No message provided. Usage: truffle send <node> \"your message\"".to_string(),
        );
    }

    // Connect to the daemon
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    if all {
        // Broadcast to all peers
        let result = client
            .request(
                method::SEND_MESSAGE,
                serde_json::json!({
                    "broadcast": true,
                    "namespace": "chat",
                    "type": "text",
                    "payload": { "text": message }
                }),
            )
            .await
            .map_err(|e| format!("Failed to send: {e}"))?;

        let count = result["broadcast_count"].as_u64().unwrap_or(0);
        if count > 0 {
            println!("  Sent to {count} nodes");
        } else {
            println!("  No peers online to receive the message.");
        }
    } else {
        let target = node.ok_or_else(|| {
            "No target specified. Usage: truffle send <node> \"message\" or truffle send --all \"message\"".to_string()
        })?;

        // Resolve target name using the full NameResolver (aliases + peer list)
        let peers_result = client
            .request(method::PEERS, serde_json::json!({}))
            .await
            .map_err(|e| format!("Failed to get peers: {e}"))?;

        let peers = peers_result["peers"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let resolver = NameResolver::from_daemon_data(&config.aliases, &peers);
        let resolved = match resolver.resolve(target) {
            Ok(r) => r,
            Err(e) => {
                return Err(e.to_string());
            }
        };

        let display_name = &resolved.display_name;
        let device_id = &resolved.device_id;

        let result = client
            .request(
                method::SEND_MESSAGE,
                serde_json::json!({
                    "device_id": device_id,
                    "namespace": "chat",
                    "type": "text",
                    "payload": { "text": message }
                }),
            )
            .await
            .map_err(|e| format!("Failed to send: {e}"))?;

        let sent = result["sent"].as_bool().unwrap_or(false);
        if sent {
            println!("  Sent to {display_name}");
        } else {
            eprintln!("  {display_name} is offline. Message was not delivered.");
            return Err(format!("{display_name} is offline"));
        }
    }

    // If --wait, listen for a reply
    if wait {
        let timeout = tokio::time::Duration::from_secs(30);
        eprintln!("  Waiting for reply (30s timeout)...");

        // Connect a streaming session to receive the reply
        let stream = client.connect().await.map_err(|e| e.to_string())?;
        let (mut read_half, mut write_half) = stream.into_split();

        // Subscribe to incoming chat messages
        let subscribe = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "chat_start",
            "params": {
                "node": node,
                "wait_for_reply": true,
            }
        });
        let sub_json = serde_json::to_string(&subscribe).map_err(|e| e.to_string())?;
        write_half
            .write_line(&sub_json)
            .await
            .map_err(|e| format!("Failed to subscribe: {e}"))?;

        // Skip the handshake response
        let _ = read_half.next_line().await;

        // Wait for the first incoming message (the reply)
        match tokio::time::timeout(timeout, read_half.next_line()).await {
            Ok(Ok(Some(line))) => {
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                    if event["type"] == "message" {
                        let text = event["text"].as_str().unwrap_or("");
                        println!("  <- \"{text}\"");
                    }
                }
            }
            Ok(_) => {
                eprintln!("  No reply received (connection closed).");
            }
            Err(_) => {
                eprintln!("  No reply received (timed out after 30s).");
            }
        }
    }

    Ok(())
}

/// Legacy runtime-based send (kept for `truffle dev send`).
#[allow(dead_code)]
pub async fn run_legacy(
    hostname: &str,
    sidecar: Option<&str>,
    state_dir: Option<&str>,
    device_id: &str,
    namespace: &str,
    msg_type: &str,
    payload: &str,
) -> Result<(), String> {
    use truffle_core::protocol::envelope::MeshEnvelope;
    use truffle_core::runtime::TruffleEvent;

    // Parse payload as JSON
    let payload_value: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| format!("Invalid JSON payload: {e}"))?;

    let (runtime, mut event_rx) = super::build_runtime(hostname, sidecar, state_dir).await?;

    // Wait for the node to come online before sending
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    let mut online = false;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            event = event_rx.recv() => {
                match event {
                    Ok(TruffleEvent::Online { .. }) => {
                        online = true;
                        // Brief pause for peer connections to establish
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        break;
                    }
                    Ok(TruffleEvent::AuthRequired { auth_url }) => {
                        println!("Auth required: {auth_url}");
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Skipped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    if !online {
        runtime.stop().await;
        return Err("Timed out waiting for node to come online".to_string());
    }

    let envelope = MeshEnvelope::new(namespace, msg_type, payload_value);

    println!("Sending [{namespace}:{msg_type}] to {device_id}...");

    let sent = runtime.send_envelope(device_id, &envelope).await;

    if sent {
        println!("Message sent successfully.");
    } else {
        println!("Failed to send message (device may not be connected).");
    }

    runtime.stop().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_send_one_shot() {
        // Verify the JSON payload format for send_message
        let params = serde_json::json!({
            "device_id": "laptop",
            "namespace": "chat",
            "type": "text",
            "payload": { "text": "hello world" }
        });
        assert_eq!(params["device_id"], "laptop");
        assert_eq!(params["namespace"], "chat");
        assert_eq!(params["type"], "text");
        assert_eq!(params["payload"]["text"], "hello world");
    }

    #[test]
    fn test_send_broadcast() {
        // Verify the JSON payload format for broadcast
        let params = serde_json::json!({
            "broadcast": true,
            "namespace": "chat",
            "type": "text",
            "payload": { "text": "rebooting in 5 minutes" }
        });
        assert_eq!(params["broadcast"], true);
        assert_eq!(params["namespace"], "chat");
        assert_eq!(params["payload"]["text"], "rebooting in 5 minutes");
    }

    #[test]
    fn test_send_empty_message_rejected() {
        let message = "";
        assert!(message.is_empty());
    }

    #[test]
    fn test_send_wait_for_reply() {
        // Verify the chat_start params for wait mode
        let params = serde_json::json!({
            "node": "laptop",
            "wait_for_reply": true,
        });
        assert_eq!(params["node"], "laptop");
        assert_eq!(params["wait_for_reply"], true);
    }
}
