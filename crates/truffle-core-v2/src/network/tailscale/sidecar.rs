//! Go sidecar process management.
//!
//! Spawns the Go sidecar binary, communicates via stdin/stdout JSON lines,
//! and manages the process lifecycle. This is Layer 1 in the architecture.
//!
//! All types and functions in this module are private to the `tailscale` module.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, Mutex, Notify};

use super::protocol::*;
use crate::network::NetworkError;

/// Configuration for spawning the Go sidecar.
#[derive(Debug, Clone)]
pub(crate) struct SidecarConfig {
    /// Path to the Go sidecar binary.
    pub binary_path: PathBuf,
    /// Hostname for the tsnet node.
    pub hostname: String,
    /// State directory for tsnet.
    pub state_dir: String,
    /// Optional Tailscale auth key.
    pub auth_key: Option<String>,
    /// Bridge port that Rust is listening on.
    pub bridge_port: u16,
    /// Session token as hex string (64 hex chars = 32 bytes).
    pub session_token_hex: String,
    /// Whether the node is ephemeral.
    pub ephemeral: Option<bool>,
    /// ACL tags to advertise.
    pub tags: Option<Vec<String>>,
}

/// Internal events from the sidecar event processing loop.
#[derive(Debug, Clone)]
pub(crate) enum SidecarInternalEvent {
    /// Sidecar reached "running" state.
    Started {
        hostname: String,
        dns_name: String,
        tailscale_ip: String,
    },
    /// Sidecar stopped.
    Stopped,
    /// Auth required.
    AuthRequired { auth_url: String },
    /// Needs admin approval.
    NeedsApproval,
    /// State changed.
    StateChange { state: String },
    /// Key expiring.
    KeyExpiring { expires_at: String },
    /// Health warnings.
    HealthWarning { warnings: Vec<String> },
    /// Peer list received (from getPeers).
    PeersReceived(Vec<SidecarPeer>),
    /// A single peer changed (from WatchIPNBus).
    PeerChanged(PeerChangedEventData),
    /// Dial result (success — the bridge connection will arrive separately).
    #[allow(dead_code)]
    DialSucceeded { request_id: String },
    /// Dial result (failure — no bridge connection coming).
    DialFailed { request_id: String, error: String },
    /// Listening on a port succeeded.
    Listening { port: u16 },
    /// Unlistened from a port.
    #[allow(dead_code)]
    Unlistened { port: u16 },
    /// UDP listening on a port succeeded. `local_port` is the localhost relay port.
    ListeningPacket { port: u16, local_port: u16 },
    /// Ping result.
    PingResult(PingResultEventData),
    /// Error from sidecar.
    Error { code: String, message: String },
    /// Sidecar process exited unexpectedly.
    ProcessExited { exit_code: Option<i32> },
}

/// Manages the Go sidecar child process.
///
/// Handles spawning, command sending via stdin, event reading from stdout,
/// and provides a broadcast channel for internal event distribution.
pub(crate) struct GoSidecar {
    /// Channel for sending commands to the sidecar's stdin writer task.
    stdin_tx: mpsc::Sender<String>,
    /// Internal events broadcast from the stdout reader task.
    event_tx: broadcast::Sender<SidecarInternalEvent>,
    /// Signal to shut down the sidecar.
    shutdown: Arc<Notify>,
    /// Handle to the child process (for kill on drop).
    child: Arc<Mutex<Option<Child>>>,
}

