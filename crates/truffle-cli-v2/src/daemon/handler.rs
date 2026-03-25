//! Request handler: dispatches JSON-RPC methods to the Node API.
//!
//! Each method maps directly to Node methods. No GoShim, no BridgeManager,
//! no pending_dials -- pure Node API.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Notify;
use tracing::info;
use truffle_core_v2::node::Node;
use truffle_core_v2::network::tailscale::TailscaleProvider;

use super::protocol::{error_code, method, DaemonNotification, DaemonRequest, DaemonResponse};
use crate::apps;

/// Context bundling all daemon-owned resources needed by request handlers.
pub struct DaemonContext {
    pub node: Arc<Node<TailscaleProvider>>,
    pub shutdown_signal: Arc<Notify>,
    pub started_at: Instant,
}

/// Dispatch a JSON-RPC request to the appropriate handler.
pub async fn dispatch(
    request: &DaemonRequest,
    ctx: &DaemonContext,
    notification_tx: tokio::sync::mpsc::UnboundedSender<DaemonNotification>,
) -> DaemonResponse {
    let node = &ctx.node;
    let started_at = ctx.started_at;
    let shutdown_signal = &ctx.shutdown_signal;

    match request.method.as_str() {
        method::STATUS => handle_status(request.id, node, started_at).await,
        method::PEERS => handle_peers(request.id, node).await,
        method::PING => handle_ping(request.id, &request.params, node).await,
        method::SEND_MESSAGE => handle_send_message(request.id, &request.params, node).await,
        method::SHUTDOWN => handle_shutdown(request.id, shutdown_signal),
        method::TCP_CONNECT => handle_tcp_connect(request.id, &request.params, node).await,
        method::PUSH_FILE => {
            handle_push_file(request.id, &request.params, node, notification_tx).await
        }
        method::GET_FILE => {
            handle_get_file(request.id, &request.params, node, notification_tx).await
        }
        method::DOCTOR => handle_doctor(request.id, node).await,
        _ => DaemonResponse::error(
            request.id,
            error_code::METHOD_NOT_FOUND,
            format!("Method '{}' not found", request.method),
        ),
    }
}

// ==========================================================================
// Status
// ==========================================================================

async fn handle_status(
    id: u64,
    node: &Arc<Node<TailscaleProvider>>,
    started_at: Instant,
) -> DaemonResponse {
    let info = node.local_info();
    let peers = node.peers().await;
    let health = node.health().await;
    let uptime_secs = started_at.elapsed().as_secs();

    let status = if health.healthy { "online" } else { &health.state };
    let ip_str = info.ip.map(|ip| ip.to_string()).unwrap_or_default();

    DaemonResponse::success(
        id,
        serde_json::json!({
            "name": info.name,
            "hostname": info.hostname,
            "id": info.id,
            "ip": ip_str,
            "dns_name": info.dns_name.unwrap_or_default(),
            "status": status,
            "uptime_secs": uptime_secs,
            "peer_count": peers.len(),
            "health": {
                "state": health.state,
                "healthy": health.healthy,
                "key_expiry": health.key_expiry,
                "warnings": health.warnings,
            },
        }),
    )
}

// ==========================================================================
// Peers
// ==========================================================================

async fn handle_peers(
    id: u64,
    node: &Arc<Node<TailscaleProvider>>,
) -> DaemonResponse {
    let peers = node.peers().await;

    let peers_json: Vec<serde_json::Value> = peers
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "ip": p.ip.to_string(),
                "online": p.online,
                "connected": p.connected,
                "connection_type": p.connection_type,
                "os": p.os,
                "last_seen": p.last_seen,
            })
        })
        .collect();

    DaemonResponse::success(id, serde_json::json!({ "peers": peers_json }))
}

// ==========================================================================
// Ping
// ==========================================================================

async fn handle_ping(
    id: u64,
    params: &serde_json::Value,
    node: &Arc<Node<TailscaleProvider>>,
) -> DaemonResponse {
    let peer_id = match params["node"].as_str() {
        Some(n) => n,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing 'node' parameter",
            )
        }
    };

    match node.ping(peer_id).await {
        Ok(result) => DaemonResponse::success(
            id,
            serde_json::json!({
                "latency_ms": result.latency.as_secs_f64() * 1000.0,
                "connection": result.connection,
                "peer_addr": result.peer_addr,
            }),
        ),
        Err(e) => DaemonResponse::error(id, error_code::NODE_NOT_FOUND, e.to_string()),
    }
}

// ==========================================================================
// Send message
// ==========================================================================

async fn handle_send_message(
    id: u64,
    params: &serde_json::Value,
    node: &Arc<Node<TailscaleProvider>>,
) -> DaemonResponse {
    let peer_id = match params["peer_id"].as_str().or(params["device_id"].as_str()) {
        Some(n) => n,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing 'peer_id' parameter",
            )
        }
    };

    let namespace = params["namespace"].as_str().unwrap_or("chat");
    let message = params["message"]
        .as_str()
        .or(params["payload"].as_str())
        .unwrap_or("");

    match node.send(peer_id, namespace, message.as_bytes()).await {
        Ok(()) => DaemonResponse::success(id, serde_json::json!({ "sent": true })),
        Err(e) => DaemonResponse::error(id, error_code::INTERNAL_ERROR, e.to_string()),
    }
}

