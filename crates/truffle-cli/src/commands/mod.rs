pub mod peers;
pub mod proxy;
pub mod send;
pub mod serve;
pub mod status;

use std::path::PathBuf;

use truffle_core::runtime::{TruffleEvent, TruffleRuntime};

/// Build and start a TruffleRuntime from common CLI args.
///
/// Returns the runtime and a receiver for events.
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
pub async fn wait_for_shutdown(runtime: &TruffleRuntime) {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");
    println!("\nShutting down...");
    runtime.stop().await;
}
