//! `truffle up` -- start the truffle daemon.
//!
//! If `foreground` is true, runs the daemon inline with a live status dashboard.
//! Otherwise, forks a background daemon process and shows a brief status summary.

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::daemon::server::DaemonServer;
use crate::output;

/// Start the daemon.
///
/// If `foreground` is true, run the daemon in the current process (blocking)
/// with a live status dashboard. Otherwise, fork a background process and
/// return immediately after showing a brief status.
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
        // Try to get current status for a helpful message
        if let Ok(result) = client
            .request(method::STATUS, serde_json::json!({}))
            .await
        {
            let name = result["name"].as_str().unwrap_or("-");
            let uptime = result["uptime_secs"]
                .as_u64()
                .map(output::format_uptime)
                .unwrap_or_else(|| "-".to_string());
            println!(
                "Already running as {} (uptime: {}). Use 'truffle down' first, or 'truffle status' to check.",
                output::bold(name),
                uptime,
            );
        } else {
            println!("Daemon is already running. Use 'truffle down' first, or 'truffle status' to check.");
        }
        return Ok(());
    }

    if foreground {
        // Run in foreground (blocking) with dashboard output
        println!();
        println!("  {}", output::bold("truffle"));
        println!(
            "  {}",
            output::dim(&"\u{2500}".repeat(39))
        );
        println!();
        println!(
            "  {:<12}{}",
            "Node",
            output::bold(&config.node.name)
        );
        println!(
            "  {:<12}{} {}",
            "Status",
            output::status_indicator("connecting"),
            output::status_label("connecting"),
        );
        println!();
        println!(
            "  Starting daemon in foreground (PID {})...",
            std::process::id()
        );

        let server = DaemonServer::start(&config).await?;

        // Clear and reprint with actual status
        println!();
        println!(
            "  {:<12}{}",
            "Socket",
            output::dim(&TruffleConfig::socket_path().display().to_string()),
        );
        println!(
            "  {:<12}{} {}",
            "Status",
            output::status_indicator("online"),
            output::status_label("online"),
        );
        println!();
        println!(
            "  Listening for connections...",
        );
        println!(
            "  {}",
            output::dim("Press Ctrl+C to stop, or run 'truffle down' from another terminal."),
        );
        println!();

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

        // Get status from the newly started daemon
        println!();
        println!("  {}", output::bold("truffle"));
        println!(
            "  {}",
            output::dim(&"\u{2500}".repeat(39))
        );
        println!();

        if let Ok(result) = client
            .request(method::STATUS, serde_json::json!({}))
            .await
        {
            let name = result["name"].as_str().unwrap_or(&config.node.name);
            let status = result["status"].as_str().unwrap_or("Online");

            println!(
                "  {:<12}{}",
                "Node",
                output::bold(name)
            );
            println!(
                "  {:<12}{} {}",
                "Status",
                output::status_indicator(status),
                output::status_label(status),
            );

            if let Some(ip) = result["tailscale_ip"].as_str() {
                println!("  {:<12}{}", "IP", ip);
            }
            if let Some(dns) = result["tailscale_dns_name"].as_str() {
                println!("  {:<12}{}", "DNS", output::dim(dns));
            }
        } else {
            println!(
                "  {:<12}{}",
                "Node",
                output::bold(&config.node.name)
            );
            println!(
                "  {:<12}{} {}",
                "Status",
                output::status_indicator("online"),
                output::status_label("online"),
            );
        }

        println!();
    }

    Ok(())
}
