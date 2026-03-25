//! `truffle down` -- stop the daemon.

use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output;

pub async fn run(force: bool) -> Result<(), String> {
    let _ = force; // TODO: force-kill if transfers in progress

    let client = DaemonClient::new();

    if !client.is_daemon_running() {
        println!("  Daemon is not running.");
        return Ok(());
    }

    match client.request(method::SHUTDOWN, serde_json::json!({})).await {
        Ok(_) => {
            output::print_success("Daemon stopped.");
        }
        Err(e) => {
            // Connection may close before we get the response (shutdown is immediate)
            let err_str = e.to_string();
            if err_str.contains("closed connection") || err_str.contains("I/O error") {
                output::print_success("Daemon stopped.");
            } else {
                return Err(format!("Failed to stop daemon: {e}"));
            }
        }
    }

    Ok(())
}
