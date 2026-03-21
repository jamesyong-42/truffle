use truffle_core::runtime::TruffleEvent;

use super::build_runtime;

/// Show local node status: device ID, role, Tailscale IP, auth status.
///
/// **Legacy**: This function creates its own runtime. New code should use
/// `truffle status` which goes through the daemon.
#[allow(dead_code)]
pub async fn run(
    hostname: &str,
    sidecar: Option<&str>,
    state_dir: Option<&str>,
) -> Result<(), String> {
    let (runtime, mut event_rx) = build_runtime(hostname, sidecar, state_dir).await?;

    // Give the runtime a moment to discover its own status
    let mut online_ip = None;
    let mut online_dns = None;
    let mut authed = false;

    // Drain events briefly to pick up initial state
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            event = event_rx.recv() => {
                match event {
                    Ok(TruffleEvent::Online { ip, dns_name }) => {
                        online_ip = Some(ip);
                        online_dns = Some(dns_name);
                        // Once online, we have enough info
                        break;
                    }
                    Ok(TruffleEvent::AuthRequired { auth_url }) => {
                        println!("Auth required: {auth_url}");
                    }
                    Ok(TruffleEvent::AuthComplete) => {
                        authed = true;
                    }
                    Ok(TruffleEvent::Started) => {}
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Skipped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    let device = runtime.local_device().await;

    println!("=== Truffle Node Status ===");
    println!("Device ID:    {}", device.id);
    println!("Name:         {}", device.name);
    println!("Type:         {}", device.device_type);
    println!("Hostname:     {}", device.tailscale_hostname);
    println!("Status:       {:?}", device.status);

    if let Some(ip) = &online_ip {
        println!("Tailscale IP: {ip}");
    } else if let Some(ip) = &device.tailscale_ip {
        println!("Tailscale IP: {ip}");
    } else {
        println!("Tailscale IP: (not connected)");
    }

    if let Some(dns) = &online_dns {
        println!("DNS Name:     {dns}");
    } else if let Some(dns) = &device.tailscale_dns_name {
        println!("DNS Name:     {dns}");
    }

    println!(
        "Auth:         {}",
        if authed || online_ip.is_some() {
            "Authenticated"
        } else {
            "Pending"
        }
    );

    runtime.stop().await;
    Ok(())
}
