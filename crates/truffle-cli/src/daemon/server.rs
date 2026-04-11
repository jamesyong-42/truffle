//! Daemon server: IPC listener with JSON-RPC dispatch.
//!
//! The server owns a `Node<TailscaleProvider>`, listens on IPC, and dispatches
//! incoming JSON-RPC requests to the handler module.
//!
//! This is MUCH simpler than the v1 server — no FileTransferManager, no
//! BridgeManager, no DialFn, no axum HTTP server. Just a Node and IPC.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Notify;
use tracing::{error, info};
use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::node::Node;
use truffle_core::session::PeerEvent;

use super::handler::{DaemonContext, DispatchResult};
use super::ipc;
use super::pid;
use super::protocol::DaemonRequest;
use crate::config::TruffleConfig;

/// The daemon server process.
pub struct DaemonServer {
    /// The Node instance (owned by the daemon for the lifetime of the process).
    node: Arc<Node<TailscaleProvider>>,
    /// Path to the Unix socket file.
    socket_path: PathBuf,
    /// Path to the PID file.
    pid_path: PathBuf,
    /// Shutdown signal.
    shutdown: Arc<Notify>,
    /// When the server started (for uptime tracking).
    started_at: Instant,
}

impl DaemonServer {
    /// Start the daemon server.
    ///
    /// 1. Build a `Node` from config (this starts the sidecar, auth, WS listener).
    /// 2. Write the PID file.
    /// 3. Return the server instance; call `run()` to enter the accept loop.
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

        // Resolve sidecar path
        let sidecar_path = resolve_sidecar_path(config)?;
        info!(sidecar = %sidecar_path.display(), "Using sidecar binary");

        // Resolve state directory
        let state_dir = resolve_state_dir(config)?;

        // Build the Node — this is the ONLY thing we need.
        // No FileTransferManager, no BridgeManager, no DialFn, no axum.
        //
        // Ensure the tsnet hostname starts with "truffle-" so that other
        // truffle nodes recognise this peer (the peer-discovery filter in
        // TailscaleProvider::is_truffle_peer checks for this prefix).
        let tsnet_hostname = if config.node.name.starts_with("truffle-") {
            config.node.name.clone()
        } else {
            format!("truffle-{}", config.node.name)
        };
        let mut builder = Node::<TailscaleProvider>::builder()
            .name(&tsnet_hostname)
            .sidecar_path(&sidecar_path)
            .state_dir(&state_dir)
            .ws_port(9417);

        if !config.node.auth_key.is_empty() {
            builder = builder.auth_key(&config.node.auth_key);
        }

        let node = builder
            .build()
            .await
            .map_err(|e| format!("Failed to build node: {e}"))?;

        let node = Arc::new(node);

        // Write PID file
        pid::write_pid(&pid_path).map_err(|e| format!("Failed to write PID file: {e}"))?;

