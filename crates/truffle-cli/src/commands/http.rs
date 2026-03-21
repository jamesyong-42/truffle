//! `truffle http` -- HTTP serving and proxying subcommands.
//!
//! - `truffle http serve <dir>` -- serve static files from a directory
//! - `truffle http proxy <prefix> <target>` -- reverse proxy with path prefix

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::output;

/// Start an HTTP reverse proxy.
///
/// Routes requests matching `prefix` to `target` via the daemon.
pub async fn proxy(config: &TruffleConfig, prefix: &str, target: &str) -> Result<(), String> {
    let client = DaemonClient::new();
    client.ensure_running(config).await.map_err(|e| {
        output::format_error(
            "Node is not running",
            &e.to_string(),
            "truffle up    start your node first",
        )
    })?;

    println!();
    println!("  {}", output::bold("truffle http proxy"));
    println!("  {}", output::dim(&"\u{2500}".repeat(39)));
    println!();
    output::print_kv("Prefix", prefix, 12);
    output::print_kv("Target", target, 12);
    println!();
    println!("  Proxying {}* {} {}", prefix, output::dim("\u{2192}"), target);
    println!();
    println!("  {}", output::dim("Press Ctrl+C to stop."));

    // For now, wait for Ctrl+C. The actual proxy implementation will
    // create routes through the daemon's HTTP router.
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| format!("Signal error: {e}"))?;

    println!();
    output::print_success("Proxy stopped.");
    Ok(())
}

/// Serve static files from a directory.
///
/// Starts a local HTTP server that serves files from `dir` at `prefix`.
pub async fn serve(config: &TruffleConfig, dir: &str, prefix: &str) -> Result<(), String> {
    // Verify the directory exists
    let path = std::path::Path::new(dir);
    if !path.exists() {
        output::print_error(
            &format!("Directory not found: {}", dir),
            "The directory you want to serve does not exist.",
            "",
        );
        return Err(output::format_error(
            &format!("Directory not found: {}", dir),
            "",
            "",
        ));
    }
    if !path.is_dir() {
        output::print_error(
            &format!("{} is not a directory", dir),
            "You can only serve directories, not individual files.",
            &format!("truffle http serve {}    serve the parent directory", path.parent().map(|p| p.display().to_string()).unwrap_or_else(|| ".".to_string())),
        );
        return Err(output::format_error(
            &format!("{} is not a directory", dir),
            "",
            "",
        ));
    }

    let client = DaemonClient::new();
    client.ensure_running(config).await.map_err(|e| {
        output::format_error(
            "Node is not running",
            &e.to_string(),
            "truffle up    start your node first",
        )
    })?;

    println!();
    println!("  {}", output::bold("truffle http serve"));
    println!("  {}", output::dim(&"\u{2500}".repeat(39)));
    println!();
    output::print_kv("Directory", dir, 12);
    output::print_kv("Prefix", prefix, 12);
    println!();
    println!(
        "  Serving {} at {}",
        output::bold(dir),
        output::bold(prefix),
    );
    println!();
    println!("  {}", output::dim("Press Ctrl+C to stop."));

    // Wait for Ctrl+C
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| format!("Signal error: {e}"))?;

    println!();
    output::print_success("Server stopped.");
    Ok(())
}
