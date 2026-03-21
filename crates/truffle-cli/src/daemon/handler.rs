//! Request handler: dispatches JSON-RPC methods to `TruffleRuntime`.
//!
//! Each method is dispatched to a handler function that interacts with the
//! runtime and returns a JSON-RPC response.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Notify;
use truffle_core::runtime::TruffleRuntime;

use super::protocol::{error_code, method, DaemonRequest, DaemonResponse};

/// Dispatch a JSON-RPC request to the appropriate handler.
///
/// Returns a `DaemonResponse` with either a result or an error.
pub async fn dispatch(
    request: &DaemonRequest,
    runtime: &Arc<TruffleRuntime>,
    started_at: Instant,
    shutdown_signal: &Arc<Notify>,
) -> DaemonResponse {
    match request.method.as_str() {
        method::STATUS => handle_status(request.id, runtime, started_at).await,
        method::PEERS => handle_peers(request.id, runtime).await,
        method::PING => handle_ping(request.id, &request.params, runtime).await,
        method::SEND_MESSAGE => handle_send_message(request.id, &request.params, runtime).await,
        method::SHUTDOWN => handle_shutdown(request.id, shutdown_signal),
        _ => DaemonResponse::error(
            request.id,
            error_code::METHOD_NOT_FOUND,
            format!("Method '{}' not found", request.method),
        ),
    }
}

/// Handle `status` -- return node status information.
async fn handle_status(id: u64, runtime: &Arc<TruffleRuntime>, started_at: Instant) -> DaemonResponse {
    let device = runtime.local_device().await;
    let uptime_secs = started_at.elapsed().as_secs();

    DaemonResponse::success(
        id,
        serde_json::json!({
            "device_id": device.id,
            "name": device.name,
            "device_type": device.device_type,
            "hostname": device.tailscale_hostname,
            "status": format!("{:?}", device.status),
            "tailscale_ip": device.tailscale_ip,
            "tailscale_dns_name": device.tailscale_dns_name,
            "uptime_secs": uptime_secs,
        }),
    )
}

/// Handle `peers` -- return the list of discovered peers.
async fn handle_peers(id: u64, runtime: &Arc<TruffleRuntime>) -> DaemonResponse {
    let local_id = runtime.device_id().await;
    let devices = runtime.devices().await;

    let peers: Vec<serde_json::Value> = devices
        .into_iter()
        .filter(|d| d.id != local_id)
        .map(|d| {
            serde_json::json!({
                "id": d.id,
                "name": d.name,
                "device_type": d.device_type,
                "status": format!("{:?}", d.status),
                "tailscale_ip": d.tailscale_ip,
                "tailscale_dns_name": d.tailscale_dns_name,
                "latency_ms": d.latency_ms,
            })
        })
        .collect();

    DaemonResponse::success(id, serde_json::json!({ "peers": peers }))
}

/// Handle `ping` -- connectivity check to a node.
async fn handle_ping(
    id: u64,
    params: &serde_json::Value,
    runtime: &Arc<TruffleRuntime>,
) -> DaemonResponse {
    let node = match params.get("node").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'node'",
            );
        }
    };

    // For now, check if the node is in our peer list
    let devices = runtime.devices().await;
    let found = devices.iter().any(|d| d.name == node || d.id == node);

    if found {
        DaemonResponse::success(
            id,
            serde_json::json!({
                "node": node,
                "reachable": true,
            }),
        )
    } else {
        DaemonResponse::success(
            id,
            serde_json::json!({
                "node": node,
                "reachable": false,
                "reason": "Node not found in peer list",
            }),
        )
    }
}

/// Handle `send_message` -- send a mesh message to a specific device.
async fn handle_send_message(
    id: u64,
    params: &serde_json::Value,
    runtime: &Arc<TruffleRuntime>,
) -> DaemonResponse {
    let device_id = match params.get("device_id").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'device_id'",
            );
        }
    };

    let namespace = match params.get("namespace").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'namespace'",
            );
        }
    };

    let msg_type = match params.get("type").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'type'",
            );
        }
    };

    let payload = params
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    let envelope =
        truffle_core::protocol::envelope::MeshEnvelope::new(namespace, msg_type, payload);
    let sent = runtime.send_envelope(device_id, &envelope).await;

    DaemonResponse::success(
        id,
        serde_json::json!({
            "sent": sent,
            "device_id": device_id,
        }),
    )
}

/// Handle `shutdown` -- gracefully stop the daemon.
fn handle_shutdown(id: u64, shutdown_signal: &Arc<Notify>) -> DaemonResponse {
    // Signal the main loop to shut down
    shutdown_signal.notify_one();
    DaemonResponse::success(id, serde_json::json!({ "shutting_down": true }))
}
