pub mod chat;
pub mod completion;
pub mod cp;
pub mod doctor;
pub mod down;
pub mod expose;
pub mod http;
pub mod install_sidecar;
pub mod ls;
pub mod peers;
pub mod ping;
pub mod proxy;
pub mod send;
pub mod serve;
pub mod status;
pub mod tcp;
pub mod uninstall;
pub mod up;
pub mod ws;

use std::path::PathBuf;

use truffle_core::runtime::{TruffleEvent, TruffleRuntime};

/// Build and start a TruffleRuntime from common CLI args.
///
/// Returns the runtime and a receiver for events.
///
/// **Deprecated for new commands**: New commands should use the daemon
/// architecture instead of building a runtime directly. This function
/// is kept for backward compatibility with legacy commands.
#[allow(dead_code)]
pub async fn build_runtime(
    hostname: &str,
    sidecar: Option<&str>,
    state_dir: Option<&str>,
) -> Result<(TruffleRuntime, tokio::sync::broadcast::Receiver<TruffleEvent>), String> {
    let mut builder = TruffleRuntime::builder()
        .hostname(hostname)
        .device_type("cli");

    if let Some(path) = sidecar {
        builder = builder.sidecar_path(PathBuf::from(path));
    }
    if let Some(dir) = state_dir {
        builder = builder.state_dir(dir);
    }

    let (runtime, _mesh_rx) = builder
        .build()
        .map_err(|e| format!("Failed to build runtime: {e}"))?;

    let event_rx = runtime
        .start()
        .await
        .map_err(|e| format!("Failed to start runtime: {e}"))?;

    Ok((runtime, event_rx))
}

/// Wait for Ctrl+C (SIGINT), then stop the runtime.
#[allow(dead_code)]
pub async fn wait_for_shutdown(runtime: &TruffleRuntime) {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");
    println!("\nShutting down...");
    runtime.stop().await;
}
