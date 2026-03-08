use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, Notify};
use tokio::net::TcpStream;

use super::protocol::{
    command_type, event_type, DialCommandData, DialResultEventData, ShimCommand, ShimEvent,
    StartCommandData,
};

/// Maximum exponential backoff delay for auto-restart.
const MAX_RESTART_DELAY: Duration = Duration::from_secs(30);

/// Initial restart delay.
const INITIAL_RESTART_DELAY: Duration = Duration::from_secs(1);

/// Errors from GoShim operations.
#[derive(Debug, thiserror::Error)]
pub enum ShimError {
    #[error("shim process not running")]
    NotRunning,

    #[error("failed to spawn shim: {0}")]
    SpawnFailed(std::io::Error),

    #[error("failed to send command: {0}")]
    SendFailed(String),

    #[error("dial cancelled (shim crashed or sender dropped)")]
    DialCancelled,

    #[error("dial timed out after {0:?}")]
    DialTimeout(Duration),

    #[error("dial failed: {0}")]
    DialFailed(String),

    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Events emitted by the GoShim for the application layer.
#[derive(Debug, Clone)]
pub enum ShimLifecycleEvent {
    /// Shim process started successfully.
    Started,
    /// Shim crashed unexpectedly.
    Crashed {
        exit_code: Option<i32>,
        stderr_tail: String,
    },
    /// Shim requires Tailscale authentication.
    AuthRequired { auth_url: String },
    /// Shim status update.
    Status(super::protocol::StatusEventData),
    /// Peer list update.
    Peers(super::protocol::PeersEventData),
    /// Shim stopped gracefully.
    Stopped,
}

/// Configuration for spawning the Go shim.
#[derive(Debug, Clone)]
pub struct ShimConfig {
    /// Path to the Go shim binary.
    pub binary_path: PathBuf,
    /// Hostname for the tsnet node.
    pub hostname: String,
    /// State directory for tsnet.
    pub state_dir: String,
    /// Optional Tailscale auth key.
    pub auth_key: Option<String>,
    /// Bridge port that Rust is listening on.
    pub bridge_port: u16,
    /// Session token (32-byte hex string).
    pub session_token: String,
    /// Whether to auto-restart on crash.
    pub auto_restart: bool,
}

/// Manages the Go shim child process.
///
/// Handles spawning, command sending, event reading, crash recovery,
/// and dial correlation.
pub struct GoShim {
    #[allow(dead_code)]
    config: ShimConfig,
    /// Channel sender for writing commands to the shim's stdin.
    stdin_tx: mpsc::Sender<String>,
    /// Pending outgoing dials waiting for bridge connections.
    pending_dials: Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>>,
    /// Lifecycle events for the application layer.
    lifecycle_tx: broadcast::Sender<ShimLifecycleEvent>,
    /// Signal to stop the shim.
    shutdown: Arc<Notify>,
    /// Whether auto-restart is paused (e.g., due to auth storm).
    auto_restart_paused: Arc<std::sync::atomic::AtomicBool>,
}

impl GoShim {
    /// Create a new GoShim and spawn the child process.
    ///
    /// Returns the GoShim handle and a broadcast receiver for lifecycle events.
    pub async fn spawn(config: ShimConfig) -> Result<(Self, broadcast::Receiver<ShimLifecycleEvent>), ShimError> {
        let (lifecycle_tx, lifecycle_rx) = broadcast::channel(64);
        let (stdin_tx, stdin_rx) = mpsc::channel::<String>(64);
        let pending_dials: Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let shutdown = Arc::new(Notify::new());
        let auto_restart_paused =
            Arc::new(std::sync::atomic::AtomicBool::new(false));

        let shim = GoShim {
            config: config.clone(),
            stdin_tx,
            pending_dials: pending_dials.clone(),
            lifecycle_tx: lifecycle_tx.clone(),
            shutdown: shutdown.clone(),
            auto_restart_paused: auto_restart_paused.clone(),
        };

        // Spawn the child process management task
        Self::spawn_manager_task(
            config,
            stdin_rx,
            pending_dials,
            lifecycle_tx.clone(),
            shutdown,
            auto_restart_paused,
        );

        Ok((shim, lifecycle_rx))
    }

