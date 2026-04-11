//! `truffle up` -- start the truffle daemon.

use crate::apps::file_transfer::receive;
use crate::auto_update;
use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::daemon::server::DaemonServer;
use crate::exit_codes;
use crate::json_output;
use crate::output;

/// Start the daemon.
pub async fn run(
    config: &TruffleConfig,
    device_name: Option<&str>,
    app_id: Option<&str>,
    foreground: bool,
    json: bool,
) -> Result<(), (i32, String)> {
    let mut config = config.clone();
    if let Some(name) = device_name {
        config.node.device_name = name.to_string();
    }
    if let Some(id) = app_id {
        config.node.app_id = id.to_string();
    }

    let client = DaemonClient::new();
    if client.is_daemon_running() {
        if json {
            let mut map = json_output::envelope(&config.node.device_name);
            map.insert("status".to_string(), serde_json::json!("already_running"));
            if let Ok(result) = client.request(method::STATUS, serde_json::json!({})).await {
                if let Some(pid) = result["pid"].as_u64() {
                    map.insert("pid".to_string(), serde_json::json!(pid));
                }
            }
            json_output::print_json(&serde_json::Value::Object(map));
        } else if let Ok(result) = client.request(method::STATUS, serde_json::json!({})).await {
            let name = result["device_name"]
                .as_str()
                .or_else(|| result["name"].as_str())
                .unwrap_or("-");
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
            println!("Daemon is already running. Use 'truffle down' first.");
        }
        return Ok(());
    }

    if foreground {
        run_foreground(&config).await
    } else {
        run_background(&config, device_name, app_id, json).await
    }
}

