//! `truffle up` -- start the truffle daemon.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::server::DaemonServer;

/// Start the daemon.
///
/// If `foreground` is true, run the daemon in the current process (blocking).
/// Otherwise, fork a background process and return immediately.
pub async fn run(
    config: &TruffleConfig,
    name: Option<&str>,
    foreground: bool,
) -> Result<(), String> {
    // Apply CLI overrides to config
    let mut config = config.clone();
    if let Some(name) = name {
        config.node.name = name.to_string();
    }

    // Check if daemon is already running
    let client = DaemonClient::new();
    if client.is_daemon_running() {
        println!("Daemon is already running. Use 'truffle down' first, or 'truffle status' to check.");
        return Ok(());
    }

    if foreground {
        // Run in foreground (blocking)
        println!("Starting daemon in foreground...");
        let server = DaemonServer::start(&config).await?;

        println!("node is up");
        println!("  name:       {}", config.node.name);
        println!("  socket:     {}", TruffleConfig::socket_path().display());
        println!("  pid:        {}", std::process::id());
        println!();
        println!("Press Ctrl+C to stop.");

        server.run().await?;
    } else {
        // Fork a background daemon process
        let exe = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable: {e}"))?;

        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("up").arg("--foreground");

        if let Some(name) = name {
            cmd.arg("--name").arg(name);
        }

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // On Unix, use setsid to fully detach the child
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }

        cmd.spawn()
            .map_err(|e| format!("Failed to start background daemon: {e}"))?;

        // Wait for daemon to be ready
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
        loop {
            if tokio::time::Instant::now() > deadline {
                return Err("Timed out waiting for daemon to start".to_string());
            }

            if client.is_daemon_running() {
                // Try to connect to the socket
                if client.connect().await.is_ok() {
                    break;
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }

        println!("node is up");
        println!("  name:       {}", config.node.name);
        println!("  socket:     {}", TruffleConfig::socket_path().display());
    }

    Ok(())
}