    fn spawn_manager_task(
        config: ShimConfig,
        stdin_rx: mpsc::Receiver<String>,
        pending_dials: Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>>,
        lifecycle_tx: broadcast::Sender<ShimLifecycleEvent>,
        shutdown: Arc<Notify>,
        auto_restart_paused: Arc<std::sync::atomic::AtomicBool>,
    ) {
        tokio::spawn(async move {
            let stdin_rx = Arc::new(Mutex::new(stdin_rx));
            let mut restart_delay = INITIAL_RESTART_DELAY;
            let mut last_event_was_auth = false;

            loop {
                // Spawn the child process
                let child = match Self::spawn_child(&config) {
                    Ok(child) => child,
                    Err(e) => {
                        tracing::error!("failed to spawn Go shim: {e}");
                        let _ = lifecycle_tx.send(ShimLifecycleEvent::Crashed {
                            exit_code: None,
                            stderr_tail: e.to_string(),
                        });

                        if !config.auto_restart {
                            break;
                        }

                        tokio::select! {
                            _ = tokio::time::sleep(restart_delay) => {}
                            _ = shutdown.notified() => break,
                        }
                        restart_delay = (restart_delay * 2).min(MAX_RESTART_DELAY);
                        continue;
                    }
                };

                let exit_status = Self::run_child(
                    child,
                    &config,
                    &stdin_rx,
                    &pending_dials,
                    &lifecycle_tx,
                    &shutdown,
                    &mut last_event_was_auth,
                )
                .await;

                // Drain all pending dials on exit
                {
                    let mut dials = pending_dials.lock().await;
                    let count = dials.len();
                    dials.clear();
                    if count > 0 {
                        tracing::warn!("drained {count} pending dials after shim exit");
                    }
                }

                match exit_status {
                    ChildExit::Shutdown => break,
                    ChildExit::Crashed { code, stderr_tail } => {
                        tracing::error!("Go shim crashed with exit code {code:?}");
                        let _ = lifecycle_tx.send(ShimLifecycleEvent::Crashed {
                            exit_code: code,
                            stderr_tail: stderr_tail.clone(),
                        });

                        if !config.auto_restart {
                            break;
                        }

                        // Restart storm prevention
                        if last_event_was_auth {
                            tracing::warn!(
                                "shim crashed after auth required — pausing auto-restart"
                            );
                            auto_restart_paused.store(
                                true,
                                std::sync::atomic::Ordering::Relaxed,
                            );
                            // Wait for shutdown or resume
                            shutdown.notified().await;
                            break;
                        }

                        tokio::select! {
                            _ = tokio::time::sleep(restart_delay) => {}
                            _ = shutdown.notified() => break,
                        }
                        restart_delay = (restart_delay * 2).min(MAX_RESTART_DELAY);
                    }
                    ChildExit::GracefulStop => {
                        let _ = lifecycle_tx.send(ShimLifecycleEvent::Stopped);
                        break;
                    }
                }
            }
        });
    }

