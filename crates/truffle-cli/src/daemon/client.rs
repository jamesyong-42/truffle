//! Client connector: connect to the daemon's IPC endpoint and send requests.
//!
//! CLI commands use `DaemonClient` to communicate with the running daemon.
//! If no daemon is running and `auto_up` is enabled, the client will
//! automatically start one.
//!
//! Uses Unix sockets on macOS/Linux and named pipes on Windows.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::debug;

use super::ipc;
use super::pid;
use super::protocol::{DaemonRequest, DaemonResponse};
use crate::config::TruffleConfig;

/// Client for communicating with the truffle daemon.
pub struct DaemonClient {
    /// Path to the daemon's IPC endpoint (Unix socket or named pipe).
    socket_path: PathBuf,
    /// Path to the daemon's PID file.
    pid_path: PathBuf,
    /// Auto-incrementing request ID counter.
    next_id: AtomicU64,
}

/// Errors that can occur when using the client.
#[derive(Debug)]
pub enum ClientError {
    /// The daemon is not running.
    DaemonNotRunning,
    /// Failed to connect to the IPC endpoint.
    ConnectionFailed(String),
    /// Failed to send or receive data.
    IoError(String),
    /// The daemon returned an error response.
    DaemonError {
        code: i32,
        message: String,
    },
    /// Failed to parse the daemon's response.
    ParseError(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::DaemonNotRunning => write!(f, "Daemon is not running. Start it with 'truffle up'."),
            ClientError::ConnectionFailed(msg) => write!(f, "Failed to connect to daemon: {msg}"),
            ClientError::IoError(msg) => write!(f, "I/O error: {msg}"),
            ClientError::DaemonError { code, message } => write!(f, "Daemon error ({code}): {message}"),
            ClientError::ParseError(msg) => write!(f, "Failed to parse daemon response: {msg}"),
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

    /// Create a new client with a custom socket path (for testing).
    pub fn with_socket_path(socket_path: PathBuf) -> Self {
        let pid_path = socket_path
            .parent()
            .map(|p| p.join("truffle.pid"))
            .unwrap_or_else(|| PathBuf::from("truffle.pid"));
        Self {
            socket_path,
            pid_path,
            next_id: AtomicU64::new(1),
        }
    }

    /// Check if the daemon is currently running.
    ///
    /// Checks the PID file and verifies the process is alive.
    pub fn is_daemon_running(&self) -> bool {
        match pid::read_pid(&self.pid_path) {
            Ok(Some(p)) => pid::is_process_running(p),
            _ => false,
        }
    }

    /// Connect to the daemon's IPC endpoint.
    ///
    /// Returns an `IpcStream` for sending/receiving data.
    pub async fn connect(&self) -> Result<ipc::IpcStream, ClientError> {
        if !self.is_daemon_running() {
            return Err(ClientError::DaemonNotRunning);
        }

        ipc::IpcStream::connect(&self.socket_path)
            .await
            .map_err(|e| ClientError::ConnectionFailed(e.to_string()))
    }

    /// Send a JSON-RPC request and receive the response.
    ///
    /// Opens a connection, sends the request, reads one response line, and
    /// returns the result value (or an error).
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

        // Read response
        let response_line = reader
            .next_line()
            .await
            .map_err(|e| ClientError::IoError(e.to_string()))?
            .ok_or_else(|| ClientError::IoError("Daemon closed connection without responding".into()))?;

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

    /// Ensure the daemon is running, starting it if necessary.
    ///
    /// If `auto_up` is true in the config and the daemon is not running,
    /// this will fork a background daemon process.
    ///
    /// Returns `Ok(())` if the daemon is running (or was just started).
    pub async fn ensure_running(&self, config: &TruffleConfig) -> Result<(), ClientError> {
        if self.is_daemon_running() {
            return Ok(());
        }

        if !config.node.auto_up {
            return Err(ClientError::DaemonNotRunning);
        }

        // Fork a background daemon process
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

        // Detach the child process so it continues running
        drop(child);

        // Wait for daemon to be ready (poll the IPC endpoint).
        // On Windows, named pipe paths don't respond to Path::exists(),
        // so we skip the exists() check and try connecting directly.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(ClientError::IoError("Timed out waiting for daemon to start".into()));
            }

            if self.is_daemon_running() {
                // Try to connect to the IPC endpoint
                if ipc::IpcStream::connect(&self.socket_path).await.is_ok() {
                    return Ok(());
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_daemon_client_connect_no_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let client = DaemonClient::with_socket_path(socket_path);

        // Should report daemon not running
        assert!(!client.is_daemon_running());

        // Should fail to connect
        let result = client.connect().await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ClientError::DaemonNotRunning => {} // expected
            other => panic!("Expected DaemonNotRunning, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_daemon_client_request_no_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let client = DaemonClient::with_socket_path(socket_path);

        let result = client.request("status", serde_json::json!({})).await;
        assert!(result.is_err());
    }
}
