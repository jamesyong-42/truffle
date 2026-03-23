//! Request handler: dispatches JSON-RPC methods to `TruffleRuntime`.
//!
//! Each method is dispatched to a handler function that interacts with the
//! runtime and returns a JSON-RPC response.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Notify;
use tracing::info;
use truffle_core::runtime::TruffleRuntime;
use truffle_core::services::file_transfer::adapter::{
    FileTransferAdapter, FileTransferOffer, generate_transfer_id, generate_token,
};
use truffle_core::services::file_transfer::manager::FileTransferManager;
use truffle_core::services::file_transfer::types::{FileInfo, FileTransferEvent};

use super::protocol::{error_code, method, notification, DaemonNotification, DaemonRequest, DaemonResponse};

/// Context bundling all daemon-owned resources needed by request handlers.
///
/// Avoids a growing parameter list on `dispatch()` as new subsystems are added.
pub struct DaemonContext {
    pub runtime: Arc<TruffleRuntime>,
    pub file_transfer_adapter: Arc<FileTransferAdapter>,
    pub file_transfer_manager: Arc<FileTransferManager>,
    pub shutdown_signal: Arc<Notify>,
    pub started_at: Instant,
}

/// Dispatch a JSON-RPC request to the appropriate handler.
///
/// Returns a `DaemonResponse` with either a result or an error.
///
/// The `notification_tx` channel is used by long-running handlers (like
/// file transfer) to send streaming progress notifications back to the
/// client before the final response.
pub async fn dispatch(
    request: &DaemonRequest,
    ctx: &DaemonContext,
    notification_tx: tokio::sync::mpsc::UnboundedSender<DaemonNotification>,
) -> DaemonResponse {
    let runtime = &ctx.runtime;
    let started_at = ctx.started_at;
    let shutdown_signal = &ctx.shutdown_signal;

    match request.method.as_str() {
        method::STATUS => handle_status(request.id, runtime, started_at).await,
        method::PEERS => handle_peers(request.id, runtime).await,
        method::PING => handle_ping(request.id, &request.params, runtime).await,
        method::SEND_MESSAGE => handle_send_message(request.id, &request.params, runtime).await,
        method::SHUTDOWN => handle_shutdown(request.id, shutdown_signal),

        // Phase 9: Connectivity
        method::TCP_CONNECT => handle_tcp_connect(request.id, &request.params, runtime).await,
        method::WS_CONNECT => handle_ws_connect(request.id, &request.params, runtime).await,
        method::PROXY_START => handle_proxy_start(request.id, &request.params, runtime).await,
        method::PROXY_STOP => handle_proxy_stop(request.id, &request.params).await,
        method::EXPOSE_START => handle_expose_start(request.id, &request.params, runtime).await,
        method::EXPOSE_STOP => handle_expose_stop(request.id, &request.params).await,

        // Phase 10: Communication
        "chat_start" => handle_chat_start(request.id, &request.params, runtime).await,

        // Phase 11: File transfer (with progress notifications)
        method::PUSH_FILE => {
            handle_push_file(request.id, &request.params, ctx, notification_tx).await
        }
        method::GET_FILE => {
            handle_get_file(request.id, &request.params, ctx).await
        }

        _ => DaemonResponse::error(
            request.id,
            error_code::METHOD_NOT_FOUND,
            format!("Method '{}' not found", request.method),
        ),
    }
}

