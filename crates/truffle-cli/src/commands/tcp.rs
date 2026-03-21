//! `truffle tcp` -- raw TCP connection (like netcat).

/// Open a raw TCP connection to a target.
pub async fn run(_target: &str, _check: bool) -> Result<(), String> {
    eprintln!("truffle tcp: not yet implemented (Phase 7)");
    std::process::exit(1);
}
