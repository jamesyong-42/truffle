use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod apps;
mod auto_update;
mod commands;
pub mod config;
pub mod daemon;
pub mod exit_codes;
pub mod json_output;
pub mod output;
pub mod resolve;
mod tui;

// ==========================================================================
// CLI structure
// ==========================================================================

#[derive(Parser)]
#[command(
    name = "truffle",
    about = "Mesh networking for your devices, built on Tailscale. (v2 — Node API)",
    long_about = "truffle v2 -- Mesh networking for your devices, built on Tailscale.\n\n\
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

    /// Suppress all non-essential output
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Show detailed output (debug info, timings)
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Force color: auto, always, never
    #[arg(long, global = true, default_value = "auto")]
    color: String,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Start your node and join the mesh
    Up {
        /// Custom node name
        #[arg(long)]
        name: Option<String>,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// See who's on your mesh
    #[command(visible_aliases = &["list", "nodes"])]
    Ls {
        /// Show offline peers too
        #[arg(short, long)]
        all: bool,
        /// Show detailed info (IP, OS, connection type)
        #[arg(short, long)]
        long: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Check if a node is reachable and measure latency
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

    /// Send a one-shot message to a node
    Send {
        /// Target node name
        node: String,
        /// Message text
        message: String,
        /// Send to all nodes
        #[arg(short, long)]
        all: bool,
        /// Wait for and print the reply
        #[arg(short, long)]
        wait: bool,
    },

    /// Copy files between nodes (like scp)
    #[command(long_about = "Copy files between nodes (like scp).\n\n\
        Uses scp-style syntax: truffle cp file.txt server:/tmp/\n\
        Transfers use raw TCP via Tailscale. SHA-256 verification is always on.")]
    Cp {
        /// Source (local path or node:path)
        source: String,
        /// Destination (local path or node:path)
        dest: String,
        /// Skip SHA-256 integrity verification after transfer
        #[arg(long = "no-verify", default_value_t = false)]
        no_verify: bool,
    },

    /// Stream mesh events in real time
    Watch {
        /// Output as JSON lines (JSONL)
        #[arg(long)]
        json: bool,
        /// Filter by event type: peer, message, transfer
        #[arg(long = "filter", short)]
        filters: Vec<String>,
        /// Stop after N seconds
        #[arg(long)]
        timeout: Option<u64>,
    },

    /// Block until a peer comes online
    Wait {
        /// Target node name
        node: String,
        /// Timeout in seconds (exit code 5 on timeout)
        #[arg(long)]
        timeout: Option<u64>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Block for the next incoming message
    Recv {
        /// Only accept messages from this node
        #[arg(long)]
        from: Option<String>,
        /// Timeout in seconds (exit code 5 on timeout)
        #[arg(long)]
        timeout: Option<u64>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Diagnose connectivity issues
    Doctor,

    /// Update truffle to the latest release
    Update,
}

// ==========================================================================
// Main
// ==========================================================================

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let global_json = cli.json;

    // Initialize color output mode; force disable colors when --json is active
    if global_json {
        output::init_color("never");
    } else {
        output::init_color(&cli.color);
    }

    // Load configuration
    let config_path = cli.config.as_deref().map(std::path::Path::new);
    let config = match config::TruffleConfig::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            if global_json {
                json_output::print_json(&json_output::error_envelope(
                    exit_codes::ERROR,
                    "config_error",
                    &format!("Can't load configuration: {e}"),
                    &format!(
                        "Check your config file at {}",
                        config::TruffleConfig::default_path().display()
                    ),
                ));
            } else {
                output::print_error(
                    "Can't load configuration",
                    &e.to_string(),
                    &format!(
                        "Check your config file at {}",
                        config::TruffleConfig::default_path().display()
                    ),
                );
            }
            std::process::exit(exit_codes::ERROR);
        }
    };

    // If no subcommand given: TUI on TTY (non-json), status otherwise
    if cli.command.is_none() {
        if !global_json && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            // Bare `truffle` on a TTY → launch TUI
            if let Err(e) = tui::run(&config).await {
                output::print_error(&e, "", "");
                std::process::exit(exit_codes::ERROR);
            }
        } else {
            // Bare `truffle` piped or with --json → show status
            if let Err((code, msg)) = commands::status::run(&config, global_json, false).await {
                handle_error(global_json, code, &msg);
            }
        }
        return;
    }

    let command = cli.command.unwrap();

    let result = match command {
        // -- Node lifecycle --
        Commands::Up { name, foreground } => {
            commands::up::run(&config, name.as_deref(), foreground, global_json).await
        }

        Commands::Down { force } => commands::down::run(&config, force, global_json).await,

        Commands::Status {
            watch,
            json: local_json,
        } => {
            let json = global_json || local_json;
            commands::status::run(&config, json, watch).await
        }

        // -- Discovery --
        Commands::Ls {
            all,
            long,
            json: local_json,
        } => {
            let json = global_json || local_json;
            commands::ls::run(&config, all, long, json).await
        }

        Commands::Ping { node, count } => {
            commands::ping::run(&config, &node, count, global_json).await
        }

        // -- Connectivity --
        Commands::Tcp { target, check } => {
            commands::tcp::run(&config, &target, check, global_json).await
        }

        // -- Communication --
        Commands::Send {
            node,
            message,
            all,
            wait,
        } => commands::send::run(&config, &node, &message, all, wait, global_json).await,

        // -- Files --
        Commands::Cp {
            source,
            dest,
            no_verify,
        } => commands::cp::run(&config, &source, &dest, !no_verify, global_json).await,

        // -- Streaming --
        Commands::Watch {
            json: local_json,
            filters,
            timeout,
        } => {
            let json = global_json || local_json;
            commands::watch::run(&config, json, &filters, timeout).await
        }

        Commands::Wait {
            node,
            timeout,
            json: local_json,
        } => {
            let json = global_json || local_json;
            commands::wait::run(&config, &node, timeout, json).await
        }

        Commands::Recv {
            from,
            timeout,
            json: local_json,
        } => {
            let json = global_json || local_json;
            commands::recv::run(&config, from.as_deref(), timeout, json).await
        }

        // -- Diagnostics --
        Commands::Doctor => commands::doctor::run(&config, global_json).await,

        // -- Self-update --
        Commands::Update => commands::update::run(global_json).await,
    };

    if let Err((code, msg)) = result {
        handle_error(global_json, code, &msg);
    }
}

fn handle_error(json: bool, code: i32, msg: &str) {
    if json {
        if !msg.is_empty() {
            json_output::print_json(&json_output::error_envelope(
                code,
                "command_error",
                msg,
                "",
            ));
        }
    } else if !msg.is_empty() && !msg.contains('\u{2717}') {
        output::print_error(msg, "", "");
    }
    std::process::exit(code);
}