    fn spawn_child(config: &ShimConfig) -> Result<Child, std::io::Error> {
        Command::new(&config.binary_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
    }

    async fn run_child(
        mut child: Child,
        config: &ShimConfig,
        stdin_rx: &Arc<Mutex<mpsc::Receiver<String>>>,
        pending_dials: &Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>>,
        lifecycle_tx: &broadcast::Sender<ShimLifecycleEvent>,
        shutdown: &Arc<Notify>,
        last_event_was_auth: &mut bool,
    ) -> ChildExit {
        let mut child_stdin = child.stdin.take().expect("stdin was piped");
        let child_stdout = child.stdout.take().expect("stdout was piped");
        let child_stderr = child.stderr.take().expect("stderr was piped");

        // Send the start command
        let start_data = StartCommandData {
            hostname: config.hostname.clone(),
            state_dir: config.state_dir.clone(),
            auth_key: config.auth_key.clone(),
            bridge_port: config.bridge_port,
            session_token: config.session_token.clone(),
        };
        let start_cmd = ShimCommand {
            command: command_type::START,
            data: Some(serde_json::to_value(&start_data).unwrap()),
        };
        let start_json = serde_json::to_string(&start_cmd).unwrap();
        if let Err(e) = child_stdin.write_all(start_json.as_bytes()).await {
            tracing::error!("failed to write start command: {e}");
            let _ = child.kill().await;
            return ChildExit::Crashed {
                code: None,
                stderr_tail: e.to_string(),
            };
        }
        if let Err(e) = child_stdin.write_all(b"\n").await {
            tracing::error!("failed to write newline: {e}");
            let _ = child.kill().await;
            return ChildExit::Crashed {
                code: None,
                stderr_tail: e.to_string(),
            };
        }
        let _ = child_stdin.flush().await;

        // Read stdout lines in a task
        let mut stdout_reader = BufReader::new(child_stdout).lines();
        let lifecycle_tx2 = lifecycle_tx.clone();
        let pending_dials2 = pending_dials.clone();

        // Capture stderr in a task
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_buf2 = stderr_buf.clone();
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(child_stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            tracing::warn!(target: "go_shim", "{}", trimmed);
                            let mut buf = stderr_buf2.lock().await;
                            // Keep last 1KB of stderr
                            if buf.len() > 1024 {
                                let drain_at = buf.len() - 512;
                                buf.drain(..drain_at);
                            }
                            buf.push_str(trimmed);
                            buf.push('\n');
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut graceful_stop = false;
        *last_event_was_auth = false;

        // Main event loop
        loop {
            tokio::select! {
                // Read events from shim stdout
                line = stdout_reader.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            if line.trim().is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<ShimEvent>(&line) {
                                Ok(event) => {
                                    *last_event_was_auth = event.event == event_type::AUTH_REQUIRED;
                                    Self::handle_event(
                                        event,
                                        &lifecycle_tx2,
                                        &pending_dials2,
                                    ).await;
                                }
                                Err(e) => {
                                    tracing::warn!("failed to parse shim event: {e}: {line}");
                                }
                            }
                        }
                        Ok(None) => {
                            // stdout closed — process exiting
                            break;
                        }
                        Err(e) => {
                            tracing::error!("error reading shim stdout: {e}");
                            break;
                        }
                    }
                }

                // Forward commands from stdin_tx to child stdin
                cmd = async {
                    let mut rx = stdin_rx.lock().await;
                    rx.recv().await
                } => {
                    match cmd {
                        Some(json_line) => {
                            if let Err(e) = child_stdin.write_all(json_line.as_bytes()).await {
                                tracing::error!("failed to write command to shim: {e}");
                                break;
                            }
                            if let Err(e) = child_stdin.write_all(b"\n").await {
                                tracing::error!("failed to write newline to shim: {e}");
                                break;
                            }
                            let _ = child_stdin.flush().await;
                        }
                        None => {
                            // All senders dropped — shutting down
                            graceful_stop = true;
                            break;
                        }
                    }
                }

                // Shutdown signal
                _ = shutdown.notified() => {
                    graceful_stop = true;
                    // Send stop command before killing
                    let stop_cmd = ShimCommand {
                        command: command_type::STOP,
                        data: None,
                    };
                    if let Ok(json) = serde_json::to_string(&stop_cmd) {
                        let _ = child_stdin.write_all(json.as_bytes()).await;
                        let _ = child_stdin.write_all(b"\n").await;
                        let _ = child_stdin.flush().await;
                    }
                    // Give the shim a moment to shut down gracefully
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        child.wait(),
                    ).await;
                    break;
                }
            }
        }