/// Handle `status` -- return node status information.
async fn handle_status(
    id: u64,
    runtime: &Arc<TruffleRuntime>,
    started_at: Instant,
) -> DaemonResponse {
    let device = runtime.local_device().await;
    let uptime_secs = started_at.elapsed().as_secs();
    let auth_status = runtime.mesh_node().auth_status().await;
    let auth_url = runtime.mesh_node().auth_url().await;
    let peer_count = runtime.devices().await.len();

    let status_str = if device.tailscale_ip.is_some() {
        "online"
    } else if auth_url.is_some() {
        "authRequired"
    } else {
        match auth_status {
            truffle_core::types::AuthStatus::Authenticated => "online",
            truffle_core::types::AuthStatus::Required => "authRequired",
            _ => "connecting",
        }
    };

    DaemonResponse::success(
        id,
        serde_json::json!({
            "device_id": device.id,
            "name": device.name,
            "device_type": device.device_type,
            "hostname": device.tailscale_hostname,
            "status": status_str,
            "tailscale_ip": device.tailscale_ip,
            "tailscale_dns_name": device.tailscale_dns_name,
            "auth_url": auth_url,
            "auth_status": format!("{:?}", auth_status),
            "peer_count": peer_count,
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
                "hostname": d.tailscale_hostname,
                "status": format!("{:?}", d.status),
                "tailscale_ip": d.tailscale_ip,
                "tailscale_dns_name": d.tailscale_dns_name,
                "latency_ms": d.latency_ms,
                "os": d.os,
                "last_seen": d.last_seen,
                "capabilities": d.capabilities,
                "cur_addr": d.cur_addr,
                "relay": d.relay,
            })
        })
        .collect();

    DaemonResponse::success(id, serde_json::json!({ "peers": peers }))
}

/// Handle `ping` -- connectivity check to a node.
///
/// Looks up the node by name, device_id, hostname, or IP. Returns
/// reachability status along with latency and connection type when available.
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

    // Also accept device_id for direct lookups from the CLI (after name resolution)
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let devices = runtime.devices().await;

    // Find the peer: match by device_id first, then name, hostname, or IP
    let found_device = devices.iter().find(|d| {
        (!device_id.is_empty() && d.id == device_id)
            || d.name == node
            || d.id == node
            || d.tailscale_hostname == node
            || d.tailscale_ip.as_deref() == Some(node)
    });

    match found_device {
        Some(device) => {
            let is_online = device.status == truffle_core::types::DeviceStatus::Online;

            let connection = if is_online {
                // In the future, this will come from the sidecar's ping result
                "direct"
            } else {
                "unknown"
            };

            DaemonResponse::success(
                id,
                serde_json::json!({
                    "node": node,
                    "display_name": device.name,
                    "device_id": device.id,
                    "reachable": is_online,
                    "latency_ms": device.latency_ms,
                    "connection": connection,
                    "tailscale_ip": device.tailscale_ip,
                    "reason": if is_online { serde_json::Value::Null } else {
                        serde_json::Value::String("Node is offline".to_string())
                    },
                }),
            )
        }
        None => DaemonResponse::success(
            id,
            serde_json::json!({
                "node": node,
                "reachable": false,
                "reason": "Node not found in peer list",
            }),
        ),
    }
}

/// Handle `send_message` -- send a mesh message to a specific device or broadcast.
async fn handle_send_message(
    id: u64,
    params: &serde_json::Value,
    runtime: &Arc<TruffleRuntime>,
) -> DaemonResponse {
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

    // Check if this is a broadcast
    let is_broadcast = params
        .get("broadcast")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if is_broadcast {
        runtime.broadcast_envelope(&envelope).await;

        // Count online peers for the response
        let local_id = runtime.device_id().await;
        let devices = runtime.devices().await;
        let peer_count = devices.iter().filter(|d| d.id != local_id).count();

        DaemonResponse::success(
            id,
            serde_json::json!({
                "broadcast": true,
                "broadcast_count": peer_count,
            }),
        )
    } else {
        let device_id = match params.get("device_id").and_then(|v| v.as_str()) {
            Some(d) => d,
            None => {
                return DaemonResponse::error(
                    id,
                    error_code::INVALID_PARAMS,
                    "Missing required parameter: 'device_id' (or use 'broadcast': true)",
                );
            }
        };

        let sent = runtime.send_envelope(device_id, &envelope).await;

        DaemonResponse::success(
            id,
            serde_json::json!({
                "sent": sent,
                "device_id": device_id,
            }),
        )
    }
}

/// Handle `shutdown` -- gracefully stop the daemon.
fn handle_shutdown(id: u64, shutdown_signal: &Arc<Notify>) -> DaemonResponse {
    // Signal the main loop to shut down
    shutdown_signal.notify_one();
    DaemonResponse::success(id, serde_json::json!({ "shutting_down": true }))
}

// ═══════════════════════════════════════════════════════════════════════════
// Phase 11: File transfer handlers
// ═══════════════════════════════════════════════════════════════════════════

