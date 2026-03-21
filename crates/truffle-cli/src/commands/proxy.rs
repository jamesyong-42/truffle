use truffle_core::runtime::TruffleEvent;

use super::{build_runtime, wait_for_shutdown};

/// Start a reverse proxy route. Runs until Ctrl+C.
///
/// **Legacy**: This function creates its own runtime. New code should use
/// `truffle proxy` which goes through the daemon.
#[allow(dead_code)]
pub async fn run(
    hostname: &str,
    sidecar: Option<&str>,
    state_dir: Option<&str>,
    prefix: &str,
    target: &str,
) -> Result<(), String> {
    let (runtime, mut event_rx) = build_runtime(hostname, sidecar, state_dir).await?;

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

    wait_for_shutdown(&runtime).await;
    Ok(())
}
