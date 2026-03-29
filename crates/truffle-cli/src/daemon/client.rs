//! Client connector: connect to the daemon's IPC endpoint and send requests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::debug;

use super::ipc;
use super::pid;
use super::protocol::{DaemonNotification, DaemonRequest, DaemonResponse};
use crate::config::TruffleConfig;

/// Client for communicating with the truffle daemon.
pub struct DaemonClient {
    socket_path: PathBuf,
    pid_path: PathBuf,
    next_id: AtomicU64,
}

/// Errors that can occur when using the client.
#[derive(Debug)]
pub enum ClientError {
    DaemonNotRunning,
    ConnectionFailed(String),
    IoError(String),
    DaemonError { code: i32, message: String },
    ParseError(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::DaemonNotRunning => {
                write!(f, "Daemon is not running. Start it with 'truffle up'.")
            }
            ClientError::ConnectionFailed(msg) => {
                write!(f, "Failed to connect to daemon: {msg}")
            }
            ClientError::IoError(msg) => write!(f, "I/O error: {msg}"),
            ClientError::DaemonError { code, message } => {
                write!(f, "Daemon error ({code}): {message}")
            }
            ClientError::ParseError(msg) => {
                write!(f, "Failed to parse daemon response: {msg}")
            }
        }
    }
}

impl std::error::Error for ClientError {}

impl DaemonClient {
    /// Create a new client that connects to the default socket path.
    pub fn new() -> Self {
        Self {
            socket_path: TruffleConfig::socket_path(),
            pid_path: TruffleConfig::pid_path(),
            next_id: AtomicU64::new(1),
        }
    }

    /// Check if the daemon is currently running.
    pub fn is_daemon_running(&self) -> bool {
        match pid::read_pid(&self.pid_path) {
            Ok(Some(p)) => pid::is_process_running(p),
            _ => false,
        }
    }

    /// Connect to the daemon's IPC endpoint.
    pub async fn connect(&self) -> Result<ipc::IpcStream, ClientError> {
        if !self.is_daemon_running() {
            return Err(ClientError::DaemonNotRunning);
        }

        ipc::IpcStream::connect(&self.socket_path)
            .await
            .map_err(|e| ClientError::ConnectionFailed(e.to_string()))
    }

    /// Send a JSON-RPC request and receive the response.
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let stream = self.connect().await?;
        let (mut reader, mut writer) = stream.into_split();

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = DaemonRequest::new(id, method, params);

        let request_json =
            serde_json::to_string(&request).map_err(|e| ClientError::IoError(e.to_string()))?;

        debug!(method = method, id = id, "Sending request to daemon");

        writer
            .write_line(&request_json)
            .await
            .map_err(|e| ClientError::IoError(e.to_string()))?;

        let response_line = reader
            .next_line()
            .await
            .map_err(|e| ClientError::IoError(e.to_string()))?
            .ok_or_else(|| {
                ClientError::IoError("Daemon closed connection without responding".into())
            })?;

        let response: DaemonResponse = serde_json::from_str(&response_line)
            .map_err(|e| ClientError::ParseError(e.to_string()))?;

        if let Some(err) = response.error {
            return Err(ClientError::DaemonError {
                code: err.code,
                message: err.message,
            });
        }

