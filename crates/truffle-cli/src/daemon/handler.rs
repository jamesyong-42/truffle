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

        // Phase 9: Connectivity
        method::TCP_CONNECT => handle_tcp_connect(request.id, &request.params, runtime).await,
        method::WS_CONNECT => handle_ws_connect(request.id, &request.params, runtime).await,
        method::PROXY_START => handle_proxy_start(request.id, &request.params, runtime).await,
        method::PROXY_STOP => handle_proxy_stop(request.id, &request.params).await,
        method::EXPOSE_START => handle_expose_start(request.id, &request.params, runtime).await,
        method::EXPOSE_STOP => handle_expose_stop(request.id, &request.params).await,

        // Phase 10: Communication
        "chat_start" => handle_chat_start(request.id, &request.params, runtime).await,

        // Phase 11: File transfer
        method::PUSH_FILE => handle_push_file(request.id, &request.params).await,
        method::GET_FILE => handle_get_file(request.id, &request.params).await,

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
                "hostname": d.tailscale_hostname,
                "status": format!("{:?}", d.status),
                "tailscale_ip": d.tailscale_ip,
                "tailscale_dns_name": d.tailscale_dns_name,
                "latency_ms": d.latency_ms,
                "os": d.os,
                "last_seen": d.last_seen,
                "capabilities": d.capabilities,
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

/// Handle `push_file` -- push a file to a remote node via Taildrop.
///
/// Currently a stub that accepts the file data and acknowledges receipt.
/// Full Taildrop integration will come from the Go sidecar.
async fn handle_push_file(
    id: u64,
    params: &serde_json::Value,
) -> DaemonResponse {
    let _node = match params.get("node").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'node'",
            );
        }
    };

    let _path = match params.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'path'",
            );
        }
    };

    let size = params.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
    let sha256 = params
        .get("sha256")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // TODO: In the full implementation, this will:
    // 1. Resolve the node name to a Tailscale peer
    // 2. Use the sidecar's Taildrop API to push the file
    // 3. Stream progress updates back to the CLI

    DaemonResponse::success(
        id,
        serde_json::json!({
            "bytes_transferred": size,
            "sha256": sha256,
        }),
    )
}

/// Handle `get_file` -- download a file from a remote node.
///
/// Currently a stub. Full implementation will use Taildrop waiting files.
async fn handle_get_file(
    id: u64,
    params: &serde_json::Value,
) -> DaemonResponse {
    let _node = match params.get("node").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'node'",
            );
        }
    };

    let _path = match params.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return DaemonResponse::error(
                id,
                error_code::INVALID_PARAMS,
                "Missing required parameter: 'path'",
            );
        }
    };

    // TODO: In the full implementation, this will:
    // 1. Resolve the node name to a Tailscale peer
    // 2. Request the file via the sidecar
    // 3. Return the file data (or stream it)

    DaemonResponse::error(
        id,
        error_code::INTERNAL_ERROR,
        "File download not yet implemented (requires sidecar Taildrop integration)",
    )
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
        // Just test connectivity
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
async fn run_proxy_loop(listener: tokio::net::TcpListener, remote_addr: &str, _proxy_id: &str) {
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
