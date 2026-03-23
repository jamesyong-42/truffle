//! Daemon server: IPC listener with JSON-RPC dispatch.
//!
//! The server owns a `TruffleRuntime`, listens on a platform-appropriate IPC
//! transport (Unix socket on macOS/Linux, named pipe on Windows), and
//! dispatches incoming JSON-RPC requests to the handler module.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, Notify};
use tracing::{error, info, warn};
use truffle_core::runtime::TruffleRuntime;
use truffle_core::services::file_transfer::adapter::{
    FileTransferAdapter, FileTransferAdapterConfig,
};
use truffle_core::services::file_transfer::manager::FileTransferManager;
use truffle_core::services::file_transfer::receiver::file_transfer_router;
use truffle_core::services::file_transfer::sender::DialFn;
use truffle_core::services::file_transfer::types::{FileTransferConfig, FileTransferEvent};

use super::handler::{self, DaemonContext};
use super::ipc;
use super::pid;
use super::protocol::DaemonRequest;
use crate::config::TruffleConfig;
use crate::sidecar;

/// The daemon server process.
pub struct DaemonServer {
    /// The runtime instance (owned by the daemon for the lifetime of the process).
    runtime: Arc<TruffleRuntime>,
    /// File transfer adapter for mesh signaling.
    file_transfer_adapter: Arc<FileTransferAdapter>,
    /// File transfer manager for transfer state and data plane.
    file_transfer_manager: Arc<FileTransferManager>,
    /// Path to the Unix socket file.
    socket_path: PathBuf,
    /// Path to the PID file.
    pid_path: PathBuf,
    /// Shutdown signal (notified when `shutdown` RPC is received or signal caught).
    shutdown: Arc<Notify>,
    /// When the server started (for uptime tracking).
    started_at: Instant,
}

