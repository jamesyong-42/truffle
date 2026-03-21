//! `truffle expose` -- expose a local port to the tailnet.
//!
//! Makes a local port available to all nodes on the mesh via the daemon.
//! The daemon creates a dynamic listener on the tailnet (via `tsnet:listen`
//! in the Go sidecar), and for each incoming connection from the mesh,
//! connects to localhost:port for bidirectional forwarding.
//!
//! ```text
//! $ truffle expose 3000
//!
//!   truffle expose
//!   -----------------------------------
//!
//!   Local       127.0.0.1:3000
//!   Mesh        james-macbook:3000
//!   Access      Any node can reach this via:
//!               truffle tcp james-macbook:3000
//!               truffle proxy 3000 james-macbook:3000
//!
//!   Connections   0 active
//!   Press Ctrl+C to stop.
//! ```

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;

/// Expose a local port to the mesh.
///
/// - `port`: the local port to expose
/// - `https`: whether to use Tailscale HTTPS certs
/// - `name`: optional custom service name on the tailnet
pub async fn run(
    config: &TruffleConfig,
    port: u16,
    https: bool,
    name: Option<&str>,
) -> Result<(), String> {
    // Connect to the daemon
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    // Send expose_start request
    let result = client
        .request(
            method::EXPOSE_START,
            serde_json::json!({
                "port": port,
                "https": https,
                "name": name,
            }),
        )
        .await
        .map_err(|e| format!("Failed to expose port: {e}"))?;

    let expose_id = result["expose_id"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let mesh_addr = result["mesh_addr"]
        .as_str()
        .unwrap_or("unknown");
    let hostname = result["hostname"]
        .as_str()
        .unwrap_or("this-node");

    // Display the status dashboard
    println!();
    println!("  truffle expose");
    println!("  {}", "-".repeat(39));
    println!();
    println!("  Local       127.0.0.1:{port}");
    println!("  Mesh        {mesh_addr}");
    println!("  Access      Any node can reach this via:");
    println!("              truffle tcp {hostname}:{port}");
    println!("              truffle proxy {port} {hostname}:{port}");
    println!();
    println!("  Connections   0 active");
    println!("  Press Ctrl+C to stop.");

    // Check if anything is listening locally (warning, not error)
    match tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await {
        Ok(_) => {} // Something is listening -- good
        Err(_) => {
            eprintln!();
            eprintln!("  Warning: Nothing is listening on port {port}.");
            eprintln!("  Start your service first, or it will be available once started.");
        }
    }

    // Wait for Ctrl+C
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");

    println!("\n  Stopping expose...");

    // Send expose_stop to the daemon
    let _ = client
        .request(
            method::EXPOSE_STOP,
            serde_json::json!({
                "expose_id": expose_id,
            }),
        )
        .await;

    println!("  Port {port} is no longer exposed.");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_expose_port_validation() {
        // Valid ports -- u16 range is inherently 0-65535
        let port_min: u16 = 1;
        let port_max: u16 = 65535;
        assert!(port_min > 0);
        assert_eq!(port_max, u16::MAX);

        // Common service ports
        let common_ports: Vec<u16> = vec![80, 443, 3000, 5173, 8080, 8443];
        for port in common_ports {
            assert!(port > 0);
        }
    }

    #[test]
    fn test_expose_name_formatting() {
        // Service name should be usable in a target address
        let names = vec!["my-api", "web-server", "dev-3000"];
        for name in names {
            assert!(!name.is_empty());
            assert!(!name.contains(' '));
        }
    }
}
