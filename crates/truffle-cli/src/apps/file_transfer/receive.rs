//! Receive handler — background task that handles incoming file transfer
//! requests from other peers.
//!
//! Thin wrapper over `truffle_core::file_transfer::FileTransfer::auto_accept()`.
//! The core handles all protocol details (OFFER, PULL_REQUEST, TCP, SHA-256).

use std::sync::Arc;

use tracing::info;
use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::node::Node;

/// Spawn a background task that auto-accepts incoming file transfers.
///
/// Delegates to `node.file_transfer().auto_accept()` which handles:
/// - Incoming OFFERs (auto-accept, receive via TCP, verify SHA-256)
/// - Incoming PULL_REQUESTs (read file, send OFFER, stream via TCP)
///
/// Uses concrete `TailscaleProvider` to ensure futures are Send.
pub fn spawn_receive_handler(
    node: Arc<Node<TailscaleProvider>>,
    output_dir: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!(output_dir = output_dir.as_str(), "Starting file transfer receive handler (core auto_accept)");

        // Create output directory if it doesn't exist
        if let Err(e) = tokio::fs::create_dir_all(&output_dir).await {
            tracing::error!(dir = output_dir.as_str(), "Failed to create output dir: {e}");
            return;
        }

        let ft = node.file_transfer();
        ft.auto_accept(node.clone(), &output_dir).await;

        // Keep the task alive — auto_accept spawns its own internal task
        // that runs until the node shuts down. We just need to hold this
        // task open so the JoinHandle remains valid.
        std::future::pending::<()>().await;
    })
}
