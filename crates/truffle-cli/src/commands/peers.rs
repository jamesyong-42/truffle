use truffle_core::runtime::TruffleEvent;

use super::build_runtime;

/// List discovered peers with connection quality info.
pub async fn run(
    hostname: &str,
    sidecar: Option<&str>,
    state_dir: Option<&str>,
) -> Result<(), String> {
    let (runtime, mut event_rx) = build_runtime(hostname, sidecar, state_dir).await?;

    // Wait for initial discovery to populate the device list
    println!("Discovering peers...");
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            event = event_rx.recv() => {
                match event {
                    Ok(TruffleEvent::PeersChanged(_)) => {
                        // Peers updated, give a brief moment for more updates
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        break;
                    }
                    Ok(TruffleEvent::Online { .. }) => {
                        // We're online - wait a bit more for peer discovery
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
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

    let local_id = runtime.device_id().await;
    let devices = runtime.devices().await;

    let peers: Vec<_> = devices.iter().filter(|d| d.id != local_id).collect();

    if peers.is_empty() {
        println!("No peers discovered.");
    } else {
        println!("=== Discovered Peers ({}) ===", peers.len());
        println!(
            "{:<36}  {:<16}  {:<10}  {:<10}  {:<16}  {}",
            "ID", "NAME", "TYPE", "STATUS", "IP", "LATENCY"
        );
        println!("{}", "-".repeat(110));

        for peer in &peers {
            let ip = peer
                .tailscale_ip
                .as_deref()
                .unwrap_or("-");
            let latency = peer
                .latency_ms
                .map(|ms| format!("{ms:.1}ms"))
                .unwrap_or_else(|| "-".to_string());
            let role_badge = String::new();

            println!(
                "{:<36}  {:<16}  {:<10}  {:<10}  {:<16}  {}{}",
                peer.id,
                truncate(&peer.name, 16),
                peer.device_type,
                format!("{:?}", peer.status),
                ip,
                latency,
                role_badge,
            );
        }
    }

    runtime.stop().await;
    Ok(())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}
