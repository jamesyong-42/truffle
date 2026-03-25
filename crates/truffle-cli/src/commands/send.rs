//! `truffle send` -- send a one-shot message to a node.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output;

pub async fn run(
    config: &TruffleConfig,
    node: &str,
    message: &str,
    _all: bool,
    _wait: bool,
) -> Result<(), String> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    let result = client
        .request(
            method::SEND_MESSAGE,
            serde_json::json!({
                "peer_id": node,
                "namespace": "chat",
                "message": message,
            }),
        )
        .await
        .map_err(|e| e.to_string())?;

    let sent = result["sent"].as_bool().unwrap_or(false);
    if sent {
        output::print_success(&format!("Message sent to {}.", output::bold(node)));
    } else {
        output::print_error(
            &format!("Failed to send message to {}", node),
            "The node may not be connected.",
            "truffle ls    see who's online\ntruffle ping  check connectivity",
        );
    }

    Ok(())
}