impl GoSidecar {
    /// Spawn a new Go sidecar process.
    ///
    /// Returns the sidecar handle and a broadcast receiver for internal events.
    pub async fn spawn(
        config: SidecarConfig,
    ) -> Result<(Self, broadcast::Receiver<SidecarInternalEvent>), NetworkError> {
        let (event_tx, event_rx) = broadcast::channel(256);
        let (stdin_tx, stdin_rx) = mpsc::channel::<String>(64);
        let shutdown = Arc::new(Notify::new());

        // Spawn the child process
        let child = Self::spawn_child(&config)?;
        let child = Arc::new(Mutex::new(Some(child)));

        let sidecar = GoSidecar {
            stdin_tx,
            event_tx: event_tx.clone(),
            shutdown: shutdown.clone(),
            child: child.clone(),
        };

        // Take stdout/stdin from child before spawning tasks
        {
            let mut guard = child.lock().await;
            let child_proc = guard.as_mut().ok_or_else(|| {
                NetworkError::SidecarError("child process not available".into())
            })?;

            let stdout = child_proc.stdout.take().ok_or_else(|| {
                NetworkError::SidecarError("failed to capture stdout".into())
            })?;
            let stdin = child_proc.stdin.take().ok_or_else(|| {
                NetworkError::SidecarError("failed to capture stdin".into())
            })?;

            // Spawn stdin writer task
            Self::spawn_stdin_writer(stdin, stdin_rx, shutdown.clone());

            // Spawn stdout reader task
            Self::spawn_stdout_reader(stdout, event_tx.clone(), shutdown.clone());
        }

        // Spawn process watcher task
        Self::spawn_process_watcher(child.clone(), event_tx, shutdown);

        Ok((sidecar, event_rx))
    }

    fn spawn_child(config: &SidecarConfig) -> Result<Child, NetworkError> {
        Command::new(&config.binary_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| NetworkError::SidecarError(format!("failed to spawn: {e}")))
    }

