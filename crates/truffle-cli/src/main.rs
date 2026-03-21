use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod commands;
pub mod config;
pub mod daemon;
pub mod target;

// ═══════════════════════════════════════════════════════════════════════════
// CLI structure
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Parser)]
#[command(
    name = "truffle",
    about = "Mesh networking over Tailscale",
    long_about = "truffle -- Mesh networking for your devices, built on Tailscale.\n\n\
        Start a node with 'truffle up', list peers with 'truffle ls',\n\
        and connect to services with 'truffle tcp', 'truffle proxy', etc.",
    version,
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Config file path
    #[arg(long, global = true)]
    config: Option<String>,

    /// Output format override
    #[arg(long, global = true)]
    format: Option<OutputFormat>,

    /// Suppress all non-essential output
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Show detailed output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Color mode
    #[arg(long, global = true, default_value = "auto")]
    color: String,
}

#[derive(Clone, Debug, clap::ValueEnum)]
enum OutputFormat {
    Pretty,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    /// Start your node and join the mesh
    Up {
        /// Custom node name
        #[arg(long)]
        name: Option<String>,
        /// Tailscale hostname prefix
        #[arg(long)]
        hostname: Option<String>,
        /// Use an ephemeral Tailscale node (cleaned up on exit)
        #[arg(long)]
        ephemeral: bool,
        /// Run in foreground (for debugging)
        #[arg(long)]
        foreground: bool,
    },

    /// Stop your node and leave the mesh
    Down {
        /// Force stop even if transfers are in progress
        #[arg(short, long)]
        force: bool,
    },

    /// Show your node's status and connectivity
    Status {
        /// Continuously update (like top)
        #[arg(short, long)]
        watch: bool,
    },

