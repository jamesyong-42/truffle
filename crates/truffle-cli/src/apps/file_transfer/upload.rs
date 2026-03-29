//! Upload (push) — send a local file to a remote peer.
//!
//! Thin wrapper over `truffle_core::file_transfer::FileTransfer::send_file()`.
//! The progress callback is bridged from the core's broadcast event channel.

use truffle_core::file_transfer::types::{FileTransferEvent, TransferError, TransferResult};
use truffle_core::network::NetworkProvider;
use truffle_core::node::Node;

/// Upload a local file to a remote peer.
///
/// `progress_cb` is called periodically with (bytes_sent, total_bytes, speed_bps).
/// Progress information comes from the core's `FileTransferEvent::Progress` events.
pub async fn upload<N: NetworkProvider + 'static>(
    node: &Node<N>,
    peer_id: &str,
    local_path: &str,
    remote_path: &str,
    progress_cb: impl Fn(u64, u64, f64) + Send + 'static,
) -> Result<TransferResult, TransferError> {
    let ft = node.file_transfer();
    let mut rx = ft.subscribe();

    // Spawn a task that forwards Progress events to the callback
    let progress_handle = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(FileTransferEvent::Progress(p)) => {
                    progress_cb(p.bytes_transferred, p.total_bytes, p.speed_bps);
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let result = ft.send_file(peer_id, local_path, remote_path).await;

    // Stop the progress forwarder
    progress_handle.abort();

    result
}