/// Handle `push_file` -- push a file to a remote node via the file transfer subsystem.
///
/// Orchestrates the full transfer flow:
/// 1. Validate params and local file
/// 2. Resolve target node to device_id
/// 3. Prepare the file (stat + SHA-256 hash)
/// 4. Send OFFER with cli_mode=true to the target
/// 5. Wait for ACCEPT from the target
/// 6. The adapter's ACCEPT handler registers the send and starts streaming
/// 7. Wait for Complete or Error event
/// 8. Return the result
async fn handle_push_file(
    id: u64,
    params: &serde_json::Value,
    ctx: &DaemonContext,
    notification_tx: tokio::sync::mpsc::UnboundedSender<DaemonNotification>,
) -> DaemonResponse {
    let runtime = &ctx.runtime;
    let adapter = &ctx.file_transfer_adapter;
    let manager = &ctx.file_transfer_manager;

    // --- Parse params ---

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

    let local_path = match params.get("local_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'local_path'",
            );
        }
    };

    let remote_path = match params.get("remote_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'remote_path'",
            );
        }
    };

    // --- Validate local file ---

    let path = std::path::Path::new(local_path);
    if !path.exists() {
        return DaemonResponse::error(
            id,
            error_code::INVALID_PARAMS,
            format!("File not found: {local_path}"),
        );
    }
    if !path.is_file() {
        return DaemonResponse::error(
            id,
            error_code::INVALID_PARAMS,
            format!("Not a file: {local_path}"),
        );
    }

    // --- Resolve node to device_id ---

    let device_id = match resolve_node_device_id(runtime, node).await {
        Some(did) => did,
        None => {
            return DaemonResponse::error(
                id,
                error_code::NODE_NOT_FOUND,
                format!("Node not found: {node}"),
            );
        }
    };

    // --- Generate transfer credentials ---

    let transfer_id = generate_transfer_id();
    let token = generate_token();

    info!(
        transfer_id = %transfer_id,
        local_path = %local_path,
        remote_path = %remote_path,
        target = %device_id,
        "Starting file transfer"
    );

    // --- Subscribe to manager events BEFORE preparing ---
    // This ensures we don't miss the Prepared event.

    let mut event_rx = manager.subscribe_events();

    // --- Prepare the file (stat + SHA-256 hash) ---

    manager.prepare_file(&transfer_id, local_path).await;

    // --- Wait for Prepared event (with timeout) ---

    let overall_timeout = tokio::time::Duration::from_secs(60);
    let started = Instant::now();

    let (file_name, file_size, file_sha256) = loop {
        let remaining = overall_timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return DaemonResponse::error(
                id,
                error_code::TIMEOUT,
                "Timed out waiting for file preparation",
            );
        }

        match tokio::time::timeout(remaining, event_rx.recv()).await {
            Ok(Ok(FileTransferEvent::Prepared {
                transfer_id: ref tid,
                ref name,
                size,
                ref sha256,
            })) if tid == &transfer_id => {
                break (name.clone(), size, sha256.clone());
            }
            Ok(Ok(FileTransferEvent::Error {
                transfer_id: ref tid,
                ref message,
                ..
            })) if tid == &transfer_id => {
                return DaemonResponse::error(
                    id,
                    error_code::INTERNAL_ERROR,
                    format!("File preparation failed: {message}"),
                );
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                // Missed some events, continue listening
                continue;
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return DaemonResponse::error(
                    id,
                    error_code::INTERNAL_ERROR,
                    "Event channel closed during file preparation",
                );
            }
            Ok(Ok(_)) => {
                // Event for a different transfer or different type, skip
                continue;
            }
            Err(_) => {
                return DaemonResponse::error(
                    id,
                    error_code::TIMEOUT,
                    "Timed out waiting for file preparation",
                );
            }
        }
    };

    let file_info = FileInfo {
        name: file_name,
        size: file_size,
        sha256: file_sha256.clone(),
    };

    // --- Register the pending send in the adapter ---
    // This is needed so the adapter's ACCEPT handler can find the file path
    // and transfer info when the remote node accepts.

    adapter
        .register_pending_send(&transfer_id, local_path, &file_info, &device_id)
        .await;

    // --- Construct and send OFFER with cli_mode=true ---

    let offer = FileTransferOffer {
        transfer_id: transfer_id.clone(),
        sender_device_id: adapter.local_device_id().to_string(),
        sender_addr: adapter.local_addr().to_string(),
        file: file_info,
        token: token.clone(),
        cli_mode: true,
        save_path: Some(remote_path.to_string()),
    };

    adapter.send_offer(&device_id, &offer);

    info!(
        transfer_id = %transfer_id,
        target = %device_id,
        "OFFER sent, waiting for ACCEPT"
    );

    // --- Wait for Complete or Error event (with timeout) ---
    // The adapter's ACCEPT handler will register_send + send_file when ACCEPT arrives.
    // We watch for the final Complete or Error event from the manager.
    // Progress events are forwarded as JSON-RPC notifications to the CLI.

    loop {
        let remaining = overall_timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return DaemonResponse::error(
                id,
                error_code::TIMEOUT,
                "File transfer timed out",
            );
        }

        match tokio::time::timeout(remaining, event_rx.recv()).await {
            Ok(Ok(FileTransferEvent::Progress {
                transfer_id: ref tid,
                bytes_transferred,
                total_bytes,
                percent,
                bytes_per_second,
                eta,
                ..
            })) if tid == &transfer_id => {
                // Forward progress as a JSON-RPC notification
                let _ = notification_tx.send(DaemonNotification::new(
                    notification::CP_PROGRESS,
                    serde_json::json!({
                        "transfer_id": transfer_id,
                        "bytes_transferred": bytes_transferred,
                        "total_bytes": total_bytes,
                        "percent": percent,
                        "bytes_per_second": bytes_per_second,
                        "eta": eta,
                    }),
                ));
                continue;
            }
            Ok(Ok(FileTransferEvent::Complete {
                transfer_id: ref tid,
                ref sha256,
                size,
                duration_ms,
                ..
            })) if tid == &transfer_id => {
                info!(
                    transfer_id = %transfer_id,
                    size = size,
                    duration_ms = duration_ms,
                    "File transfer complete"
                );
                return DaemonResponse::success(
                    id,
                    serde_json::json!({
                        "transfer_id": transfer_id,
                        "bytes_transferred": size,
                        "sha256": sha256,
                        "duration_ms": duration_ms,
                    }),
                );
            }
            Ok(Ok(FileTransferEvent::Error {
                transfer_id: ref tid,
                ref code,
                ref message,
                ..
            })) if tid == &transfer_id => {
                return DaemonResponse::error(
                    id,
                    error_code::INTERNAL_ERROR,
                    format!("Transfer failed ({code}): {message}"),
                );
            }
            Ok(Ok(FileTransferEvent::Cancelled {
                transfer_id: ref tid,
            })) if tid == &transfer_id => {
                return DaemonResponse::error(
                    id,
                    error_code::INTERNAL_ERROR,
                    "Transfer was cancelled",
                );
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                continue;
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return DaemonResponse::error(
                    id,
                    error_code::INTERNAL_ERROR,
                    "Event channel closed during transfer",
                );
            }
            Ok(Ok(_)) => {
                // Event for different transfer or unrelated type, skip
                continue;
            }
            Err(_) => {
                return DaemonResponse::error(
                    id,
                    error_code::TIMEOUT,
                    "File transfer timed out",
                );
            }
        }
    }
}

