//! `truffle down` -- stop the truffle daemon.

use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;

/// Stop the daemon.
///
/// Sends a `shutdown` JSON-RPC request to the daemon, then waits for
/// the process to exit. If `force` is true, sends SIGKILL as a fallback.
pub async fn run(force: bool) -> Result<(), String> {
    let client = DaemonClient::new();

    if !client.is_daemon_running() {
        println!("Node is not running.");
        return Ok(());
    }

    // Try graceful shutdown via JSON-RPC
    match client.request(method::SHUTDOWN, serde_json::json!({})).await {
        Ok(_) => {
            println!("Shutdown signal sent.");
        }
        Err(e) => {
            if force {
                eprintln!("Warning: graceful shutdown failed ({e}), forcing...");
            } else {
                return Err(format!("Failed to send shutdown command: {e}"));
            }
        }
    }

    // Wait for the daemon to exit
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        if !client.is_daemon_running() {
            println!("Node stopped.");
            return Ok(());
        }

        if tokio::time::Instant::now() > deadline {
            if force {
                // Force kill via PID file
                let pid_path = crate::config::TruffleConfig::pid_path();
                if let Ok(Some(pid)) = crate::daemon::pid::read_pid(&pid_path) {
                    unsafe {
                        libc::kill(pid as libc::pid_t, libc::SIGKILL);
                    }
                    crate::daemon::pid::remove_pid(&pid_path)
                        .map_err(|e| format!("Failed to remove PID file: {e}"))?;
                    println!("Node force-stopped.");
                    return Ok(());
                }
            }
            return Err("Daemon did not stop in time. Use '--force' to force-kill.".to_string());
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }
}