impl DaemonServer {
    /// Start the daemon server.
    ///
    /// 1. Build and start a `TruffleRuntime` from config.
    /// 2. Create the file transfer subsystem (manager + adapter + axum listener + mesh wiring).
    /// 3. Write the PID file.
    /// 4. Bind the Unix socket.
    ///
    /// Returns the server instance; call `run()` to enter the accept loop.
    pub async fn start(config: &TruffleConfig) -> Result<Self, String> {
        let socket_path = TruffleConfig::socket_path();
        let pid_path = TruffleConfig::pid_path();

        // Check for stale PID file and clean up
        if pid::check_stale_pid(&pid_path).map_err(|e| format!("PID check failed: {e}"))? {
            info!("Removing stale PID file");
            pid::remove_pid(&pid_path).map_err(|e| format!("Failed to remove stale PID: {e}"))?;
        }

        // Check if daemon is already running
        if let Ok(Some(existing_pid)) = pid::read_pid(&pid_path) {
            if pid::is_process_running(existing_pid) {
                return Err(format!(
                    "Daemon is already running (PID {existing_pid}). Use 'truffle down' first."
                ));
            }
        }

        // Auto-discover sidecar binary
        let sidecar_path = sidecar::find_sidecar(
            if config.node.sidecar_path.is_empty() {
                None
            } else {
                Some(config.node.sidecar_path.as_str())
            },
        )?;
        info!(sidecar = %sidecar_path.display(), "Using sidecar binary");

        // Auto-create state directory
        let state_dir = if config.node.state_dir.is_empty() {
            let dir = dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("truffle")
                .join("state");
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create state directory {}: {e}", dir.display()))?;
            dir.to_string_lossy().to_string()
        } else {
            let dir = PathBuf::from(&config.node.state_dir);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create state directory {}: {e}", dir.display()))?;
            config.node.state_dir.clone()
        };

        // Build the runtime
        // Use "truffle" as the shared mesh prefix so all nodes can discover
        // each other. The machine hostname is used as the display name only.
        let (runtime, _mesh_rx) = TruffleRuntime::builder()
            .hostname("truffle")
            .device_name(&config.node.name)
            .device_type("cli")
            .sidecar_path(sidecar_path)
            .state_dir(&state_dir)
            .build()
            .map_err(|e| format!("Failed to build runtime: {e}"))?;

        let runtime = Arc::new(runtime);

        // Start the runtime
        let _event_rx = runtime
            .start()
            .await
            .map_err(|e| format!("Failed to start runtime: {e}"))?;

        // ── File transfer subsystem bootstrap ──────────────────────────

        // 1. Create the manager event channel (broadcast for multiple consumers)
        let (manager_event_tx, manager_event_rx) =
            tokio::sync::broadcast::channel::<FileTransferEvent>(256);

        // 2. Create the FileTransferManager
        let ft_config = FileTransferConfig::default();
        let ft_manager = FileTransferManager::new(ft_config, manager_event_tx);

        // 3. Start the axum file transfer HTTP server on an ephemeral local port
        let ft_listener_port = start_file_transfer_listener(Arc::clone(&ft_manager))
            .await
            .map_err(|e| format!("Failed to start file transfer listener: {e}"))?;
        info!(port = ft_listener_port, "File transfer HTTP listener started");

        // 4. Create a DialFn for the adapter (dials through the Go sidecar bridge)
        let dial_fn = create_dial_fn(&runtime);

        // 5. Create the FileTransferAdapter
        let (bus_tx, bus_rx) = mpsc::unbounded_channel();
        let (adapter_event_tx, _adapter_event_rx) = mpsc::unbounded_channel();

        let device_id = runtime.device_id().await;
        let local_addr = format!("127.0.0.1:{ft_listener_port}");

        let ft_adapter = FileTransferAdapter::new(
            FileTransferAdapterConfig {
                local_device_id: device_id,
                local_addr,
                output_dir: dirs::download_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join("truffle")
                    .to_string_lossy()
                    .to_string(),
                dial_fn,
            },
            Arc::clone(&ft_manager),
            bus_tx,
            adapter_event_tx,
        );

        // 6. Wire the adapter to the mesh node (incoming/outgoing message pumps)
        let mesh_node = runtime.mesh_node();
        let (ft_incoming_handle, ft_outgoing_handle) =
            truffle_core::integration::wire_file_transfer(mesh_node, &ft_adapter, bus_rx);

        // 7. Spawn the manager event -> adapter event forwarding loop
        let adapter_for_events = Arc::clone(&ft_adapter);
        let event_loop_handle = tokio::spawn(async move {
            forward_manager_events(manager_event_rx, &adapter_for_events).await;
        });

        // 8. Spawn the manager cleanup loop
        let manager_for_cleanup = Arc::clone(&ft_manager);
        tokio::spawn(async move {
            manager_for_cleanup.cleanup_loop().await;
        });

        // Log spawned task handles (they run until shutdown)
        info!(
            ft_incoming = ?ft_incoming_handle.id(),
            ft_outgoing = ?ft_outgoing_handle.id(),
            ft_events = ?event_loop_handle.id(),
            "File transfer subsystem wired to mesh"
        );

        // ── End file transfer bootstrap ────────────────────────────────

        // Write PID file
        pid::write_pid(&pid_path).map_err(|e| format!("Failed to write PID file: {e}"))?;

        // Remove stale socket file if it exists (Unix only; Windows named
        // pipes are kernel objects and don't leave stale files).
        #[cfg(unix)]
        if socket_path.exists() {
            std::fs::remove_file(&socket_path)
                .map_err(|e| format!("Failed to remove stale socket: {e}"))?;
        }

        info!(
            socket = %socket_path.display(),
            pid = std::process::id(),
            "Daemon starting"
        );

        Ok(Self {
            runtime,
            file_transfer_adapter: ft_adapter,
            file_transfer_manager: ft_manager,
            socket_path,
            pid_path,
            shutdown: Arc::new(Notify::new()),
            started_at: Instant::now(),
        })
    }