/// Handle `get_file` -- download a file from a remote node via the file transfer subsystem.
///
/// Orchestrates the PULL_REQUEST reverse-roles download flow:
/// 1. Parse params: node, remote_path, local_path
/// 2. Resolve node to device_id
/// 3. Send PULL_REQUEST via adapter
/// 4. Wait for incoming OFFER from the remote node (it becomes the sender)
/// 5. The auto-accept from Phase 5 handles accepting with the local save_path
/// 6. Wait for transfer Complete/Error event
/// 7. Return result
async fn handle_get_file(
    id: u64,
    params: &serde_json::Value,
    ctx: &DaemonContext,
) -> DaemonResponse {
    let runtime = &ctx.runtime;
    let adapter = &ctx.file_transfer_adapter;
    let manager = &ctx.file_transfer_manager;

    // --- Parse params ---

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

    let remote_path = match params.get("remote_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'remote_path'",
            );
        }
    };

    let local_path = match params.get("local_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'local_path'",
            );
        }
    };

    // --- Validate local_path parent directory exists ---

    let local = std::path::Path::new(local_path);
    if let Some(parent) = local.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                format!("Parent directory does not exist: {}", parent.display()),
            );
        }
    }

    // --- Resolve node to device_id ---

    let device_id = match resolve_node_device_id(runtime, node).await {
        Some(did) => did,
        None => {
            return DaemonResponse::error(
                id,
                error_code::NODE_NOT_FOUND,
                format!("Node not found: {node}"),
            );
        }
    };

    // --- Generate request ID ---

    let request_id = generate_transfer_id();

    info!(
        request_id = %request_id,
        remote_path = %remote_path,
        local_path = %local_path,
        target = %device_id,
        "Starting file download (PULL_REQUEST)"
    );

    // --- Subscribe to manager events BEFORE sending PULL_REQUEST ---
    // This ensures we don't miss the Complete event from the auto-accepted transfer.

    let mut event_rx = manager.subscribe_events();

    // --- Send PULL_REQUEST to the remote node ---

    let pull_request = truffle_core::services::file_transfer::adapter::FileTransferPullRequest {
        request_id: request_id.clone(),
        remote_path: remote_path.to_string(),
        requester_device_id: adapter.local_device_id().to_string(),
        save_path: local_path.to_string(),
    };

    adapter.send_pull_request(&device_id, &pull_request);

    info!(
        request_id = %request_id,
        target = %device_id,
        "PULL_REQUEST sent, waiting for OFFER + transfer"
    );

    // --- Wait for Complete or Error event (with timeout) ---
    // The remote node will:
    //   1. Receive PULL_REQUEST
    //   2. Validate the file, prepare it (stat + hash)
    //   3. Send OFFER back with cli_mode=true and save_path=local_path
    // Our adapter will:
    //   1. Receive the OFFER
    //   2. Auto-accept it (Phase 5 cli_mode handling)
    //   3. Receive the file data via HTTP
    //   4. Emit Complete or Error event

    let overall_timeout = tokio::time::Duration::from_secs(120);
    let started = Instant::now();

    loop {
        let remaining = overall_timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return DaemonResponse::error(
                id,
                error_code::TIMEOUT,
                "File download timed out",
            );
        }

        match tokio::time::timeout(remaining, event_rx.recv()).await {
            Ok(Ok(FileTransferEvent::Complete {
                transfer_id: ref tid,
                ref sha256,
                size,
                duration_ms,
                ref direction,
                ..
            })) if direction == "receive" => {
                // Accept any receive-direction Complete event since we don't know
                // the transfer_id the remote side will generate for the OFFER
                info!(
                    transfer_id = %tid,
                    size = size,
                    duration_ms = duration_ms,
                    "File download complete"
                );
                return DaemonResponse::success(
                    id,
                    serde_json::json!({
                        "transfer_id": tid,
                        "bytes_transferred": size,
                        "sha256": sha256,
                        "duration_ms": duration_ms,
                    }),
                );
            }
            Ok(Ok(FileTransferEvent::Error {
                ref code,
                ref message,
                ref transfer_id,
                ..
            })) => {
                // Accept any error since we can't filter by our request_id
                // (the remote generates a new transfer_id)
                return DaemonResponse::error(
                    id,
                    error_code::INTERNAL_ERROR,
                    format!("Download failed ({code}): {message} [transfer: {transfer_id}]"),
                );
            }
            Ok(Ok(FileTransferEvent::Cancelled {
                transfer_id: ref tid,
            })) => {
                return DaemonResponse::error(
                    id,
                    error_code::INTERNAL_ERROR,
                    format!("Download was cancelled [transfer: {tid}]"),
                );
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                continue;
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return DaemonResponse::error(
                    id,
                    error_code::INTERNAL_ERROR,
                    "Event channel closed during download",
                );
            }
            Ok(Ok(_)) => {
                // Progress or other event, skip
                continue;
            }
            Err(_) => {
                return DaemonResponse::error(
                    id,
                    error_code::TIMEOUT,
                    "File download timed out",
                );
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Phase 9: Connectivity handlers
// ═══════════════════════════════════════════════════════════════════════════

/// Handle `tcp_connect` -- establish a TCP connection to a target.
///
/// The daemon resolves the target node to an IP address and establishes
/// the TCP connection. In streaming mode, the Unix socket is upgraded
/// to a raw byte pipe. In check mode, just tests connectivity.
async fn handle_tcp_connect(
    id: u64,
    params: &serde_json::Value,
    runtime: &Arc<TruffleRuntime>,
) -> DaemonResponse {
    let target = match params.get("target").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'target'",
            );
        }
    };

    let check = params
        .get("check")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let stream_mode = params
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Resolve the node to an IP address via peer list
    let node = params
        .get("node")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let port = params
        .get("port")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let resolved_addr = resolve_node_addr(runtime, node).await;

    if check {
        // Resolve the node to a DNS name for sidecar dial
        let dial_target = resolve_node_dns(runtime, node).await;

        if let Some(dns_name) = dial_target {
            // Use the sidecar's dial mechanism to check connectivity via tsnet.
            // dial_peer does a raw TCP dial through the Go sidecar, which can
            // reach tsnet IPs that the host TCP stack cannot.
            let port_u16 = port as u16;
            let start = Instant::now();
            match runtime.dial_peer(&dns_name, port_u16).await {
                Ok(()) => {
                    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                    DaemonResponse::success(
                        id,
                        serde_json::json!({
                            "connected": true,
                            "target": target,
                            "resolved": format!("{dns_name}:{port}"),
                            "latency_ms": elapsed_ms,
                        }),
                    )
                }
                Err(e) => DaemonResponse::success(
                    id,
                    serde_json::json!({
                        "connected": false,
                        "target": target,
                        "reason": format!("Connection failed via sidecar: {e}"),
                    }),
                ),
            }
        } else {
            // No DNS name found -- fall back to direct connect for non-mesh targets
            let connect_addr = if let Some(ref ip) = resolved_addr {
                format!("{ip}:{port}")
            } else {
                target.to_string()
            };

            match tokio::time::timeout(
                tokio::time::Duration::from_secs(5),
                tokio::net::TcpStream::connect(&connect_addr),
            )
            .await
            {
                Ok(Ok(_stream)) => DaemonResponse::success(
                    id,
                    serde_json::json!({
                        "connected": true,
                        "target": target,
                        "resolved": connect_addr,
                    }),
                ),
                Ok(Err(e)) => DaemonResponse::success(
                    id,
                    serde_json::json!({
                        "connected": false,
                        "target": target,
                        "reason": format!("Connection refused: {e}"),
                    }),
                ),
                Err(_) => DaemonResponse::success(
                    id,
                    serde_json::json!({
                        "connected": false,
                        "target": target,
                        "reason": "Connection timed out (5s)",
                    }),
                ),
            }
        }
    } else if stream_mode {
        // Streaming mode: return upgrade response
        let connect_addr = if let Some(ref ip) = resolved_addr {
            format!("{ip}:{port}")
        } else {
            target.to_string()
        };

        DaemonResponse::success(
            id,
            serde_json::json!({
                "stream": true,
                "handle": format!("tcp-{}", id),
                "target": target,
                "resolved": connect_addr,
            }),
        )
    } else {
        // Standard mode: report stream capability
        DaemonResponse::success(
            id,
            serde_json::json!({
                "stream": true,
                "target": target,
            }),
        )
    }
}

