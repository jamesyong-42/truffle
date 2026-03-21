//! Daemon server: Unix socket listener with JSON-RPC dispatch.
//!
//! The server owns a `TruffleRuntime`, listens on a Unix socket, and
//! dispatches incoming JSON-RPC requests to the handler module.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Notify;
use tracing::{error, info};
use truffle_core::runtime::TruffleRuntime;

use super::handler;
use super::pid;
use super::protocol::DaemonRequest;
use crate::config::TruffleConfig;
use crate::sidecar;

/// The daemon server process.
pub struct DaemonServer {
    /// The runtime instance (owned by the daemon for the lifetime of the process).
    runtime: Arc<TruffleRuntime>,
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
    /// 2. Write the PID file.
    /// 3. Bind the Unix socket.
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
        let hostname = config
            .node
            .name
            .as_str();
        let (runtime, _mesh_rx) = TruffleRuntime::builder()
            .hostname(hostname)
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

        // Write PID file
        pid::write_pid(&pid_path).map_err(|e| format!("Failed to write PID file: {e}"))?;

        // Remove stale socket file if it exists
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
        let listener = UnixListener::bind(&self.socket_path)
            .map_err(|e| format!("Failed to bind Unix socket at {}: {e}", self.socket_path.display()))?;

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

        loop {
            tokio::select! {
                _ = shutdown.notified() => {
                    info!("Shutdown signal received");
                    break;
                }
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _addr)) => {
                            let runtime = self.runtime.clone();
                            let started_at = self.started_at;
                            let shutdown_clone = self.shutdown.clone();
                            tokio::spawn(async move {
                                Self::handle_connection(stream, &runtime, started_at, &shutdown_clone).await;
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
    async fn handle_connection(
        stream: tokio::net::UnixStream,
        runtime: &Arc<TruffleRuntime>,
        started_at: Instant,
        shutdown_signal: &Arc<Notify>,
    ) {
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        while let Ok(Some(line)) = lines.next_line().await {
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
                    let mut resp_json = serde_json::to_string(&err_resp).unwrap_or_default();
                    resp_json.push('\n');
                    let _ = writer.write_all(resp_json.as_bytes()).await;
                    continue;
                }
            };

            let response =
                handler::dispatch(&request, runtime, started_at, shutdown_signal).await;

            let mut resp_json = match serde_json::to_string(&response) {
                Ok(j) => j,
                Err(e) => {
                    error!("Failed to serialize response: {e}");
                    continue;
                }
            };
            resp_json.push('\n');

            if writer.write_all(resp_json.as_bytes()).await.is_err() {
                // Client disconnected
                break;
            }
        }
    }

    /// Subscribe to unified `TruffleEvent`s from the runtime.
    ///
    /// Used by `truffle up --foreground` to display auth URLs, online status, etc.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<truffle_core::runtime::TruffleEvent> {
        self.runtime.subscribe()
    }

    /// Shut down the daemon: stop the runtime, remove socket and PID files.
    pub async fn shutdown(&self) {
        self.shutdown.notify_one();
    }

    /// Clean up resources on shutdown.
    async fn cleanup(&self) {
        info!("Stopping runtime...");
        self.runtime.stop().await;

        // Remove socket file
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }

        // Remove PID file
        let _ = pid::remove_pid(&self.pid_path);

        info!("Daemon stopped");
    }
}
