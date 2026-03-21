//! `truffle http` -- HTTP serving and proxying subcommands.

/// Start an HTTP reverse proxy.
pub async fn proxy(_prefix: &str, _target: &str) -> Result<(), String> {
    eprintln!("truffle http proxy: not yet implemented (Phase 7)");
    std::process::exit(1);
}

/// Serve static files.
pub async fn serve(_dir: &str, _prefix: &str) -> Result<(), String> {
    eprintln!("truffle http serve: not yet implemented (Phase 7)");
    std::process::exit(1);
}