/// Handle `ws_connect` -- establish a WebSocket connection to a target.
async fn handle_ws_connect(
    id: u64,
    params: &serde_json::Value,
    runtime: &Arc<TruffleRuntime>,
) -> DaemonResponse {
    let url = match params.get("url").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'url'",
            );
        }
    };

    // Resolve the node to verify it exists in the peer list
    let node = params
        .get("node")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let _resolved = resolve_node_addr(runtime, node).await;

    // Return stream upgrade response
    DaemonResponse::success(
        id,
        serde_json::json!({
            "stream": true,
            "handle": format!("ws-{}", id),
            "url": url,
        }),
    )
}

/// Handle `proxy_start` -- start port forwarding.
///
/// The daemon starts a local TCP listener and for each incoming connection,
/// dials the remote target and copies data bidirectionally.
async fn handle_proxy_start(
    id: u64,
    params: &serde_json::Value,
    runtime: &Arc<TruffleRuntime>,
) -> DaemonResponse {
    let local_port = match params.get("local_port").and_then(|v| v.as_u64()) {
        Some(p) => p as u16,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'local_port'",
            );
        }
    };

    let remote_target = match params.get("remote_target").and_then(|v| v.as_str()) {
        Some(t) => t.to_string(),
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'remote_target'",
            );
        }
    };

    let bind_addr = params
        .get("bind_addr")
        .and_then(|v| v.as_str())
        .unwrap_or("127.0.0.1");

    let node = params
        .get("node")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Resolve node to IP
    let resolved_ip = resolve_node_addr(runtime, node).await;

    // Try to bind the local port to check availability
    let bind_address = format!("{bind_addr}:{local_port}");
    match tokio::net::TcpListener::bind(&bind_address).await {
        Ok(listener) => {
            let proxy_id = format!("proxy-{local_port}-{}", id);
            let remote_addr = if let Some(ref ip) = resolved_ip {
                let remote_port = params
                    .get("remote_port")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                format!("{ip}:{remote_port}")
            } else {
                remote_target.clone()
            };

            // Spawn the proxy forwarding task
            let proxy_id_clone = proxy_id.clone();
            tokio::spawn(async move {
                run_proxy_loop(listener, &remote_addr, &proxy_id_clone).await;
            });

            DaemonResponse::success(
                id,
                serde_json::json!({
                    "proxy_id": proxy_id,
                    "local_addr": bind_address,
                    "remote_target": remote_target,
                    "remote_ip": resolved_ip.unwrap_or_default(),
                }),
            )
        }
        Err(e) => DaemonResponse::error(
            id,
            error_code::INTERNAL_ERROR,
            format!("Port {local_port} is already in use: {e}"),
        ),
    }
}