    /// Run the accept loop.
    ///
    /// Listens for incoming Unix socket connections and dispatches JSON-RPC
    /// requests. Runs until a shutdown signal is received (via `shutdown` RPC
    /// or SIGTERM/SIGINT).
    pub async fn run(&self) -> Result<(), String> {
        let listener = ipc::IpcListener::bind(&self.socket_path)
            .map_err(|e| format!("Failed to bind IPC at {}: {e}", self.socket_path.display()))?;

        info!(
            socket = %self.socket_path.display(),
            "Daemon listening for connections"
        );

        let shutdown = self.shutdown.clone();

        // Set up signal handlers
        let shutdown_signal = self.shutdown.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for Ctrl+C");
            info!("Received Ctrl+C, shutting down");
            shutdown_signal.notify_one();
        });

        #[cfg(unix)]
        {
            let shutdown_term = self.shutdown.clone();
            tokio::spawn(async move {
                let mut sigterm =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("Failed to listen for SIGTERM");
                sigterm.recv().await;
                info!("Received SIGTERM, shutting down");
                shutdown_term.notify_one();
            });
        }

        // Build the shared DaemonContext for all connections
        let ctx = Arc::new(DaemonContext {
            runtime: self.runtime.clone(),
            file_transfer_adapter: self.file_transfer_adapter.clone(),
            file_transfer_manager: self.file_transfer_manager.clone(),
            shutdown_signal: self.shutdown.clone(),
            started_at: self.started_at,
        });

        loop {
            tokio::select! {
                _ = shutdown.notified() => {
                    info!("Shutdown signal received");
                    break;
                }
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok(stream) => {
                            let ctx = Arc::clone(&ctx);
                            tokio::spawn(async move {
                                Self::handle_connection(stream, &ctx).await;
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept connection: {e}");
                        }
                    }
                }
            }
        }

        self.cleanup().await;
        Ok(())
    }

    /// Handle a single client connection.
    ///
    /// Reads newline-delimited JSON-RPC requests and sends back responses.
    /// For long-running operations (like file transfers), the handler may
    /// send JSON-RPC notifications (lines without an `id` field) before
    /// the final response.
    async fn handle_connection(stream: ipc::IpcStream, ctx: &Arc<DaemonContext>) {
        let (mut reader, mut writer) = stream.into_split();

        while let Ok(Some(line)) = reader.next_line().await {
            if line.trim().is_empty() {
                continue;
            }

            let request: DaemonRequest = match serde_json::from_str(&line) {
                Ok(req) => req,
                Err(e) => {
                    let err_resp = super::protocol::DaemonResponse::error(
                        0,
                        super::protocol::error_code::PARSE_ERROR,
                        format!("Invalid JSON: {e}"),
                    );
                    let resp_json = serde_json::to_string(&err_resp).unwrap_or_default();
                    let _ = writer.write_line(&resp_json).await;
                    continue;
                }
            };

            // Create a notification channel for this request.
            // The handler can send notifications (e.g., progress events)
            // which we forward to the client as JSON lines before the
            // final response.
            let (notif_tx, mut notif_rx) =
                tokio::sync::mpsc::unbounded_channel::<super::protocol::DaemonNotification>();

            // Spawn the handler in a separate task so we can concurrently
            // drain notifications while waiting for the final response.
            let ctx_clone = Arc::clone(ctx);
            let mut dispatch_handle = tokio::spawn(async move {
                handler::dispatch(&request, &ctx_clone, notif_tx).await
            });

            // Forward notifications as they arrive, then wait for the final response.
            loop {
                tokio::select! {
                    // Bias towards draining notifications before checking the
                    // dispatch result, so progress lines arrive in order.
                    biased;

                    Some(notif) = notif_rx.recv() => {
                        if let Ok(notif_json) = serde_json::to_string(&notif) {
                            if writer.write_line(&notif_json).await.is_err() {
                                // Client disconnected; abort the handler
                                dispatch_handle.abort();
                                return;
                            }
                        }
                    }

                    result = &mut dispatch_handle => {
                        // Drain any remaining notifications that were sent
                        // before the handler returned.
                        while let Ok(notif) = notif_rx.try_recv() {
                            if let Ok(notif_json) = serde_json::to_string(&notif) {
                                if writer.write_line(&notif_json).await.is_err() {
                                    return;
                                }
                            }
                        }

                        // Write the final response
                        match result {
                            Ok(response) => {
                                let resp_json = match serde_json::to_string(&response) {
                                    Ok(j) => j,
                                    Err(e) => {
                                        error!("Failed to serialize response: {e}");
                                        break;
                                    }
                                };

                                if writer.write_line(&resp_json).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                error!("Handler task failed: {e}");
                            }
                        }

                        break;
                    }
                }
            }
        }
    }

    /// Subscribe to unified `TruffleEvent`s from the runtime.
    ///
    /// Used by `truffle up --foreground` to display auth URLs, online status, etc.
    pub fn subscribe(
        &self,
    ) -> tokio::sync::broadcast::Receiver<truffle_core::runtime::TruffleEvent> {
        self.runtime.subscribe()
    }

    /// Shut down the daemon: stop the runtime, remove socket and PID files.
    pub async fn shutdown(&self) {
        self.shutdown.notify_one();
    }

    /// Clean up resources on shutdown.
    async fn cleanup(&self) {
        info!("Stopping file transfer manager...");
        self.file_transfer_manager.stop().await;

        info!("Stopping runtime...");
        self.runtime.stop().await;

        // Remove socket file (Unix only; Windows named pipes are kernel objects).
        #[cfg(unix)]
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }

        // Remove PID file
        let _ = pid::remove_pid(&self.pid_path);

        info!("Daemon stopped");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// File transfer helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Start the axum file transfer HTTP server on an ephemeral local port.