async fn run_foreground(config: &TruffleConfig) -> Result<(), (i32, String)> {
    println!();
    println!("  {}", output::bold("truffle v2"));
    println!("  {}", output::dim(&"\u{2500}".repeat(39)));
    println!();
    println!("  {:<12}{}", "Node", output::bold(&config.node.device_name));
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

    let server = DaemonServer::start(config)
        .await
        .map_err(|e| (exit_codes::ERROR, e))?;

    // Spawn the file transfer receive handler
    let node = server.node().clone();
    let output_dir = dirs::download_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("truffle")
        .to_string_lossy()
        .to_string();
    let _ft_handle = receive::spawn_receive_handler(node.clone(), output_dir);

    // Show node info once available
    let info = node.local_info();
    let ip_str = info.ip.map(|ip| ip.to_string()).unwrap_or_default();

    println!();
    println!("  {}", output::dim(&"\u{2500}".repeat(39)));
    println!();
    println!("  {:<12}{}", "Node", output::bold(&info.device_name));
    println!(
        "  {:<12}{} {}",
        "Status",
        output::status_indicator("online"),
        output::status_label("online"),
    );
    if !ip_str.is_empty() {
        println!("  {:<12}{}", "IP", ip_str);
    }
    if let Some(ref dns) = info.dns_name {
        if !dns.is_empty() {
            println!("  {:<12}{}", "DNS", output::dim(dns));
        }
    }
    println!(
        "  {:<12}{}",
        "Socket",
        output::dim(&TruffleConfig::socket_path().display().to_string()),
    );
    println!();
    println!("  Listening for connections...");
    println!(
        "  {}",
        output::dim("Press Ctrl+C to stop, or run 'truffle down' from another terminal."),
    );
    println!();

    // Subscribe to peer events for live display
    let mut peer_rx = server.subscribe_peer_events();
    tokio::spawn(async move {
        loop {
            match peer_rx.recv().await {
                Ok(event) => {
                    use truffle_core::session::PeerEvent;
                    match event {
                        PeerEvent::Joined(state) => {
                            let display_name = state
                                .identity
                                .as_ref()
                                .map(|i| i.device_name.clone())
                                .unwrap_or_else(|| state.name.clone());
                            println!(
                                "  {} peer discovered: {} ({})",
                                output::status_indicator("online"),
                                output::bold(&display_name),
                                state.ip,
                            );
                        }
                        PeerEvent::Left(id) => {
                            println!(
                                "  {} peer offline: {}",
                                output::status_indicator("offline"),
                                id,
                            );
                        }
                        PeerEvent::WsConnected(id) => {
                            println!(
                                "  {} peer ws connected: {}",
                                output::status_indicator("online"),
                                id,
                            );
                        }
                        PeerEvent::WsDisconnected(id) => {
                            println!(
                                "  {} peer ws disconnected: {}",
                                output::status_indicator("offline"),
                                id,
                            );
                        }
                        PeerEvent::Updated(_) => {}
                        PeerEvent::AuthRequired { .. } => {}
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Peer event stream lagged, missed {n} events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    server.run().await.map_err(|e| (exit_codes::ERROR, e))?;
    Ok(())
}

async fn run_background(
    config: &TruffleConfig,
    device_name: Option<&str>,
    app_id: Option<&str>,
    json: bool,
) -> Result<(), (i32, String)> {
    let exe = std::env::current_exe().map_err(|e| {
        (
            exit_codes::ERROR,
            format!("Failed to get current executable: {e}"),
        )
    })?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("up").arg("--foreground");

    if let Some(name) = device_name {
        cmd.arg("--device-name").arg(name);
    }

    if let Some(id) = app_id {
        cmd.arg("--app-id").arg(id);
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

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

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd.spawn().map_err(|e| {
        (
            exit_codes::ERROR,
            format!("Failed to start background daemon: {e}"),
        )
    })?;

    let client = DaemonClient::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err((
                exit_codes::TIMEOUT,
                "Timed out waiting for daemon to start".to_string(),
            ));
        }

        if client.is_daemon_running() {
            if client.connect().await.is_ok() {
                break;
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }

    // Fire-and-forget background update (after daemon is up, never blocks).
    auto_update::spawn_background_update(config.updates.clone());

    if json {
        // JSON output: query the daemon for PID and status
        let mut map = json_output::envelope(&config.node.device_name);
        map.insert("status".to_string(), serde_json::json!("started"));

        if let Ok(result) = client.request(method::STATUS, serde_json::json!({})).await {
            if let Some(pid) = result["pid"].as_u64() {
                map.insert("pid".to_string(), serde_json::json!(pid));
            }
        }

        json_output::print_json(&serde_json::Value::Object(map));
        return Ok(());
    }

    // Show status
    println!();
    println!("  {}", output::bold("truffle v2"));
    println!("  {}", output::dim(&"\u{2500}".repeat(39)));
    println!();

    // Show update notification from a previous background download.
    auto_update::show_update_notification();

    println!("  {:<12}{}", "Node", output::bold(&config.node.device_name));

    // Poll for status
    let status_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
    let mut final_status = None;
    while tokio::time::Instant::now() < status_deadline {
        if let Ok(result) = client.request(method::STATUS, serde_json::json!({})).await {
            let status = result["status"].as_str().unwrap_or("offline");
            let ip = result["ip"].as_str().unwrap_or("");

            if !ip.is_empty() || status == "online" {
                final_status = Some(result);
                break;
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    if let Some(result) = final_status {
        let status = result["status"].as_str().unwrap_or("online");
        println!(
            "  {:<12}{} {}",
            "Status",
            output::status_indicator(status),
            output::status_label(status),
        );
        if let Some(ip) = result["ip"].as_str() {
            if !ip.is_empty() {
                println!("  {:<12}{}", "IP", ip);
            }
        }
        if let Some(dns) = result["dns_name"].as_str() {
            if !dns.is_empty() {
                println!("  {:<12}{}", "DNS", output::dim(dns));
            }
        }
    } else {
        println!(
            "  {:<12}{} {}",
            "Status",
            output::status_indicator("connecting"),
            output::dim("Starting up..."),
        );
        println!(
            "  Run '{}' to check progress",
            output::bold("truffle status")
        );
    }

    println!();
    Ok(())
}
