//! `truffle tcp` -- open a raw TCP connection (like netcat).

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output;

pub async fn run(
    config: &TruffleConfig,
    target: &str,
    check: bool,
) -> Result<(), String> {
    // Parse target as node:port
    let (node, port_str) = target
        .rsplit_once(':')
        .ok_or_else(|| format!("Invalid target format: {target}. Expected 'node:port'."))?;

    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("Invalid port: {port_str}"))?;

    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| e.to_string())?;

    if check {
        // Check mode: just verify connectivity
        let result = client
            .request(
                method::TCP_CONNECT,
                serde_json::json!({
                    "node": node,
                    "port": port,
                }),
            )
            .await;

        match result {
            Ok(_) => {
                output::print_success(&format!(
                    "Connection to {}:{} succeeded",
                    output::bold(node),
                    port,
                ));
                Ok(())
            }
            Err(e) => {
                output::print_error(
                    &format!("Connection to {}:{} failed", node, port),
                    &e.to_string(),
                    "truffle ping    check if the node is reachable",
                );
                Err(e.to_string())
            }
        }
    } else {
        // Stream mode: pipe stdin/stdout through the TCP connection
        // For now, this is a simplified implementation that just verifies the connection
        // Full bidirectional piping requires the daemon to relay stdin/stdout,
        // which adds complexity. For v2 MVP, we support check mode.
        output::print_warning(
            "Interactive TCP streaming is not yet implemented in v2. Use --check for connectivity test.",
        );
        Err("Interactive TCP mode not yet implemented".to_string())
    }
}