    fn spawn_stdin_writer(
        mut stdin: tokio::process::ChildStdin,
        mut rx: mpsc::Receiver<String>,
        shutdown: Arc<Notify>,
    ) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.notified() => {
                        tracing::debug!("sidecar stdin writer shutting down");
                        break;
                    }
                    msg = rx.recv() => {
                        match msg {
                            Some(line) => {
                                if let Err(e) = stdin.write_all(line.as_bytes()).await {
                                    tracing::error!("failed to write to sidecar stdin: {e}");
                                    break;
                                }
                                if let Err(e) = stdin.write_all(b"\n").await {
                                    tracing::error!("failed to write newline to sidecar stdin: {e}");
                                    break;
                                }
                                if let Err(e) = stdin.flush().await {
                                    tracing::error!("failed to flush sidecar stdin: {e}");
                                    break;
                                }
                            }
                            None => {
                                tracing::debug!("sidecar stdin channel closed");
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    fn spawn_stdout_reader(
        stdout: tokio::process::ChildStdout,
        event_tx: broadcast::Sender<SidecarInternalEvent>,
        shutdown: Arc<Notify>,
    ) {
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            loop {
                tokio::select! {
                    _ = shutdown.notified() => {
                        tracing::debug!("sidecar stdout reader shutting down");
                        break;
                    }
                    result = lines.next_line() => {
                        match result {
                            Ok(Some(line)) => {
                                if line.trim().is_empty() {
                                    continue;
                                }
                                match serde_json::from_str::<SidecarEvent>(&line) {
                                    Ok(event) => {
                                        if let Some(internal) = Self::map_event(event) {
                                            let _ = event_tx.send(internal);
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("failed to parse sidecar event: {e}, line: {line}");
                                    }
                                }
                            }
                            Ok(None) => {
                                tracing::info!("sidecar stdout closed (process exited)");
                                break;
                            }
                            Err(e) => {
                                tracing::error!("sidecar stdout read error: {e}");
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    fn spawn_process_watcher(
        child: Arc<Mutex<Option<Child>>>,
        event_tx: broadcast::Sender<SidecarInternalEvent>,
        shutdown: Arc<Notify>,
    ) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.notified() => {
                        tracing::debug!("process watcher shutting down");
                        return;
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                        let mut guard = child.lock().await;
                        if let Some(ref mut c) = *guard {
                            match c.try_wait() {
                                Ok(Some(status)) => {
                                    let exit_code = status.code();
                                    tracing::warn!("sidecar process exited with code: {exit_code:?}");
                                    let _ = event_tx.send(SidecarInternalEvent::ProcessExited { exit_code });
                                    *guard = None;
                                    return;
                                }
                                Ok(None) => {
                                    // Still running, continue polling
                                }
                                Err(e) => {
                                    tracing::error!("failed to check sidecar process status: {e}");
                                    return;
                                }
                            }
                        } else {
                            return;
                        }
                    }
                }
            }
        });
    }

    fn map_event(event: SidecarEvent) -> Option<SidecarInternalEvent> {
        match event.event.as_str() {
            event_type::STATUS | event_type::STARTED => {
                if let Ok(data) = serde_json::from_value::<StatusEventData>(event.data) {
                    if data.state == "running" {
                        Some(SidecarInternalEvent::Started {
                            hostname: data.hostname,
                            dns_name: data.dns_name,
                            tailscale_ip: data.tailscale_ip,
                        })
                    } else if data.state == "error" {
                        Some(SidecarInternalEvent::Error {
                            code: "STATUS_ERROR".to_string(),
                            message: data.error,
                        })
                    } else {
                        // Other status updates (starting, etc.)
                        Some(SidecarInternalEvent::StateChange { state: data.state })
                    }
                } else {
                    None
                }
            }
            event_type::STOPPED => Some(SidecarInternalEvent::Stopped),
            event_type::AUTH_REQUIRED => {
                serde_json::from_value::<AuthRequiredEventData>(event.data)
                    .ok()
                    .map(|d| SidecarInternalEvent::AuthRequired {
                        auth_url: d.auth_url,
                    })
            }
            event_type::NEEDS_APPROVAL => Some(SidecarInternalEvent::NeedsApproval),
            event_type::STATE_CHANGE => {
                serde_json::from_value::<StateChangeEventData>(event.data)
                    .ok()
                    .map(|d| SidecarInternalEvent::StateChange { state: d.state })
            }
            event_type::KEY_EXPIRING => {
                serde_json::from_value::<KeyExpiringEventData>(event.data)
                    .ok()
                    .map(|d| SidecarInternalEvent::KeyExpiring {
                        expires_at: d.expires_at,
                    })
            }
            event_type::HEALTH_WARNING => {
                serde_json::from_value::<HealthWarningEventData>(event.data)
                    .ok()
                    .map(|d| SidecarInternalEvent::HealthWarning {
                        warnings: d.warnings,
                    })
            }
            event_type::PEERS => {
                serde_json::from_value::<PeersEventData>(event.data)
                    .ok()
                    .map(|d| SidecarInternalEvent::PeersReceived(d.peers))
            }
            event_type::PEER_CHANGED => {
                serde_json::from_value::<PeerChangedEventData>(event.data)
                    .ok()
                    .map(SidecarInternalEvent::PeerChanged)
            }
            event_type::DIAL_RESULT => {
                serde_json::from_value::<DialResultEventData>(event.data).ok().map(|d| {
                    if d.success {
                        SidecarInternalEvent::DialSucceeded {
                            request_id: d.request_id,
                        }
                    } else {
                        SidecarInternalEvent::DialFailed {
                            request_id: d.request_id,
                            error: d.error,
                        }
                    }
                })
            }
            event_type::LISTENING => {
                serde_json::from_value::<ListeningEventData>(event.data)
                    .ok()
                    .map(|d| SidecarInternalEvent::Listening { port: d.port })
            }
            event_type::UNLISTENED => {
                serde_json::from_value::<UnlistenedEventData>(event.data)
                    .ok()
                    .map(|d| SidecarInternalEvent::Unlistened { port: d.port })
            }
            event_type::LISTENING_PACKET => {
                serde_json::from_value::<ListeningPacketEventData>(event.data)
                    .ok()
                    .map(|d| SidecarInternalEvent::ListeningPacket {
                        port: d.port,
                        local_port: d.local_port,
                    })
            }
            event_type::PING_RESULT => {
                serde_json::from_value::<PingResultEventData>(event.data)
                    .ok()
                    .map(SidecarInternalEvent::PingResult)
            }
            event_type::ERROR => {
                serde_json::from_value::<ErrorEventData>(event.data)
                    .ok()
                    .map(|d| SidecarInternalEvent::Error {
                        code: d.code,
                        message: d.message,
                    })
            }
            other => {
                tracing::debug!("unhandled sidecar event type: {other}");
                None
            }
        }
    }

    /// Send a JSON command to the sidecar.
    pub async fn send_command(&self, cmd: SidecarCommand) -> Result<(), NetworkError> {
        let json = serde_json::to_string(&cmd)?;
        self.stdin_tx
            .send(json)
            .await
            .map_err(|e| NetworkError::SidecarError(format!("stdin channel closed: {e}")))
    }

    /// Send the tsnet:start command.
    pub async fn send_start(&self, config: &SidecarConfig) -> Result<(), NetworkError> {
        let data = StartCommandData {
            hostname: config.hostname.clone(),
            state_dir: config.state_dir.clone(),
            auth_key: config.auth_key.clone(),
            bridge_port: config.bridge_port,
            session_token: config.session_token_hex.clone(),
            ephemeral: config.ephemeral,
            tags: config.tags.clone(),
        };
        self.send_command(SidecarCommand {
            command: command_type::START,
            data: Some(serde_json::to_value(&data)?),
        })
        .await
    }

    /// Send the tsnet:stop command.
    pub async fn send_stop(&self) -> Result<(), NetworkError> {
        self.send_command(SidecarCommand {
            command: command_type::STOP,
            data: None,
        })
        .await
    }

    /// Send the tsnet:getPeers command.
    pub async fn send_get_peers(&self) -> Result<(), NetworkError> {
        self.send_command(SidecarCommand {
            command: command_type::GET_PEERS,
            data: None,
        })
        .await
    }

    /// Send the bridge:dial command.
    pub async fn send_dial(
        &self,
        request_id: String,
        target: String,
        port: u16,
    ) -> Result<(), NetworkError> {
        let data = DialCommandData {
            request_id,
            target,
            port,
        };
        self.send_command(SidecarCommand {
            command: command_type::DIAL,
            data: Some(serde_json::to_value(&data)?),
        })
        .await
    }

    /// Send the tsnet:listen command.
    pub async fn send_listen(&self, port: u16, tls: Option<bool>) -> Result<(), NetworkError> {
        let data = ListenCommandData { port, tls };
        self.send_command(SidecarCommand {
            command: command_type::LISTEN,
            data: Some(serde_json::to_value(&data)?),
        })
        .await
    }

    /// Send the tsnet:unlisten command.
    pub async fn send_unlisten(&self, port: u16) -> Result<(), NetworkError> {
        let data = UnlistenCommandData { port };
        self.send_command(SidecarCommand {
            command: command_type::UNLISTEN,
            data: Some(serde_json::to_value(&data)?),
        })
        .await
    }

    /// Send the tsnet:ping command.
    pub async fn send_ping(
        &self,
        target: String,
        ping_type: Option<String>,
    ) -> Result<(), NetworkError> {
        let data = PingCommandData { target, ping_type };
        self.send_command(SidecarCommand {
            command: command_type::PING,
            data: Some(serde_json::to_value(&data)?),
        })
        .await
    }

    /// Send the tsnet:listenPacket command to bind a UDP socket via tsnet.
    pub async fn send_listen_packet(&self, port: u16) -> Result<(), NetworkError> {
        let data = ListenPacketCommandData { port };
        self.send_command(SidecarCommand {
            command: command_type::LISTEN_PACKET,
            data: Some(serde_json::to_value(&data)?),
        })
        .await
    }

    /// Send the tsnet:watchPeers command to start WatchIPNBus-based peer events.
    pub async fn send_watch_peers(&self) -> Result<(), NetworkError> {
        let data = WatchPeersCommandData { include_all: None };
        self.send_command(SidecarCommand {
            command: command_type::WATCH_PEERS,
            data: Some(serde_json::to_value(&data)?),
        })
        .await
    }

    /// Subscribe to sidecar internal events.
    pub fn subscribe(&self) -> broadcast::Receiver<SidecarInternalEvent> {
        self.event_tx.subscribe()
    }

    /// Shut down the sidecar process.
    pub async fn shutdown(&self) {
        // Try to send stop command gracefully
        let _ = self.send_stop().await;

        // Signal all tasks to stop
        self.shutdown.notify_waiters();

        // Give the process a moment to exit gracefully
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Force kill if still running
        let mut guard = self.child.lock().await;
        if let Some(ref mut child) = *guard {
            tracing::info!("force-killing sidecar process");
            let _ = child.kill().await;
            *guard = None;
        }
    }
}

impl Drop for GoSidecar {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
    }
}
