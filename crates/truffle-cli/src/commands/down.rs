//! `truffle down` -- stop the truffle daemon.
//!
//! Sends a graceful shutdown signal to the running daemon via JSON-RPC.
//! With `--force`, falls back to SIGKILL if graceful shutdown fails.

use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output;

/// Stop the daemon.
///
/// Sends a `shutdown` JSON-RPC request to the daemon, then waits for
/// the process to exit. If `force` is true, sends SIGKILL as a fallback.
pub async fn run(force: bool) -> Result<(), String> {
    let client = DaemonClient::new();

    if !client.is_daemon_running() {
        println!("  Node is not running.");
        return Ok(());
    }

    // Get peer count before shutdown for the notification message
    let peer_count = if let Ok(result) = client
        .request(method::PEERS, serde_json::json!({}))
        .await
    {
        result["peers"]
            .as_array()
            .map(|arr| arr.len())
            .unwrap_or(0)
    } else {
        0
    };

    // Try graceful shutdown via JSON-RPC
    match client
        .request(method::SHUTDOWN, serde_json::json!({}))
        .await
    {
        Ok(_) => {
            // Shutdown signal sent successfully
        }
        Err(e) => {
            if force {
                eprintln!(
                    "  {} graceful shutdown failed ({}), forcing...",
                    output::yellow("Warning:"),
                    e,
                );
            } else {
                return Err(format!("Failed to send shutdown command: {e}"));
            }
        }
    }

    // Wait for the daemon to exit
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        if !client.is_daemon_running() {
            println!();
            if peer_count > 0 {
                println!(
                    "  {} Notified {} peer{}",
                    output::green("\u{2713}"),
                    peer_count,
                    if peer_count == 1 { "" } else { "s" },
                );
            }
            println!("  {} Node stopped", output::green("\u{2713}"));
            println!();
            return Ok(());
        }

        if tokio::time::Instant::now() > deadline {
            if force {
                // Force kill via PID file
                let pid_path = crate::config::TruffleConfig::pid_path();
                if let Ok(Some(pid)) = crate::daemon::pid::read_pid(&pid_path) {
                    force_kill_process(pid);
                    crate::daemon::pid::remove_pid(&pid_path)
                        .map_err(|e| format!("Failed to remove PID file: {e}"))?;
                    println!();
                    println!(
                        "  {} Node force-stopped (PID {})",
                        output::yellow("\u{2713}"),
                        pid,
                    );
                    println!();
                    return Ok(());
                }
            }
            return Err(
                "Daemon did not stop in time. Use '--force' to force-kill.".to_string(),
            );
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }
}

/// Force-kill a process by PID.
///
/// - Unix: sends SIGKILL.
/// - Windows: uses `TerminateProcess`.
#[cfg(unix)]
fn force_kill_process(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn force_kill_process(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}