        // Remove stale socket file
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
            node,
            socket_path,
            pid_path,
            shutdown: Arc::new(Notify::new()),
            started_at: Instant::now(),
        })
    }

    /// Run the IPC accept loop.
    pub async fn run(&self) -> Result<(), String> {
        let listener = ipc::IpcListener::bind(&self.socket_path)
            .map_err(|e| format!("Failed to bind IPC at {}: {e}", self.socket_path.display()))?;

        info!(
            socket = %self.socket_path.display(),
            "Daemon listening for connections"
        );

        let shutdown = self.shutdown.clone();

        // Signal handlers
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

        let ctx = Arc::new(DaemonContext {
            node: self.node.clone(),
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

            let (notif_tx, mut notif_rx) =
                tokio::sync::mpsc::unbounded_channel::<super::protocol::DaemonNotification>();

            let ctx_clone = Arc::clone(ctx);
            let notif_tx_clone = notif_tx.clone();
            let mut dispatch_handle = tokio::spawn(async move {
                super::handler::dispatch(&request, &ctx_clone, notif_tx_clone).await
            });

            // For normal requests: stream notifications while waiting for the
            // response (same as original behavior). For subscribe: enter
            // streaming mode where the connection stays open indefinitely.
            loop {
                tokio::select! {
                    biased;

                    Some(notif) = notif_rx.recv() => {
                        if let Ok(notif_json) = serde_json::to_string(&notif) {
                            if writer.write_line(&notif_json).await.is_err() {
                                dispatch_handle.abort();
                                return;
                            }
                        }
                    }

                    result = &mut dispatch_handle => {
                        match result {
                            Ok(DispatchResult::Response(response)) => {
                                // Drain remaining notifications
                                while let Ok(notif) = notif_rx.try_recv() {
                                    if let Ok(notif_json) = serde_json::to_string(&notif) {
                                        if writer.write_line(&notif_json).await.is_err() {
                                            return;
                                        }
                                    }
                                }

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

                            Ok(DispatchResult::Subscribe(params)) => {
                                // Enter streaming mode.
                                let ctx_for_sub = Arc::clone(ctx);

                                let subscribe_handle = tokio::spawn(async move {
                                    super::handler::run_subscribe(
                                        &params,
                                        &ctx_for_sub,
                                        notif_tx,
                                    )
                                    .await;
                                });

                                // Forward notifications until client disconnects.
                                loop {
                                    tokio::select! {
                                        biased;

                                        Some(notif) = notif_rx.recv() => {
                                            if let Ok(notif_json) = serde_json::to_string(&notif) {
                                                if writer.write_line(&notif_json).await.is_err() {
                                                    subscribe_handle.abort();
                                                    return;
                                                }
                                            }
                                        }

                                        client_line = reader.next_line() => {
                                            match client_line {
                                                Ok(None) | Err(_) => {
                                                    subscribe_handle.abort();
                                                    return;
                                                }
                                                Ok(Some(_)) => {
                                                    // Ignore further requests during subscribe.
                                                }
                                            }
                                        }
                                    }
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

    /// Get a peer event subscriber for the foreground display.
    pub fn subscribe_peer_events(&self) -> tokio::sync::broadcast::Receiver<PeerEvent> {
        self.node.on_peer_change()
    }

    /// Get the node reference.
    pub fn node(&self) -> &Arc<Node<TailscaleProvider>> {
        &self.node
    }

    /// Signal shutdown.
    pub async fn shutdown(&self) {
        self.shutdown.notify_one();
    }

    /// Clean up resources on shutdown.
    async fn cleanup(&self) {
        info!("Stopping node...");
        self.node.stop().await;

        #[cfg(unix)]
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }

        let _ = pid::remove_pid(&self.pid_path);
        info!("Daemon stopped");
    }
}

// ==========================================================================
// Helper functions
// ==========================================================================

/// Find the Go sidecar binary.
fn resolve_sidecar_path(config: &TruffleConfig) -> Result<PathBuf, String> {
    if !config.node.sidecar_path.is_empty() {
        let p = PathBuf::from(&config.node.sidecar_path);
        if p.exists() {
            return Ok(p);
        }
        return Err(format!(
            "Configured sidecar path does not exist: {}",
            p.display()
        ));
    }

    // Auto-discover: check common locations
    // The sidecar binary may be named "sidecar-slim" (original) or "truffle-sidecar"
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let config_bin = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("truffle")
        .join("bin");

    let names = if cfg!(windows) {
        &[
            "sidecar-slim.exe",
            "truffle-sidecar.exe",
            "sidecar-slim",
            "truffle-sidecar",
        ][..]
    } else {
        &["sidecar-slim", "truffle-sidecar"][..]
    };

    let mut candidates: Vec<Option<PathBuf>> = Vec::new();
    for name in names {
        // Same directory as the CLI binary
        candidates.push(exe_dir.as_ref().map(|d| d.join(name)));
        // Config bin directory
        candidates.push(Some(config_bin.join(name)));
    }
    if !cfg!(windows) {
        candidates.push(Some(PathBuf::from("/usr/local/bin/sidecar-slim")));
        candidates.push(Some(PathBuf::from("/usr/local/bin/truffle-sidecar")));
    }

    for candidate in candidates.into_iter().flatten() {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err("Could not find the Go sidecar binary. \
         Install it with 'truffle install-sidecar' or set node.sidecar_path in config."
        .to_string())
}

/// Resolve the Tailscale state directory.
fn resolve_state_dir(config: &TruffleConfig) -> Result<String, String> {
    if !config.node.state_dir.is_empty() {
        let dir = PathBuf::from(&config.node.state_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create state directory {}: {e}", dir.display()))?;
        return Ok(config.node.state_dir.clone());
    }

    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("truffle")
        .join("state");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create state directory {}: {e}", dir.display()))?;
    Ok(dir.to_string_lossy().to_string())
}