///
/// Returns the port number the server is listening on.
async fn start_file_transfer_listener(
    manager: Arc<FileTransferManager>,
) -> Result<u16, String> {
    let app = file_transfer_router(manager);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind file transfer listener: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local addr: {e}"))?
        .port();

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            warn!("File transfer HTTP server exited: {e}");
        }
    });

    Ok(port)
}

/// Create a `DialFn` that dials through the Go sidecar's bridge.
///
/// The DialFn is used by the file transfer sender to establish a TCP connection
/// to the receiver's axum HTTP server via the Tailscale network.
fn create_dial_fn(runtime: &Arc<TruffleRuntime>) -> DialFn {
    let shim = runtime.shim().clone();
    let pending = runtime.pending_dials().clone();

    Arc::new(move |addr: &str| {
        let shim = shim.clone();
        let pending = pending.clone();
        let addr = addr.to_string();

        Box::pin(async move {
            // Parse host:port from addr
            let (host, port_str) = addr.rsplit_once(':').ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("invalid addr: {addr}"),
                )
            })?;
            let port: u16 = port_str.parse().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("invalid port: {port_str}"),
                )
            })?;

            let request_id = uuid::Uuid::new_v4().to_string();
            let (tx, rx) = tokio::sync::oneshot::channel();

            // Insert pending dial BEFORE sending dial command (prevents race)
            {
                let mut dials = pending.lock().await;
                dials.insert(request_id.clone(), tx);
            }

            // Tell Go sidecar to dial the target
            {
                let shim_guard = shim.lock().await;
                let shim_ref = shim_guard.as_ref().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotConnected,
                        "no sidecar running",
                    )
                })?;

                if let Err(e) = shim_ref
                    .dial_raw(host.to_string(), port, request_id.clone())
                    .await
                {
                    let mut dials = pending.lock().await;
                    dials.remove(&request_id);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        format!("dial command failed: {e}"),
                    ));
                }
            } // Release shim lock during network wait

            // Await bridge connection with timeout
            let timeout = std::time::Duration::from_secs(10);
            let bridge_conn = match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(conn)) => conn,
                Ok(Err(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "dial cancelled (sender dropped)",
                    ));
                }
                Err(_) => {
                    let mut dials = pending.lock().await;
                    dials.remove(&request_id);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("dial timed out after {timeout:?}"),
                    ));
                }
            };

            // The bridge connection gives us a raw TCP stream
            Ok(bridge_conn.stream)
        })
    })
}

/// Forward events from the FileTransferManager's event channel to the adapter.
///
/// Runs until the event channel is closed (typically at shutdown).
async fn forward_manager_events(
    mut event_rx: tokio::sync::broadcast::Receiver<FileTransferEvent>,
    adapter: &Arc<FileTransferAdapter>,
) {
    loop {
        match event_rx.recv().await {
            Ok(event) => adapter.handle_manager_event(event).await,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Manager event receiver lagged, skipped {n} events");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}