        // Wait for child exit
        let exit_code = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) => status.code(),
            _ => {
                let _ = child.kill().await;
                None
            }
        };

        stderr_task.abort();
        let stderr_tail = stderr_buf.lock().await.clone();

        if graceful_stop {
            ChildExit::GracefulStop
        } else {
            ChildExit::Crashed {
                code: exit_code,
                stderr_tail,
            }
        }
    }

    async fn handle_event(
        event: ShimEvent,
        lifecycle_tx: &broadcast::Sender<ShimLifecycleEvent>,
        pending_dials: &Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>>,
    ) {
        match event.event.as_str() {
            event_type::STARTED => {
                let _ = lifecycle_tx.send(ShimLifecycleEvent::Started);
            }
            event_type::STOPPED => {
                let _ = lifecycle_tx.send(ShimLifecycleEvent::Stopped);
            }
            event_type::STATUS => {
                if let Ok(data) = serde_json::from_value(event.data) {
                    let _ = lifecycle_tx.send(ShimLifecycleEvent::Status(data));
                }
            }
            event_type::AUTH_REQUIRED => {
                if let Ok(data) = serde_json::from_value::<super::protocol::AuthRequiredEventData>(event.data) {
                    let _ = lifecycle_tx.send(ShimLifecycleEvent::AuthRequired {
                        auth_url: data.auth_url,
                    });
                }
            }
            event_type::PEERS => {
                if let Ok(data) = serde_json::from_value(event.data) {
                    let _ = lifecycle_tx.send(ShimLifecycleEvent::Peers(data));
                }
            }
            event_type::DIAL_RESULT => {
                if let Ok(data) = serde_json::from_value::<DialResultEventData>(event.data) {
                    if !data.success {
                        // Remove pending dial and drop sender to signal error
                        let mut dials = pending_dials.lock().await;
                        if dials.remove(&data.request_id).is_some() {
                            tracing::debug!(
                                "dial failed for request_id={}: {}",
                                data.request_id,
                                data.error
                            );
                        }
                    }
                    // On success, the bridge connection arrives via TCP accept loop,
                    // which correlates by request_id in the header.
                }
            }
            event_type::ERROR => {
                if let Ok(data) = serde_json::from_value::<super::protocol::ErrorEventData>(event.data) {
                    tracing::error!("shim error [{}]: {}", data.code, data.message);
                }
            }
            other => {
                tracing::debug!("unknown shim event: {other}");
            }
        }
    }

    /// Send a command to the Go shim.
    pub async fn send_command(&self, cmd: ShimCommand) -> Result<(), ShimError> {
        let json = serde_json::to_string(&cmd)?;
        self.stdin_tx
            .send(json)
            .await
            .map_err(|_| ShimError::NotRunning)
    }

    /// Request the Go shim to dial a remote peer.
    ///
    /// Returns a oneshot receiver that will receive the bridged TcpStream
    /// once the Go shim connects back to our local bridge port.
    pub async fn dial(
        &self,
        target: String,
        port: u16,
    ) -> Result<oneshot::Receiver<TcpStream>, ShimError> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        // Step 1: Insert BEFORE sending command (prevents race)
        {
            let mut dials = self.pending_dials.lock().await;
            dials.insert(request_id.clone(), tx);
        }

        // Step 2: Send command to Go
        let data = DialCommandData {
            request_id: request_id.clone(),
            target,
            port,
        };
        let cmd = ShimCommand {
            command: command_type::DIAL,
            data: Some(serde_json::to_value(&data)?),
        };

        if let Err(e) = self.send_command(cmd).await {
            // Clean up on send failure
            let mut dials = self.pending_dials.lock().await;
            dials.remove(&request_id);
            return Err(e);
        }

        Ok(rx)
    }

    /// Request peers list from the Go shim.
    pub async fn get_peers(&self) -> Result<(), ShimError> {
        let cmd = ShimCommand {
            command: command_type::GET_PEERS,
            data: None,
        };
        self.send_command(cmd).await
    }

    /// Request the Go shim to stop gracefully.
    pub async fn stop(&self) -> Result<(), ShimError> {
        let cmd = ShimCommand {
            command: command_type::STOP,
            data: None,
        };
        self.send_command(cmd).await
    }

    /// Signal the shim manager to shut down completely.
    pub fn shutdown(&self) {
        self.shutdown.notify_one();
    }

    /// Subscribe to lifecycle events.
    pub fn subscribe(&self) -> broadcast::Receiver<ShimLifecycleEvent> {
        self.lifecycle_tx.subscribe()
    }

    /// Access pending dials map (used by BridgeManager to deliver incoming connections).
    pub fn pending_dials(&self) -> &Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>> {
        &self.pending_dials
    }

    /// Check if auto-restart is paused due to auth storm.
    pub fn is_auto_restart_paused(&self) -> bool {
        self.auto_restart_paused
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Resume auto-restart (e.g., after user completes auth).
    pub fn resume_auto_restart(&self) {
        self.auto_restart_paused
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // Notify to trigger a restart attempt
        self.shutdown.notify_one();
    }
}

enum ChildExit {
    #[allow(dead_code)]
    Shutdown,
    GracefulStop,
    Crashed {
        code: Option<i32>,
        stderr_tail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_config_clone() {
        let config = ShimConfig {
            binary_path: PathBuf::from("/usr/bin/truffle-shim"),
            hostname: "test-node".to_string(),
            state_dir: "/tmp/tsnet".to_string(),
            auth_key: None,
            bridge_port: 12345,
            session_token: "aa".repeat(32),
            auto_restart: true,
        };
        let config2 = config.clone();
        assert_eq!(config.hostname, config2.hostname);
        assert_eq!(config.bridge_port, config2.bridge_port);
    }

    #[test]
    fn lifecycle_event_debug() {
        let event = ShimLifecycleEvent::Crashed {
            exit_code: Some(1),
            stderr_tail: "panic: runtime error".to_string(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("Crashed"));
        assert!(debug.contains("panic"));
    }

    #[tokio::test]
    async fn pending_dials_insert_and_remove() {
        let dials: Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (tx, _rx) = oneshot::channel();
        {
            let mut map = dials.lock().await;
            map.insert("test-id".to_string(), tx);
            assert!(map.contains_key("test-id"));
        }

        {
            let mut map = dials.lock().await;
            let removed = map.remove("test-id");
            assert!(removed.is_some());
            assert!(!map.contains_key("test-id"));
        }
    }

    #[tokio::test]
    async fn pending_dials_drain_on_crash() {
        let dials: Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Insert several pending dials
        let mut receivers = Vec::new();
        for i in 0..5 {
            let (tx, rx) = oneshot::channel();
            dials.lock().await.insert(format!("dial-{i}"), tx);
            receivers.push(rx);
        }

        // Drain (simulating crash recovery)
        {
            let mut map = dials.lock().await;
            assert_eq!(map.len(), 5);
            map.clear();
        }

        // All receivers should get RecvError (sender dropped)
        for rx in receivers {
            assert!(rx.await.is_err());
        }
    }
}
