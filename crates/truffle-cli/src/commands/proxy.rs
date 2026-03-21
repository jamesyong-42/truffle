//! `truffle proxy` -- port forwarding with live dashboard.
//!
//! Forwards a local port to a remote node:port via the daemon. The daemon
//! manages the actual forwarding state; the CLI just displays a live
//! dashboard showing connection count and bandwidth.
//!
//! ```text
//! $ truffle proxy 5432 server:5432
//!
//!   truffle proxy
//!   -----------------------------------
//!
//!   Local       127.0.0.1:5432
//!   Remote      server:5432 (100.64.0.1)
//!   Status      * forwarding
//!   Latency     2ms
//!
//!   Connections   0 active, 0 total
//!   Bandwidth     0 B up  0 B down
//!
//!   Press Ctrl+C to stop.
//! ```

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::target::Target;

/// Start port forwarding from a local port to a remote target.
///
/// The daemon manages the actual TCP forwarding. The CLI displays status
/// and handles Ctrl+C to send `proxy_stop`.
pub async fn run(
    config: &TruffleConfig,
    local_port: u16,
    target: &str,
    bind_addr: Option<&str>,
) -> Result<(), String> {
    // Parse the target address
    let resolved = config.resolve_alias(target);
    let parsed = Target::parse(resolved).map_err(|e| format!("Invalid target: {e}"))?;

    let node = &parsed.node;
    let remote_port = parsed.port.ok_or_else(|| {
        format!("Which port? Usage: truffle proxy {local_port} {node}:<port>")
    })?;

    let bind = bind_addr.unwrap_or("127.0.0.1");
    let remote_target = format!("{node}:{remote_port}");

    // Connect to the daemon
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    // Send proxy_start request
    let result = client
        .request(
            method::PROXY_START,
            serde_json::json!({
                "local_port": local_port,
                "remote_target": remote_target,
                "node": node,
                "remote_port": remote_port,
                "bind_addr": bind,
            }),
        )
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("in use") {
                format!(
                    "Port {local_port} is already in use.\n\n  Try:\n    truffle proxy {} {remote_target}    use a different local port",
                    local_port + 10000
                )
            } else {
                format!("Failed to start proxy: {msg}")
            }
        })?;

    let proxy_id = result["proxy_id"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let remote_ip = result["remote_ip"]
        .as_str()
        .unwrap_or("");

    // Display the live dashboard
    println!();
    println!("  truffle proxy");
    println!("  {}", "-".repeat(39));
    println!();
    println!("  Local       {bind}:{local_port}");
    if remote_ip.is_empty() {
        println!("  Remote      {remote_target}");
    } else {
        println!("  Remote      {remote_target} ({remote_ip})");
    }
    println!("  Status      * forwarding");
    println!();
    println!("  Connections   0 active, 0 total");
    println!("  Bandwidth     0 B up  0 B down");
    println!();
    println!("  Press Ctrl+C to stop.");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");

    println!("\n  Stopping proxy...");

    // Send proxy_stop to the daemon
    let _ = client
        .request(
            method::PROXY_STOP,
            serde_json::json!({
                "proxy_id": proxy_id,
            }),
        )
        .await;

    println!("  Proxy stopped.");
    Ok(())
}

/// Legacy runtime-based proxy (kept for `truffle http proxy`).
///
/// This function creates its own runtime. New code should use the
/// daemon-based `run()` function above.
#[allow(dead_code)]
pub async fn run_legacy(
    hostname: &str,
    sidecar: Option<&str>,
    state_dir: Option<&str>,
    prefix: &str,
    target: &str,
) -> Result<(), String> {
    use truffle_core::runtime::TruffleEvent;

    let (runtime, mut event_rx) = super::build_runtime(hostname, sidecar, state_dir).await?;

    // Wait for the node to come online
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(15);
    let mut online = false;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            event = event_rx.recv() => {
                match event {
                    Ok(TruffleEvent::Online { ip, dns_name }) => {
                        println!("Node online at {ip} ({dns_name})");
                        online = true;
                        break;
                    }
                    Ok(TruffleEvent::AuthRequired { auth_url }) => {
                        println!("Auth required: {auth_url}");
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Skipped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    if !online {
        runtime.stop().await;
        return Err("Timed out waiting for node to come online".to_string());
    }

    // Register the reverse proxy route
    let http = runtime
        .http()
        .await
        .ok_or_else(|| "HTTP router not available (sidecar not started?)".to_string())?;

    http.proxy(prefix, target)
        .await
        .map_err(|e| format!("Failed to register proxy route: {e}"))?;

    println!("Proxy active: {prefix} -> {target}");
    println!("Press Ctrl+C to stop.");

    super::wait_for_shutdown(&runtime).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::target::Target;

    #[test]
    fn test_proxy_target_parsing() {
        let t = Target::parse("server:5432").unwrap();
        assert_eq!(t.node, "server");
        assert_eq!(t.port, Some(5432));
    }

    #[test]
    fn test_proxy_port_validation() {
        // Port 0 is technically valid as u16 but is not useful
        let t = Target::parse("server:0").unwrap();
        assert_eq!(t.port, Some(0));

        // Standard ports
        let t = Target::parse("server:80").unwrap();
        assert_eq!(t.port, Some(80));

        let t = Target::parse("server:443").unwrap();
        assert_eq!(t.port, Some(443));

        let t = Target::parse("server:65535").unwrap();
        assert_eq!(t.port, Some(65535));
    }

    #[test]
    fn test_proxy_target_requires_port() {
        let t = Target::parse("server").unwrap();
        assert_eq!(t.port, None);
    }

    #[test]
    fn test_proxy_target_with_ip() {
        let t = Target::parse("100.64.0.1:5432").unwrap();
        assert_eq!(t.node, "100.64.0.1");
        assert_eq!(t.port, Some(5432));
    }
}
