//! `truffle proxy` -- manage reverse proxies.

use clap::Subcommand;

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::exit_codes;
use crate::json_output;
use crate::output;

#[derive(Subcommand)]
pub enum ProxyCommands {
    /// Start a reverse proxy to expose a local port to the mesh
    Add {
        /// Unique ID and display name for this proxy
        #[arg(long)]
        name: String,
        /// Local port to proxy to (the target)
        #[arg(long)]
        target_port: u16,
        /// Port to listen on the mesh (external)
        #[arg(long)]
        listen_port: u16,
        /// Target host (default: localhost)
        #[arg(long, default_value = "localhost")]
        target_host: String,
        /// Target scheme: http or https (default: http)
        #[arg(long, default_value = "http")]
        scheme: String,
        /// Don't announce this proxy on the mesh
        #[arg(long)]
        no_announce: bool,
    },
    /// Stop a reverse proxy
    Remove {
        /// Proxy ID or name to remove
        id: String,
    },
    /// List active reverse proxies
    #[command(visible_aliases = &["ls"])]
    List,
}

pub async fn run(
    config: &TruffleConfig,
    command: ProxyCommands,
    json: bool,
) -> Result<(), (i32, String)> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| (exit_codes::NOT_ONLINE, e.to_string()))?;

    match command {
        ProxyCommands::Add {
            name,
            target_port,
            listen_port,
            target_host,
            scheme,
            no_announce,
        } => {
            let result = client
                .request(
                    method::PROXY_ADD,
                    serde_json::json!({
                        "id": name.clone(),
                        "name": name,
                        "listen_port": listen_port,
                        "target_host": target_host,
                        "target_port": target_port,
                        "target_scheme": scheme,
                        "announce": !no_announce,
                    }),
                )
                .await
                .map_err(|e| (exit_codes::ERROR, e.to_string()))?;

            if json {
                let mut map = json_output::envelope(&config.node.device_name);
                map.insert("proxy".to_string(), result.clone());
                json_output::print_json(&serde_json::Value::Object(map));
            } else {
                let url = result["url"].as_str().unwrap_or("unknown");
                let port = result["listen_port"].as_u64().unwrap_or(0);
                output::print_success(&format!(
                    "Proxy {} started on port {} → {}:{}",
                    output::bold(&name),
                    port,
                    target_host,
                    target_port,
                ));
                println!("  URL: {}", url);
            }
        }
        ProxyCommands::Remove { id } => {
            client
                .request(method::PROXY_REMOVE, serde_json::json!({ "id": id }))
                .await
                .map_err(|e| (exit_codes::ERROR, e.to_string()))?;

            if json {
                let mut map = json_output::envelope(&config.node.device_name);
                map.insert("removed".to_string(), serde_json::json!(id));
                json_output::print_json(&serde_json::Value::Object(map));
            } else {
                output::print_success(&format!("Proxy {} stopped.", output::bold(&id)));
            }
        }
        ProxyCommands::List => {
            let result = client
                .request(method::PROXY_LIST, serde_json::json!({}))
                .await
                .map_err(|e| (exit_codes::ERROR, e.to_string()))?;

            let proxies = result["proxies"].as_array();

            if json {
                let mut map = json_output::envelope(&config.node.device_name);
                map.insert(
                    "proxies".to_string(),
                    proxies
                        .map(|p| serde_json::Value::Array(p.clone()))
                        .unwrap_or(serde_json::Value::Array(vec![])),
                );
                json_output::print_json(&serde_json::Value::Object(map));
            } else {
                match proxies {
                    Some(arr) if !arr.is_empty() => {
                        println!("{:<16} {:<8} {:<24} {}", "NAME", "PORT", "TARGET", "URL");
                        for proxy in arr {
                            let name = proxy["name"].as_str().unwrap_or("-");
                            let port = proxy["listen_port"].as_u64().unwrap_or(0);
                            let host = proxy["target_host"].as_str().unwrap_or("localhost");
                            let tport = proxy["target_port"].as_u64().unwrap_or(0);
                            let scheme = proxy["target_scheme"].as_str().unwrap_or("http");
                            let url = proxy["url"].as_str().unwrap_or("-");
                            println!(
                                "{:<16} {:<8} {:<24} {}",
                                name,
                                port,
                                format!("{}://{}:{}", scheme, host, tport),
                                url,
                            );
                        }
                    }
                    _ => {
                        println!("No active proxies.");
                        println!("  Use 'truffle proxy add' to start one.");
                    }
                }
            }
        }
    }

    Ok(())
}