// ==========================================================================
// Shutdown
// ==========================================================================

fn handle_shutdown(id: u64, shutdown_signal: &Arc<Notify>) -> DaemonResponse {
    info!("Received shutdown request");
    shutdown_signal.notify_one();
    DaemonResponse::success(id, serde_json::json!({ "shutting_down": true }))
}

// ==========================================================================
// TCP connect
// ==========================================================================

async fn handle_tcp_connect(
    id: u64,
    params: &serde_json::Value,
    node: &Arc<Node<TailscaleProvider>>,
) -> DaemonResponse {
    let peer_id = match params["node"].as_str() {
        Some(n) => n,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing 'node' parameter",
            )
        }
    };

    let port = params["port"].as_u64().unwrap_or(0) as u16;
    if port == 0 {
        return DaemonResponse::error(
            id,
            error_code::INVALID_PARAMS,
            "Missing or invalid 'port' parameter",
        );
    }

    match node.open_tcp(peer_id, port).await {
        Ok(_stream) => {
            // For check mode, we just verify connectivity
            DaemonResponse::success(
                id,
                serde_json::json!({
                    "connected": true,
                    "peer": peer_id,
                    "port": port,
                }),
            )
        }
        Err(e) => DaemonResponse::error(id, error_code::INTERNAL_ERROR, e.to_string()),
    }
}

// ==========================================================================
// File transfer (push)
// ==========================================================================

async fn handle_push_file(
    id: u64,
    params: &serde_json::Value,
    node: &Arc<Node<TailscaleProvider>>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<DaemonNotification>,
) -> DaemonResponse {
    let peer_id = match params["peer_id"].as_str() {
        Some(n) => n,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing 'peer_id' parameter",
            )
        }
    };

    let local_path = match params["local_path"].as_str() {
        Some(p) => p,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing 'local_path' parameter",
            )
        }
    };

    let remote_path = match params["remote_path"].as_str() {
        Some(p) => p,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing 'remote_path' parameter",
            )
        }
    };

    let progress_cb = move |current: u64, total: u64, speed: f64| {
        let notif = DaemonNotification::new(
            super::protocol::notification::CP_PROGRESS,
            serde_json::json!({
                "bytes_sent": current,
                "total_bytes": total,
                "bytes_per_second": speed,
                "percent": if total > 0 { current as f64 / total as f64 * 100.0 } else { 0.0 },
            }),
        );
        let _ = notification_tx.send(notif);
    };

    match apps::file_transfer::upload(node, peer_id, local_path, remote_path, progress_cb).await {
        Ok(result) => DaemonResponse::success(
            id,
            serde_json::json!({
                "success": true,
                "bytes_transferred": result.bytes_transferred,
                "sha256": result.sha256,
                "elapsed_secs": result.elapsed_secs,
            }),
        ),
        Err(e) => DaemonResponse::error(id, error_code::INTERNAL_ERROR, e.to_string()),
    }
}

// ==========================================================================
// File transfer (get/download)
// ==========================================================================

async fn handle_get_file(
    id: u64,
    params: &serde_json::Value,
    node: &Arc<Node<TailscaleProvider>>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<DaemonNotification>,
) -> DaemonResponse {
    let peer_id = match params["peer_id"].as_str() {
        Some(n) => n,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing 'peer_id' parameter",
            )
        }
    };

    let remote_path = match params["remote_path"].as_str() {
        Some(p) => p,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing 'remote_path' parameter",
            )
        }
    };

    let local_path = match params["local_path"].as_str() {
        Some(p) => p,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing 'local_path' parameter",
            )
        }
    };

    let progress_cb = move |current: u64, total: u64, speed: f64| {
        let notif = DaemonNotification::new(
            super::protocol::notification::CP_PROGRESS,
            serde_json::json!({
                "bytes_received": current,
                "total_bytes": total,
                "bytes_per_second": speed,
                "percent": if total > 0 { current as f64 / total as f64 * 100.0 } else { 0.0 },
            }),
        );
        let _ = notification_tx.send(notif);
    };

    match apps::file_transfer::download(node, peer_id, remote_path, local_path, progress_cb).await
    {
        Ok(result) => DaemonResponse::success(
            id,
            serde_json::json!({
                "success": true,
                "bytes_transferred": result.bytes_transferred,
                "sha256": result.sha256,
                "elapsed_secs": result.elapsed_secs,
            }),
        ),
        Err(e) => DaemonResponse::error(id, error_code::INTERNAL_ERROR, e.to_string()),
    }
}

// ==========================================================================
// Doctor
// ==========================================================================

async fn handle_doctor(
    id: u64,
    node: &Arc<Node<TailscaleProvider>>,
) -> DaemonResponse {
    let health = node.health().await;
    let peers = node.peers().await;
    let info = node.local_info();

    DaemonResponse::success(
        id,
        serde_json::json!({
            "checks": {
                "node_online": health.healthy,
                "tailscale_state": health.state,
                "peer_count": peers.len(),
                "key_expiry": health.key_expiry,
                "warnings": health.warnings,
                "node_id": info.id,
                "node_name": info.name,
            }
        }),
    )
}
