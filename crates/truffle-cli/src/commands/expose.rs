//! `truffle expose` -- expose a local port to the tailnet.

/// Expose a local port.
pub async fn run(_port: u16, _https: bool, _name: Option<&str>) -> Result<(), String> {
    eprintln!("truffle expose: not yet implemented (Phase 7)");
    std::process::exit(1);
}