/// Run the proxy forwarding loop.
///
/// Accepts connections on the local listener and for each incoming
/// connection, dials the remote target and copies data bidirectionally.
async fn run_proxy_loop(
    listener: tokio::net::TcpListener,
    remote_addr: &str,
    _proxy_id: &str,
) {
    let remote = remote_addr.to_string();
    loop {
        match listener.accept().await {
            Ok((local_stream, _addr)) => {
                let remote = remote.clone();
                tokio::spawn(async move {
                    match tokio::net::TcpStream::connect(&remote).await {
                        Ok(remote_stream) => {
                            let (mut local_read, mut local_write) = local_stream.into_split();
                            let (mut remote_read, mut remote_write) = remote_stream.into_split();

                            let l2r = tokio::io::copy(&mut local_read, &mut remote_write);
                            let r2l = tokio::io::copy(&mut remote_read, &mut local_write);

                            tokio::select! {
                                _ = l2r => {}
                                _ = r2l => {}
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to connect to remote {remote}: {e}");
                        }
                    }
                });
            }
            Err(e) => {
                tracing::error!("Failed to accept proxy connection: {e}");
                break;
            }
        }
    }
}

/// Handle `proxy_stop` -- stop a running proxy.
async fn handle_proxy_stop(id: u64, params: &serde_json::Value) -> DaemonResponse {
    let proxy_id = params
        .get("proxy_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // The proxy stops when the daemon shuts down or the task is cancelled.
    // Full lifecycle management (tracked tasks + cancellation tokens) is
    // planned for a follow-up iteration.
    DaemonResponse::success(
        id,
        serde_json::json!({
            "stopped": true,
            "proxy_id": proxy_id,
        }),
    )
}

/// Handle `expose_start` -- expose a local port on the tailnet.
///
/// Creates a listener on the tailnet (via tsnet in the Go sidecar) and
/// forwards incoming connections to localhost:port.
async fn handle_expose_start(
    id: u64,
    params: &serde_json::Value,
    runtime: &Arc<TruffleRuntime>,
) -> DaemonResponse {
    let port = match params.get("port").and_then(|v| v.as_u64()) {
        Some(p) => p as u16,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'port'",
            );
        }
    };

    let _https = params
        .get("https")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let custom_name = params.get("name").and_then(|v| v.as_str());

    // Get the local device info for the mesh address
    let device = runtime.local_device().await;
    let hostname = custom_name
        .unwrap_or_else(|| {
            if !device.tailscale_hostname.is_empty() {
                &device.tailscale_hostname
            } else {
                &device.name
            }
        })
        .to_string();

    let expose_id = format!("expose-{port}-{}", id);
    let mesh_addr = format!("{hostname}:{port}");

    // Start a tailnet listener task that forwards to localhost:port.
    // In a full implementation, this would use tsnet:listen via the Go sidecar.
    let local_target = format!("127.0.0.1:{port}");
    let expose_id_clone = expose_id.clone();

    if let Some(ip) = device.tailscale_ip.as_deref() {
        let listen_addr = format!("{ip}:{port}");
        match tokio::net::TcpListener::bind(&listen_addr).await {
            Ok(listener) => {
                tokio::spawn(async move {
                    run_proxy_loop(listener, &local_target, &expose_id_clone).await;
                });
            }
            Err(e) => {
                tracing::warn!(
                    "Could not bind tailnet listener at {listen_addr}: {e}. \
                     Expose will rely on sidecar."
                );
            }
        }
    }

    DaemonResponse::success(
        id,
        serde_json::json!({
            "expose_id": expose_id,
            "mesh_addr": mesh_addr,
            "hostname": hostname,
            "port": port,
        }),
    )
}

