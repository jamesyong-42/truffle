use truffle_core::protocol::envelope::MeshEnvelope;
use truffle_core::runtime::TruffleEvent;

use super::build_runtime;

/// Send a message to a specific device via the mesh bus.
///
/// **Legacy**: This function creates its own runtime. New code should use
/// `truffle send` or `truffle dev send` which goes through the daemon.
#[allow(dead_code)]
pub async fn run(
    hostname: &str,
    sidecar: Option<&str>,
    state_dir: Option<&str>,
    device_id: &str,
    namespace: &str,
    msg_type: &str,
    payload: &str,
) -> Result<(), String> {
    // Parse payload as JSON
    let payload_value: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| format!("Invalid JSON payload: {e}"))?;

    let (runtime, mut event_rx) = build_runtime(hostname, sidecar, state_dir).await?;

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

    println!(
        "Sending [{namespace}:{msg_type}] to {device_id}..."
    );

    let sent = runtime.send_envelope(device_id, &envelope).await;

    if sent {
        println!("Message sent successfully.");
    } else {
        println!("Failed to send message (device may not be connected).");
    }

    runtime.stop().await;
    Ok(())
}