    /// List all nodes on your mesh
    Ls {
        /// Show offline peers too
        #[arg(short, long)]
        all: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Check if a node is reachable
    Ping {
        /// Target node name
        node: String,
        /// Number of pings
        #[arg(short = 'c', long, default_value = "4")]
        count: u32,
    },

    /// Open a raw TCP connection (like netcat)
    Tcp {
        /// Target (node:port)
        target: String,
        /// Only test connectivity, don't open interactive session
        #[arg(long)]
        check: bool,
    },

    /// Open a WebSocket connection
    Ws {
        /// Target (node:port/path)
        target: String,
        /// Pretty-print JSON frames
        #[arg(long)]
        json: bool,
        /// Send binary from stdin
        #[arg(long)]
        binary: bool,
    },

    /// Forward a remote port to your machine
    Proxy {
        /// Local port to listen on
        local_port: u16,
        /// Remote target (node:port)
        target: String,
    },

    /// Make a local port available on the mesh
    Expose {
        /// Local port to expose
        port: u16,
        /// Expose with Tailscale HTTPS cert
        #[arg(long)]
        https: bool,
        /// Custom name:port on the tailnet
        #[arg(long = "as")]
        expose_as: Option<String>,
    },

    /// Start a live chat with another node
    Chat {
        /// Target node name
        node: String,
    },

    /// Send a one-shot message to a node
    Send {
        /// Target node name
        node: String,
        /// Message text
        message: String,
    },

    /// Copy files between nodes (like scp)
    Cp {
        /// Source (local path or node:path)
        source: String,
        /// Destination (local path or node:path)
        dest: String,
    },

    /// Diagnose connectivity issues
    Doctor,

    /// Generate shell completions
    Completion {
        /// Shell type
        shell: clap_complete::Shell,
    },

    /// HTTP serving and proxying commands
    Http {
        #[command(subcommand)]
        command: HttpCommands,
    },

    /// Developer/debug subcommands
    Dev {
        #[command(subcommand)]
        command: DevCommands,
    },
}

#[derive(Subcommand)]
enum HttpCommands {
    /// Reverse proxy with path prefix
    Proxy {
        /// URL prefix to match (e.g., "/api")
        prefix: String,
        /// Target address (e.g., "localhost:3000")
        target: String,
    },
    /// Serve static files from a directory
    Serve {
        /// Directory to serve
        dir: String,
        /// URL prefix to mount at
        #[arg(long, default_value = "/")]
        prefix: String,
    },
}

#[derive(Subcommand)]
enum DevCommands {
    /// Send a raw mesh message
    Send {
        /// Target device ID
        device_id: String,
        /// Message namespace (e.g., "app", "sync")
        namespace: String,
        /// Message type within namespace
        msg_type: String,
        /// JSON payload string
        payload: String,
    },
    /// Stream mesh events (SSE-style)
    Events,
    /// Dump active connections
    Connections,
}

// ═══════════════════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    // Initialize tracing (respects RUST_LOG env, defaults to info)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // Load configuration
    let config_path = cli.config.as_deref().map(std::path::Path::new);
    let config = match config::TruffleConfig::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error loading config: {e}");
            std::process::exit(1);
        }
    };

    // If no subcommand given, behave like `truffle status`
    let command = cli.command.unwrap_or(Commands::Status { watch: false });

    let result = match command {
        // ── Node lifecycle ────────────────────────────────────────────────
        Commands::Up {
            name,
            foreground,
            ..
        } => commands::up::run(&config, name.as_deref(), foreground).await,

        Commands::Down { force } => commands::down::run(force).await,

        Commands::Status { watch: _ } => {
            // Use daemon if running, otherwise show offline status
            let client = daemon::client::DaemonClient::new();
            if client.is_daemon_running() {
                match client
                    .request(daemon::protocol::method::STATUS, serde_json::json!({}))
                    .await
                {
                    Ok(result) => {
                        println!("=== Truffle Node Status ===");
                        println!(
                            "Device ID:    {}",
                            result["device_id"].as_str().unwrap_or("-")
                        );
                        println!(
                            "Name:         {}",
                            result["name"].as_str().unwrap_or("-")
                        );
                        println!(
                            "Type:         {}",
                            result["device_type"].as_str().unwrap_or("-")
                        );
                        println!(
                            "Hostname:     {}",
                            result["hostname"].as_str().unwrap_or("-")
                        );
                        println!(
                            "Status:       {}",
                            result["status"].as_str().unwrap_or("-")
                        );
                        if let Some(ip) = result["tailscale_ip"].as_str() {
                            println!("Tailscale IP: {ip}");
                        }
                        if let Some(dns) = result["tailscale_dns_name"].as_str() {
                            println!("DNS Name:     {dns}");
                        }
                        if let Some(uptime) = result["uptime_secs"].as_u64() {
                            let hours = uptime / 3600;
                            let mins = (uptime % 3600) / 60;
                            let secs = uptime % 60;
                            println!("Uptime:       {hours}h {mins}m {secs}s");
                        }
                        Ok(())
                    }
                    Err(e) => Err(e.to_string()),
                }
            } else {
                println!("Node is not running. Start it with 'truffle up'.");
                Ok(())
            }
        }

        // ── Discovery ─────────────────────────────────────────────────────
        Commands::Ls { all, json } => commands::ls::run(&config, all, json).await,

        Commands::Ping { node, count } => commands::ping::run(&config, &node, count).await,

        // ── Connectivity ──────────────────────────────────────────────────
        Commands::Tcp { target, check } => commands::tcp::run(&target, check).await,

        Commands::Ws {
            target,
            json,
            binary,
        } => commands::ws::run(&target, json, binary).await,

        Commands::Proxy { local_port, target } => {
            // Stub for now -- will use daemon in Phase 7
            let _ = (local_port, target);
            eprintln!("truffle proxy: not yet implemented (Phase 7)");
            std::process::exit(1);
        }

        Commands::Expose {
            port,
            https,
            expose_as,
        } => commands::expose::run(port, https, expose_as.as_deref()).await,

        // ── Communication ─────────────────────────────────────────────────
        Commands::Chat { node } => commands::chat::run(&node).await,

        Commands::Send { node, message } => {
            async {
                let client = daemon::client::DaemonClient::new();
                client
                    .ensure_running(&config)
                    .await
                    .map_err(|e| e.to_string())?;

                let result = client
                    .request(
                        daemon::protocol::method::SEND_MESSAGE,
                        serde_json::json!({
                            "device_id": node,
                            "namespace": "chat",
                            "type": "text",
                            "payload": { "text": message }
                        }),
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                let sent = result["sent"].as_bool().unwrap_or(false);
                if sent {
                    println!("Message sent.");
                } else {
                    println!("Failed to send message (node may not be connected).");
                }
                Ok(())
            }
            .await
        }

        // ── Files ─────────────────────────────────────────────────────────
        Commands::Cp { source, dest } => commands::cp::run(&source, &dest).await,

        // ── Diagnostics ───────────────────────────────────────────────────
        Commands::Doctor => commands::doctor::run().await,

        Commands::Completion { shell } => {
            commands::completion::run(shell);
            Ok(())
        }

        // ── HTTP subgroup ─────────────────────────────────────────────────
        Commands::Http { command } => match command {
            HttpCommands::Proxy { prefix, target } => {
                commands::http::proxy(&prefix, &target).await
            }
            HttpCommands::Serve { dir, prefix } => {
                commands::http::serve(&dir, &prefix).await
            }
        },

        // ── Dev subgroup ──────────────────────────────────────────────────
        Commands::Dev { command } => match command {
            DevCommands::Send {
                device_id,
                namespace,
                msg_type,
                payload,
            } => {
                async {
                    let client = daemon::client::DaemonClient::new();
                    client
                        .ensure_running(&config)
                        .await
                        .map_err(|e| e.to_string())?;

                    let payload_value: serde_json::Value = serde_json::from_str(&payload)
                        .map_err(|e| format!("Invalid JSON payload: {e}"))?;

                    let result = client
                        .request(
                            daemon::protocol::method::SEND_MESSAGE,
                            serde_json::json!({
                                "device_id": device_id,
                                "namespace": namespace,
                                "type": msg_type,
                                "payload": payload_value
                            }),
                        )
                        .await
                        .map_err(|e| e.to_string())?;

                    let sent = result["sent"].as_bool().unwrap_or(false);
                    if sent {
                        println!("Message sent.");
                    } else {
                        println!("Failed to send (device may not be connected).");
                    }
                    Ok(())
                }
                .await
            }
            DevCommands::Events => {
                eprintln!("truffle dev events: not yet implemented");
                std::process::exit(1);
            }
            DevCommands::Connections => {
                eprintln!("truffle dev connections: not yet implemented");
                std::process::exit(1);
            }
        },
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
