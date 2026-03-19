use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod commands;

#[derive(Parser)]
#[command(
    name = "truffle",
    about = "Truffle mesh networking CLI",
    version,
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Hostname prefix for this node
    #[arg(long, default_value = "truffle-cli")]
    hostname: String,

    /// Path to sidecar binary
    #[arg(long, env = "TRUFFLE_SIDECAR_PATH")]
    sidecar: Option<String>,

    /// State directory
    #[arg(long, env = "TRUFFLE_STATE_DIR")]
    state_dir: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show local node status (device ID, role, Tailscale IP, auth)
    Status,

    /// List discovered peers with connection quality info
    Peers,

    /// Send a message to a specific device via the mesh bus
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

    /// Start a reverse proxy route
    Proxy {
        /// URL prefix to match (e.g., "/api")
        prefix: String,
        /// Target address to forward to (e.g., "localhost:3000")
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

#[tokio::main]
async fn main() {
    // Initialize tracing (respects RUST_LOG env, defaults to info)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Status => commands::status::run(&cli.hostname, cli.sidecar.as_deref(), cli.state_dir.as_deref()).await,
        Commands::Peers => commands::peers::run(&cli.hostname, cli.sidecar.as_deref(), cli.state_dir.as_deref()).await,
        Commands::Send {
            device_id,
            namespace,
            msg_type,
            payload,
        } => {
            commands::send::run(
                &cli.hostname,
                cli.sidecar.as_deref(),
                cli.state_dir.as_deref(),
                &device_id,
                &namespace,
                &msg_type,
                &payload,
            )
            .await
        }
        Commands::Proxy { prefix, target } => {
            commands::proxy::run(
                &cli.hostname,
                cli.sidecar.as_deref(),
                cli.state_dir.as_deref(),
                &prefix,
                &target,
            )
            .await
        }
        Commands::Serve { dir, prefix } => {
            commands::serve::run(
                &cli.hostname,
                cli.sidecar.as_deref(),
                cli.state_dir.as_deref(),
                &dir,
                &prefix,
            )
            .await
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
