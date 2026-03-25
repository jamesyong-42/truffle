//! Messaging application — send/receive messages via Node API.
//!
//! Uses `node.send()` for one-shot messages and `node.subscribe()` for
//! receiving. Pure Layer 7 — no protocol knowledge in truffle-core.

use truffle_core_v2::network::NetworkProvider;
use truffle_core_v2::node::Node;

/// Send a one-shot text message to a peer.
#[allow(dead_code)]
pub async fn send_message<N: NetworkProvider + 'static>(
    node: &Node<N>,
    peer_id: &str,
    message: &str,
) -> Result<(), String> {
    let payload = serde_json::json!({
        "type": "text",
        "text": message,
    });

    let data = serde_json::to_vec(&payload)
        .map_err(|e| format!("Failed to serialize message: {e}"))?;

    node.send(peer_id, "chat", &data)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Broadcast a text message to all connected peers.
#[allow(dead_code)]
pub async fn broadcast_message<N: NetworkProvider + 'static>(
    node: &Node<N>,
    message: &str,
) {
    let payload = serde_json::json!({
        "type": "text",
        "text": message,
    });

    if let Ok(data) = serde_json::to_vec(&payload) {
        node.broadcast("chat", &data).await;
    }
}