        Ok(response.result.unwrap_or(serde_json::Value::Null))
    }

    /// Send a request with notification streaming (for file transfers, etc.).
    pub async fn request_with_notifications(
        &self,
        method: &str,
        params: serde_json::Value,
        on_notification: impl Fn(&DaemonNotification),
    ) -> Result<serde_json::Value, ClientError> {
        let stream = self.connect().await?;
        let (mut reader, mut writer) = stream.into_split();

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = DaemonRequest::new(id, method, params);

        let request_json =
            serde_json::to_string(&request).map_err(|e| ClientError::IoError(e.to_string()))?;

        debug!(method = method, id = id, "Sending request to daemon (with notifications)");

        writer
            .write_line(&request_json)
            .await
            .map_err(|e| ClientError::IoError(e.to_string()))?;

        loop {
            let line = reader
                .next_line()
                .await
                .map_err(|e| ClientError::IoError(e.to_string()))?
                .ok_or_else(|| {
                    ClientError::IoError("Daemon closed connection without responding".into())
                })?;

            let raw: serde_json::Value = serde_json::from_str(&line)
                .map_err(|e| ClientError::ParseError(e.to_string()))?;

            if raw.get("id").is_some() {
                let response: DaemonResponse = serde_json::from_value(raw)
                    .map_err(|e| ClientError::ParseError(e.to_string()))?;

                if let Some(err) = response.error {
                    return Err(ClientError::DaemonError {
                        code: err.code,
                        message: err.message,
                    });
                }

                return Ok(response.result.unwrap_or(serde_json::Value::Null));
            }

            if let Ok(notif) = serde_json::from_value::<DaemonNotification>(raw) {
                on_notification(&notif);
            }
        }
    }

    /// Send a subscribe request and stream notifications indefinitely.
    ///
    /// The callback is called for each notification. If it returns `true`, the
    /// subscription ends and the function returns `Ok(())`. This allows
    /// commands like `wait` and `recv` to exit after the first matching event.
    ///
    /// If `timeout` is `Some`, the subscription will end after the given
    /// duration with `Err(ClientError::DaemonError { code: 3, .. })`.
    pub async fn subscribe(
        &self,
        params: serde_json::Value,
        timeout: Option<std::time::Duration>,
        mut on_notification: impl FnMut(&DaemonNotification) -> bool,
    ) -> Result<(), ClientError> {
        let stream = self.connect().await?;
        let (mut reader, mut writer) = stream.into_split();

        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let request = DaemonRequest::new(id, super::protocol::method::SUBSCRIBE, params);

        let request_json =
            serde_json::to_string(&request).map_err(|e| ClientError::IoError(e.to_string()))?;

        debug!(id = id, "Sending subscribe request to daemon");

        writer
            .write_line(&request_json)
            .await
            .map_err(|e| ClientError::IoError(e.to_string()))?;

        let deadline = timeout.map(|d| tokio::time::Instant::now() + d);

        loop {
            let read_future = reader.next_line();

            let line = if let Some(dl) = deadline {
                match tokio::time::timeout_at(dl, read_future).await {
                    Ok(result) => result
                        .map_err(|e| ClientError::IoError(e.to_string()))?
                        .ok_or_else(|| {
                            ClientError::IoError("Daemon closed connection".into())
                        })?,
                    Err(_) => {
                        return Err(ClientError::DaemonError {
                            code: super::protocol::error_code::TIMEOUT,
                            message: "Subscription timed out".into(),
                        });
                    }
                }
            } else {
                read_future
                    .await
                    .map_err(|e| ClientError::IoError(e.to_string()))?
                    .ok_or_else(|| {
                        ClientError::IoError("Daemon closed connection".into())
                    })?
            };

            let raw: serde_json::Value = serde_json::from_str(&line)
                .map_err(|e| ClientError::ParseError(e.to_string()))?;

            // If it has an "id" field, it's an error response (subscribe shouldn't
            // normally produce a response, but errors are possible).
            if raw.get("id").is_some() {
                let response: DaemonResponse = serde_json::from_value(raw)
                    .map_err(|e| ClientError::ParseError(e.to_string()))?;

                if let Some(err) = response.error {
                    return Err(ClientError::DaemonError {
                        code: err.code,
                        message: err.message,
                    });
                }

                // Unexpected response — ignore and continue
                continue;
            }

            if let Ok(notif) = serde_json::from_value::<DaemonNotification>(raw) {
                if on_notification(&notif) {
                    return Ok(());
                }
            }
        }
    }

    /// Ensure the daemon is running, starting it if necessary.
    pub async fn ensure_running(&self, config: &TruffleConfig) -> Result<(), ClientError> {
        if self.is_daemon_running() {
            return Ok(());
        }

        if !config.node.auto_up {
            return Err(ClientError::DaemonNotRunning);
        }

        let exe = std::env::current_exe()
            .map_err(|e| ClientError::IoError(format!("Failed to get current exe: {e}")))?;

        let child = tokio::process::Command::new(&exe)
            .arg("up")
            .arg("--foreground")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| ClientError::IoError(format!("Failed to spawn daemon: {e}")))?;

        drop(child);

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(ClientError::IoError(
                    "Timed out waiting for daemon to start".into(),
                ));
            }

            if self.is_daemon_running() {
                if ipc::IpcStream::connect(&self.socket_path).await.is_ok() {
                    return Ok(());
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
    }
}
