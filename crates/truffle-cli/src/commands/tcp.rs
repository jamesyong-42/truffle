//! `truffle tcp` -- open a raw TCP connection (like netcat).

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::exit_codes;
use crate::json_output;
use crate::output;

pub async fn run(
    config: &TruffleConfig,
    target: &str,
    check: bool,
    json: bool,
) -> Result<(), (i32, String)> {
    // Parse target as node:port
    let (node, port_str) = target.rsplit_once(':').ok_or_else(|| {
        (
            exit_codes::USAGE,
            format!("Invalid target format: {target}. Expected 'node:port'."),
        )
    })?;

    let port: u16 = port_str
        .parse()
        .map_err(|_| (exit_codes::USAGE, format!("Invalid port: {port_str}")))?;

    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| (exit_codes::NOT_ONLINE, e.to_string()))?;

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
                if json {
                    let mut map = json_output::envelope(&config.node.device_name);
                    map.insert("connected".to_string(), serde_json::json!(true));
                    map.insert("peer".to_string(), serde_json::json!(node));
                    map.insert("port".to_string(), serde_json::json!(port));
                    json_output::print_json(&serde_json::Value::Object(map));
                } else {
                    output::print_success(&format!(
                        "Connection to {}:{} succeeded",
                        output::bold(node),
                        port,
                    ));
                }
                Ok(())
            }
            Err(e) => {
                if json {
                    let mut map = json_output::envelope(&config.node.device_name);
                    map.insert("connected".to_string(), serde_json::json!(false));
                    map.insert("peer".to_string(), serde_json::json!(node));
                    map.insert("port".to_string(), serde_json::json!(port));
                    map.insert("error".to_string(), serde_json::json!(e.to_string()));
                    json_output::print_json(&serde_json::Value::Object(map));
                } else {
                    output::print_error(
                        &format!("Connection to {}:{} failed", node, port),
                        &e.to_string(),
                        "truffle ping    check if the node is reachable",
                    );
                }
                Err((exit_codes::ERROR, e.to_string()))
            }
        }
    } else {
        // Stream mode: pipe stdin/stdout through the TCP connection
        if json {
            return Err((
                exit_codes::USAGE,
                "Interactive TCP mode is not supported with --json".to_string(),
            ));
        }
        output::print_warning(
            "Interactive TCP streaming is not yet implemented in v2. Use --check for connectivity test.",
        );
        Err((
            exit_codes::ERROR,
            "Interactive TCP mode not yet implemented".to_string(),
        ))
    }
}
