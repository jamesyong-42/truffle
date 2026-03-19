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
    /// Node needs admin approval to join the tailnet.
    NeedsApproval,
    /// Tailscale state changed (e.g. NeedsLogin, Running, Stopped).
    StateChanged {
        state: String,
        auth_url: String,
    },
    /// Key is approaching expiry.
    KeyExpiring {
        expires_at: String,
    },
    /// Health subsystem warnings.
    HealthWarning {
        warnings: Vec<String>,
    },
    /// Shim status update.
    Status(super::protocol::StatusEventData),
    /// Peer list update.
    Peers(super::protocol::PeersEventData),
    /// Shim stopped gracefully.
    Stopped,
    /// A dial request failed. Caller should remove from BridgeManager::pending_dials.
    DialFailed { request_id: String, error: String },
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
    /// If true, the node is ephemeral (cleaned up when offline).
    pub ephemeral: Option<bool>,
    /// ACL tags to advertise.
    pub tags: Option<Vec<String>>,
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
    /// Separate signal for resuming auto-restart (not overloaded on shutdown).
    resume_signal: Arc<Notify>,
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
        let resume_signal = Arc::new(Notify::new());
        let auto_restart_paused =
            Arc::new(std::sync::atomic::AtomicBool::new(false));

        let shim = GoShim {
            config: config.clone(),
            stdin_tx,
            pending_dials: pending_dials.clone(),
            lifecycle_tx: lifecycle_tx.clone(),
            shutdown: shutdown.clone(),
            resume_signal: resume_signal.clone(),
            auto_restart_paused: auto_restart_paused.clone(),
        };

        // Spawn the child process management task
        Self::spawn_manager_task(
            config,
            stdin_rx,
            pending_dials,
            lifecycle_tx.clone(),
            shutdown,
            resume_signal,
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
        resume_signal: Arc<Notify>,
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
                            // Wait for resume OR shutdown (separate signals)
                            tokio::select! {
                                _ = resume_signal.notified() => {
                                    auto_restart_paused.store(false, std::sync::atomic::Ordering::Relaxed);
                                    restart_delay = INITIAL_RESTART_DELAY;
                                    continue;
                                }
                                _ = shutdown.notified() => break,
                            }
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
            ephemeral: config.ephemeral,
            tags: config.tags.clone(),
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
            event_type::NEEDS_APPROVAL => {
                let _ = lifecycle_tx.send(ShimLifecycleEvent::NeedsApproval);
            }
            event_type::STATE_CHANGE => {
                if let Ok(data) = serde_json::from_value::<super::protocol::StateChangeEventData>(event.data) {
                    let _ = lifecycle_tx.send(ShimLifecycleEvent::StateChanged {
                        state: data.state,
                        auth_url: data.auth_url,
                    });
                }
            }
            event_type::KEY_EXPIRING => {
                if let Ok(data) = serde_json::from_value::<super::protocol::KeyExpiringEventData>(event.data) {
                    let _ = lifecycle_tx.send(ShimLifecycleEvent::KeyExpiring {
                        expires_at: data.expires_at,
                    });
                }
            }
            event_type::HEALTH_WARNING => {
                if let Ok(data) = serde_json::from_value::<super::protocol::HealthWarningEventData>(event.data) {
                    let _ = lifecycle_tx.send(ShimLifecycleEvent::HealthWarning {
                        warnings: data.warnings,
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
                        // Report dial failure via lifecycle event.
                        // The caller (runtime/NAPI) handles BridgeManager cleanup.
                        let _ = lifecycle_tx.send(ShimLifecycleEvent::DialFailed {
                            request_id: data.request_id,
                            error: data.error,
                        });
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
    /// Returns the request_id. The caller must insert into BridgeManager::pending_dials
    /// BEFORE calling this, using the returned request_id.
    pub async fn dial_raw(
        &self,
        target: String,
        port: u16,
        request_id: String,
    ) -> Result<(), ShimError> {
        let data = DialCommandData {
            request_id,
            target,
            port,
        };
        let cmd = ShimCommand {
            command: command_type::DIAL,
            data: Some(serde_json::to_value(&data)?),
        };
        self.send_command(cmd).await
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
        // Notify the resume signal (separate from shutdown)
        self.resume_signal.notify_one();
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
            ephemeral: None,
            tags: None,
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

    // ── Layer 5: Command serialization + lifecycle event mapping ─────────

    #[test]
    fn start_command_serializes_correctly() {
        let data = super::super::protocol::StartCommandData {
            hostname: "test-node".to_string(),
            state_dir: "/tmp/tsnet-test".to_string(),
            auth_key: None,
            bridge_port: 54321,
            session_token: "ab".repeat(32),
            ephemeral: None,
            tags: None,
        };
        let cmd = super::super::protocol::ShimCommand {
            command: super::super::protocol::command_type::START,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();

        // The Go sidecar expects exactly these field names
        assert!(json.contains(r#""command":"tsnet:start""#));
        assert!(json.contains(r#""hostname":"test-node""#));
        assert!(json.contains(r#""stateDir":"/tmp/tsnet-test""#));
        assert!(json.contains(r#""bridgePort":54321"#));
        assert!(json.contains(r#""sessionToken":"#));
        // authKey should not be present when None
        assert!(!json.contains("authKey"));
    }

    #[test]
    fn stop_command_has_no_data() {
        let cmd = super::super::protocol::ShimCommand {
            command: super::super::protocol::command_type::STOP,
            data: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"command":"tsnet:stop"}"#);
    }

    #[test]
    fn get_peers_command_has_no_data() {
        let cmd = super::super::protocol::ShimCommand {
            command: super::super::protocol::command_type::GET_PEERS,
            data: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"command":"tsnet:getPeers"}"#);
    }

    #[test]
    fn dial_command_serializes_with_request_id() {
        let data = super::super::protocol::DialCommandData {
            request_id: "uuid-v4-here".to_string(),
            target: "my-peer.tail.ts.net".to_string(),
            port: 443,
        };
        let cmd = super::super::protocol::ShimCommand {
            command: super::super::protocol::command_type::DIAL,
            data: Some(serde_json::to_value(&data).unwrap()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"bridge:dial""#));
        assert!(json.contains(r#""requestId":"uuid-v4-here""#));
        assert!(json.contains(r#""target":"my-peer.tail.ts.net""#));
        assert!(json.contains(r#""port":443"#));
    }

    #[test]
    fn lifecycle_event_started_variant() {
        let event = ShimLifecycleEvent::Started;
        let debug = format!("{event:?}");
        assert_eq!(debug, "Started");
    }

    #[test]
    fn lifecycle_event_stopped_variant() {
        let event = ShimLifecycleEvent::Stopped;
        let debug = format!("{event:?}");
        assert_eq!(debug, "Stopped");
    }

    #[test]
    fn lifecycle_event_auth_required_variant() {
        let event = ShimLifecycleEvent::AuthRequired {
            auth_url: "https://login.tailscale.com/a/abc".to_string(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("AuthRequired"));
        assert!(debug.contains("https://login.tailscale.com"));
    }

    #[test]
    fn lifecycle_event_status_variant() {
        let data = super::super::protocol::StatusEventData {
            state: "running".to_string(),
            hostname: "node1".to_string(),
            dns_name: "node1.ts.net".to_string(),
            tailscale_ip: "100.64.0.1".to_string(),
            error: String::new(),
        };
        let event = ShimLifecycleEvent::Status(data);
        let debug = format!("{event:?}");
        assert!(debug.contains("Status"));
        assert!(debug.contains("running"));
    }

    #[test]
    fn lifecycle_event_peers_variant() {
        let data = super::super::protocol::PeersEventData {
            peers: vec![super::super::protocol::BridgeTailnetPeer {
                id: "p1".to_string(),
                hostname: "h1".to_string(),
                dns_name: "h1.ts.net".to_string(),
                tailscale_ips: vec!["100.64.0.2".to_string()],
                online: true,
                os: "linux".to_string(),
                cur_addr: String::new(),
                relay: String::new(),
                last_seen: None,
                key_expiry: None,
                expired: false,
            }],
        };
        let event = ShimLifecycleEvent::Peers(data);
        let debug = format!("{event:?}");
        assert!(debug.contains("Peers"));
        assert!(debug.contains("h1"));
    }

    #[test]
    fn lifecycle_event_dial_failed_variant() {
        let event = ShimLifecycleEvent::DialFailed {
            request_id: "dial-123".to_string(),
            error: "connection refused".to_string(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("DialFailed"));
        assert!(debug.contains("dial-123"));
        assert!(debug.contains("connection refused"));
    }

    #[test]
    fn lifecycle_event_crashed_with_none_exit_code() {
        let event = ShimLifecycleEvent::Crashed {
            exit_code: None,
            stderr_tail: "spawn failed".to_string(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("Crashed"));
        assert!(debug.contains("None"));
        assert!(debug.contains("spawn failed"));
    }

    #[test]
    fn shim_error_display() {
        let e1 = ShimError::NotRunning;
        assert_eq!(format!("{e1}"), "shim process not running");

        let e2 = ShimError::DialCancelled;
        assert!(format!("{e2}").contains("cancelled"));

        let e3 = ShimError::DialTimeout(Duration::from_secs(30));
        assert!(format!("{e3}").contains("30"));

        let e4 = ShimError::DialFailed("connection refused".to_string());
        assert!(format!("{e4}").contains("connection refused"));

        let e5 = ShimError::SendFailed("channel closed".to_string());
        assert!(format!("{e5}").contains("channel closed"));
    }

    #[test]
    fn shim_config_defaults() {
        let config = ShimConfig {
            binary_path: PathBuf::from("/usr/local/bin/sidecar-slim"),
            hostname: "test".to_string(),
            state_dir: "/tmp/ts".to_string(),
            auth_key: Some("tskey-xxx".to_string()),
            bridge_port: 0,
            session_token: "ff".repeat(32),
            auto_restart: false,
            ephemeral: None,
            tags: None,
        };
        assert_eq!(config.hostname, "test");
        assert_eq!(config.auth_key, Some("tskey-xxx".to_string()));
        assert!(!config.auto_restart);
    }

    #[tokio::test]
    async fn pending_dials_multiple_ids_independent() {
        let dials: Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (tx_a, rx_a) = oneshot::channel();
        let (tx_b, _rx_b) = oneshot::channel();
        let (tx_c, rx_c) = oneshot::channel();

        {
            let mut map = dials.lock().await;
            map.insert("a".to_string(), tx_a);
            map.insert("b".to_string(), tx_b);
            map.insert("c".to_string(), tx_c);
            assert_eq!(map.len(), 3);
        }

        // Remove "b" (simulating crash cleanup of just one entry)
        {
            let mut map = dials.lock().await;
            map.remove("b");
            assert_eq!(map.len(), 2);
            assert!(map.contains_key("a"));
            assert!(map.contains_key("c"));
        }

        // Drop remaining senders to verify receivers detect it
        {
            let mut map = dials.lock().await;
            map.clear();
        }

        assert!(rx_a.await.is_err());
        assert!(rx_c.await.is_err());
    }

    #[test]
    fn auto_restart_paused_flag() {
        let flag = std::sync::atomic::AtomicBool::new(false);
        assert!(!flag.load(std::sync::atomic::Ordering::Relaxed));
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
    }

    // ── Phase 2: New lifecycle event variants ─────────────────────────────

    #[test]
    fn lifecycle_event_needs_approval_variant() {
        let event = ShimLifecycleEvent::NeedsApproval;
        let debug = format!("{event:?}");
        assert_eq!(debug, "NeedsApproval");
    }

    #[test]
    fn lifecycle_event_state_changed_variant() {
        let event = ShimLifecycleEvent::StateChanged {
            state: "NeedsLogin".to_string(),
            auth_url: "".to_string(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("StateChanged"));
        assert!(debug.contains("NeedsLogin"));
    }

    #[test]
    fn lifecycle_event_key_expiring_variant() {
        let event = ShimLifecycleEvent::KeyExpiring {
            expires_at: "2026-04-01T12:00:00Z".to_string(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("KeyExpiring"));
        assert!(debug.contains("2026-04-01"));
    }

    #[test]
    fn lifecycle_event_health_warning_variant() {
        let event = ShimLifecycleEvent::HealthWarning {
            warnings: vec!["DNS not working".to_string()],
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("HealthWarning"));
        assert!(debug.contains("DNS not working"));
    }

    #[test]
    fn shim_config_with_ephemeral_and_tags() {
        let config = ShimConfig {
            binary_path: PathBuf::from("/usr/bin/sidecar"),
            hostname: "ephemeral-node".to_string(),
            state_dir: "/tmp/ts".to_string(),
            auth_key: None,
            bridge_port: 0,
            session_token: "aa".repeat(32),
            auto_restart: false,
            ephemeral: Some(true),
            tags: Some(vec!["tag:truffle".to_string()]),
        };
        assert_eq!(config.ephemeral, Some(true));
        assert_eq!(config.tags.as_ref().unwrap()[0], "tag:truffle");
    }

    #[test]
    fn shim_config_with_ephemeral_and_tags_clones_correctly() {
        let config = ShimConfig {
            binary_path: PathBuf::from("/usr/bin/sidecar"),
            hostname: "ephemeral-node".to_string(),
            state_dir: "/tmp/ts".to_string(),
            auth_key: None,
            bridge_port: 8080,
            session_token: "bb".repeat(32),
            auto_restart: true,
            ephemeral: Some(true),
            tags: Some(vec!["tag:truffle".to_string(), "tag:server".to_string()]),
        };
        let cloned = config.clone();
        assert_eq!(cloned.ephemeral, Some(true));
        assert_eq!(cloned.tags.as_ref().unwrap().len(), 2);
        assert_eq!(cloned.tags.as_ref().unwrap()[0], "tag:truffle");
        assert_eq!(cloned.tags.as_ref().unwrap()[1], "tag:server");
        assert_eq!(cloned.hostname, config.hostname);
        assert_eq!(cloned.bridge_port, config.bridge_port);
        assert_eq!(cloned.auto_restart, config.auto_restart);
    }

    #[test]
    fn lifecycle_event_state_changed_with_auth_url() {
        let event = ShimLifecycleEvent::StateChanged {
            state: "NeedsLogin".to_string(),
            auth_url: "https://login.tailscale.com/a/abc123".to_string(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("StateChanged"));
        assert!(debug.contains("NeedsLogin"));
        assert!(debug.contains("https://login.tailscale.com"));
    }

    #[test]
    fn lifecycle_event_health_warning_multiple() {
        let event = ShimLifecycleEvent::HealthWarning {
            warnings: vec![
                "DNS not resolving".to_string(),
                "DERP relay unreachable".to_string(),
                "Clock skew detected".to_string(),
            ],
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("HealthWarning"));
        assert!(debug.contains("DNS not resolving"));
        assert!(debug.contains("DERP relay unreachable"));
        assert!(debug.contains("Clock skew detected"));
    }
}
