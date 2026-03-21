use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod commands;
pub mod config;
pub mod daemon;
pub mod output;
pub mod resolve;
pub mod sidecar;
pub mod stream;
pub mod target;

// ═══════════════════════════════════════════════════════════════════════════
// CLI structure
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Parser)]
#[command(
    name = "truffle",
    about = "Mesh networking for your devices, built on Tailscale.",
    long_about = "truffle -- Mesh networking for your devices, built on Tailscale.\n\n\
        Start with 'truffle up' to join the mesh, then 'truffle ls' to see your nodes.\n\
        Run 'truffle <command> --help' for details on any command.",
    version,
    propagate_version = true,
    after_help = "Run 'truffle <command> --help' for details on any command."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to config file [default: ~/.config/truffle/config.toml]
    #[arg(long, global = true)]
    config: Option<String>,

    /// Output format override
    #[arg(long, global = true, hide = true)]
    format: Option<OutputFormat>,

    /// Suppress all non-essential output
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Show detailed output (debug info, timings)
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Force color: auto, always, never
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
    #[command(long_about = "Start your node and join the mesh.\n\n\
        Launches the truffle daemon, connects to Tailscale, and begins\n\
        discovering other nodes. Run 'truffle status' to check on it later.")]
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
    #[command(long_about = "Stop your node and leave the mesh.\n\n\
        Gracefully notifies peers and shuts down the daemon.")]
    Down {
        /// Force stop even if transfers are in progress
        #[arg(short, long)]
        force: bool,
    },

    /// Show your node's status and connectivity
    #[command(long_about = "Show your node's status and connectivity.\n\n\
        Displays your node's name, IP, uptime, and mesh statistics.\n\
        Use --watch for a live-updating dashboard.")]
    Status {
        /// Continuously update (like top)
        #[arg(short, long)]
        watch: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// See who's on your mesh
    #[command(
        long_about = "See who's on your mesh.\n\n\
            Lists all discovered nodes with their status, connection type,\n\
            and latency. Use -a to include offline nodes, -l for extra detail.",
        visible_aliases = &["list", "nodes"],
    )]
    Ls {
        /// Show offline peers too
        #[arg(short, long)]
        all: bool,
        /// Show detailed info (IP, OS, latency)
        #[arg(short, long)]
        long: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Check if a node is reachable and measure latency
    #[command(long_about = "Check if a node is reachable and measure latency.\n\n\
        Like 'ping' but for your truffle mesh. Shows round-trip time and\n\
        whether the connection is direct or relayed.")]
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
        /// Local bind address
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
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
        /// Target node (omit for group chat with all nodes)
        node: Option<String>,
    },

    /// Send a one-shot message to a node
    Send {
        /// Target node name (use "--all" to broadcast)
        node: String,
        /// Message text
        message: String,
        /// Send to all nodes (node is still required but ignored)
        #[arg(short, long)]
        all: bool,
        /// Wait for and print the reply
        #[arg(short, long)]
        wait: bool,
    },

    /// Copy files between nodes (like scp)
    #[command(long_about = "Copy files between nodes (like scp).\n\n\
        Uses scp-style syntax: truffle cp file.txt server:/tmp/\n\
        Transfers use Tailscale's Taildrop for efficient P2P data transfer.\n\
        SHA-256 verification is on by default.")]
    Cp {
        /// Source (local path or node:path)
        source: String,
        /// Destination (local path or node:path)
        dest: String,
        /// Verify integrity after transfer (SHA-256)
        #[arg(long, default_value = "true")]
        verify: bool,
    },

    /// Diagnose connectivity issues
    #[command(long_about = "Diagnose connectivity issues.\n\n\
        Checks Tailscale installation, connection status, sidecar binary,\n\
        config file, mesh connectivity, and key expiry. Each failure\n\
        includes a fix suggestion.")]
    Doctor,

    /// Generate shell completions
    #[command(long_about = "Generate shell completions.\n\n\
        Prints a completion script to stdout. Pipe it to the right location:\n\
        truffle completion zsh > ~/.zfunc/_truffle\n\
        source <(truffle completion bash)")]
    Completion {
        /// Shell type: bash, zsh, fish, powershell
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

    // Initialize color output mode
    output::init_color(&cli.color);

    // Load configuration
    let config_path = cli.config.as_deref().map(std::path::Path::new);
    let config = match config::TruffleConfig::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            output::print_error(
                "Can't load configuration",
                &e.to_string(),
                &format!(
                    "Check your config file at {}",
                    config::TruffleConfig::default_path().display()
                ),
            );
            std::process::exit(1);
        }
    };

    // If no subcommand given, behave like `truffle status`
    let command = cli
        .command
        .unwrap_or(Commands::Status {
            watch: false,
            json: false,
        });

    let result = match command {
        // ── Node lifecycle ────────────────────────────────────────────────
        Commands::Up {
            name,
            foreground,
            ..
        } => commands::up::run(&config, name.as_deref(), foreground).await,

        Commands::Down { force } => commands::down::run(force).await,

        Commands::Status { watch, json } => {
            commands::status::run(&config, json, watch).await
        }

        // ── Discovery ─────────────────────────────────────────────────────
        Commands::Ls { all, long, json } => commands::ls::run(&config, all, long, json).await,

        Commands::Ping { node, count } => commands::ping::run(&config, &node, count).await,

        // ── Connectivity ──────────────────────────────────────────────────
        Commands::Tcp { target, check } => commands::tcp::run(&config, &target, check).await,

        Commands::Ws {
            target,
            json,
            binary,
        } => commands::ws::run(&config, &target, json, binary).await,

        Commands::Proxy { local_port, target, bind } => {
            commands::proxy::run(&config, local_port, &target, Some(bind.as_str())).await
        }

        Commands::Expose {
            port,
            https,
            expose_as,
        } => commands::expose::run(&config, port, https, expose_as.as_deref()).await,

        // ── Communication ─────────────────────────────────────────────────
        Commands::Chat { node } => commands::chat::run(&config, node.as_deref()).await,

        Commands::Send { node, message, all, wait } => {
            commands::send::run(&config, Some(&node), &message, all, wait).await
        }

        // ── Files ─────────────────────────────────────────────────────────
        Commands::Cp { source, dest, verify } => commands::cp::run(&config, &source, &dest, verify).await,

        // ── Diagnostics ───────────────────────────────────────────────────
        Commands::Doctor => commands::doctor::run(&config).await,

        Commands::Completion { shell } => {
            commands::completion::run(shell);
            Ok(())
        }

        // ── HTTP subgroup ─────────────────────────────────────────────────
        Commands::Http { command } => match command {
            HttpCommands::Proxy { prefix, target } => {
                commands::http::proxy(&config, &prefix, &target).await
            }
            HttpCommands::Serve { dir, prefix } => {
                commands::http::serve(&config, &dir, &prefix).await
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
                        output::print_success("Message sent.");
                    } else {
                        output::print_error(
                            "Failed to send message",
                            "The device may not be connected.",
                            "",
                        );
                    }
                    Ok(())
                }
                .await
            }
            DevCommands::Events => {
                output::print_error(
                    "Event streaming is not yet implemented",
                    "This feature is coming in a future release.",
                    "",
                );
                std::process::exit(1);
            }
            DevCommands::Connections => {
                output::print_error(
                    "Connection dump is not yet implemented",
                    "This feature is coming in a future release.",
                    "",
                );
                std::process::exit(1);
            }
        },
    };

    if let Err(e) = result {
        // Only print if the error hasn't already been displayed by print_error
        if !e.contains('\u{2717}') {
            output::print_error(&e, "", "");
        }
        std::process::exit(1);
    }
}