/// Handle `expose_stop` -- stop exposing a local port.
async fn handle_expose_stop(id: u64, params: &serde_json::Value) -> DaemonResponse {
    let expose_id = params
        .get("expose_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    DaemonResponse::success(
        id,
        serde_json::json!({
            "stopped": true,
            "expose_id": expose_id,
        }),
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// Phase 10: Communication handlers
// ═══════════════════════════════════════════════════════════════════════════

/// Handle `chat_start` -- start a chat streaming session.
///
/// Returns a stream upgrade response. After the upgrade, the Unix socket
/// carries newline-delimited JSON events for the chat session.
async fn handle_chat_start(
    id: u64,
    _params: &serde_json::Value,
    runtime: &Arc<TruffleRuntime>,
) -> DaemonResponse {
    let device = runtime.local_device().await;

    DaemonResponse::success(
        id,
        serde_json::json!({
            "stream": true,
            "handle": format!("chat-{}", id),
            "local_name": device.name,
        }),
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Resolve a node name to a device ID.
///
/// Looks up the node in the peer list by name, device ID, or hostname.
/// Returns `None` if the node is not found.
async fn resolve_node_device_id(runtime: &Arc<TruffleRuntime>, node: &str) -> Option<String> {
    if node.is_empty() {
        return None;
    }

    let devices = runtime.devices().await;
    devices
        .iter()
        .find(|d| d.name == node || d.id == node || d.tailscale_hostname == node)
        .map(|d| d.id.clone())
}

/// Resolve a node name to a Tailscale IP address.
///
/// Looks up the node in the peer list by name, device ID, or hostname.
/// Returns `None` if the node is not found.
async fn resolve_node_addr(runtime: &Arc<TruffleRuntime>, node: &str) -> Option<String> {
    if node.is_empty() {
        return None;
    }

    let devices = runtime.devices().await;
    devices
        .iter()
        .find(|d| d.name == node || d.id == node || d.tailscale_hostname == node)
        .and_then(|d| d.tailscale_ip.clone())
}

/// Resolve a node name to a Tailscale DNS name (for sidecar dial).
///
/// Looks up the node in the peer list by name, device ID, or hostname.
/// Returns `None` if the node is not found or has no DNS name.
async fn resolve_node_dns(runtime: &Arc<TruffleRuntime>, node: &str) -> Option<String> {
    if node.is_empty() {
        return None;
    }

    let devices = runtime.devices().await;
    devices
        .iter()
        .find(|d| d.name == node || d.id == node || d.tailscale_hostname == node)
        .and_then(|d| d.tailscale_dns_name.clone())
}
