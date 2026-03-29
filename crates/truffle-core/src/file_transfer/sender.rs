//! Send (push) — send a local file to a remote peer.
//!
//! 1. Stream-hash the file (constant memory)
//! 2. Send OFFER via WS
//! 3. Wait for ACCEPT (with tcp_port)
//! 4. Open raw TCP stream to the port advertised in ACCEPT
//! 5. Stream [size][sha256][file_bytes] in 64KB chunks
//! 6. Read ACK

use std::sync::atomic::Ordering;
use std::time::Instant;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;
use tracing::info;

use crate::network::NetworkProvider;
use crate::node::Node;

use super::types::{
    FileTransferEvent, FtMessage, TransferDirection, TransferError, TransferProgress,
    TransferResult,
};

/// Send a local file to a remote peer.
///
/// Uses constant memory — the file is streamed in 64KB chunks for both
/// hashing and TCP transfer. Never loads the entire file into memory.
///
/// Emits [`FileTransferEvent::Progress`], [`FileTransferEvent::Completed`],
/// or [`FileTransferEvent::Failed`] on the provided `event_tx` channel.
pub async fn send_file<N: NetworkProvider + 'static>(
    node: &Node<N>,
    peer_id: &str,
    local_path: &str,
    remote_path: &str,
    max_transfer_size: u64,
    event_tx: &broadcast::Sender<FileTransferEvent>,
) -> Result<TransferResult, TransferError> {
    let start = Instant::now();

    // 0. Resolve peer_id to the canonical node ID.
    let peer_id = node
        .resolve_peer_id(peer_id)
        .await
        .map_err(|e| TransferError::Node(e.to_string()))?;
    let peer_id = peer_id.as_str();

    // 1. Get file metadata and check size limit
    let metadata = tokio::fs::metadata(local_path)
        .await
        .map_err(TransferError::Io)?;
    let file_size = metadata.len();

    if file_size > max_transfer_size {
        return Err(TransferError::Protocol(format!(
            "File size ({} bytes) exceeds max transfer size ({} bytes)",
            file_size, max_transfer_size
        )));
    }

    // 2. Stream-hash the file in chunks (constant memory)
    info!(path = local_path, size = file_size, "Hashing file (streaming)");
    let sha256 = {
        let mut hasher = Sha256::new();
        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(TransferError::Io)?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf).await.map_err(TransferError::Io)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hex::encode(hasher.finalize())
    };

    let file_name = std::path::Path::new(local_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let token = uuid::Uuid::new_v4().to_string();

    // 3. Send OFFER via WS
    let offer = FtMessage::Offer {
        file_name: file_name.clone(),
        size: file_size,
        sha256: sha256.clone(),
        save_path: remote_path.to_string(),
        token: token.clone(),
        tcp_port: 0,
    };

    let offer_bytes = serde_json::to_vec(&offer)
        .map_err(|e| TransferError::Protocol(format!("Failed to serialize offer: {e}")))?;

    node.send(peer_id, "ft", &offer_bytes)
        .await
        .map_err(|e| TransferError::Node(e.to_string()))?;

    info!(peer = peer_id, file = file_name.as_str(), size = file_size, "Sent OFFER");

    // 4. Wait for ACCEPT (extract tcp_port)
    let mut rx = node.subscribe("ft");
    let accept_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(60);
    let accept_port;

    loop {
        let msg = tokio::time::timeout(
            accept_deadline.duration_since(tokio::time::Instant::now()),
            rx.recv(),
        )
        .await
        .map_err(|_| TransferError::Timeout)?
        .map_err(|e| TransferError::Protocol(format!("Channel error: {e}")))?;

        if msg.from != peer_id {
            continue;
        }

        let ft_msg: FtMessage = serde_json::from_value(msg.payload.clone())
            .map_err(|e| TransferError::Protocol(format!("Bad FT message: {e}")))?;

        match ft_msg {
            FtMessage::Accept {
                token: accept_token,
                tcp_port,
            } if accept_token == token => {
                accept_port = tcp_port;
                info!(tcp_port = tcp_port, "Received ACCEPT from peer");
                break;
            }
            FtMessage::Reject {
                token: reject_token,
                reason,
            } if reject_token == token => {
                return Err(TransferError::Rejected(reason));
            }
            _ => continue,
        }
    }

    // 5. Open raw TCP stream to the port advertised in ACCEPT
    let mut stream = node
        .open_tcp(peer_id, accept_port)
        .await
        .map_err(|e| TransferError::Node(format!("Failed to open TCP to port {accept_port}: {e}")))?;

    info!(port = accept_port, "TCP stream opened to peer");

    // 6. Write header: [8-byte size][64-byte sha256_hex]
    stream.write_all(&file_size.to_be_bytes()).await?;
    stream.write_all(sha256.as_bytes()).await?;

    // 7. Stream the file in 64KB chunks (constant memory)
    let mut file = tokio::fs::File::open(local_path)
        .await
        .map_err(TransferError::Io)?;
    let chunk_size = 64 * 1024;
    let mut buf = vec![0u8; chunk_size];
    let mut bytes_sent: u64 = 0;
    let progress_start = Instant::now();

    loop {
        let n = file.read(&mut buf).await.map_err(TransferError::Io)?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n]).await?;
        bytes_sent += n as u64;

        let elapsed = progress_start.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            bytes_sent as f64 / elapsed
        } else {
            0.0
        };

        let _ = event_tx.send(FileTransferEvent::Progress(TransferProgress {
            token: token.clone(),
            direction: TransferDirection::Send,
            file_name: file_name.clone(),
            bytes_transferred: bytes_sent,
            total_bytes: file_size,
            speed_bps: speed,
        }));
    }

    stream.flush().await?;

    // 8. Read ACK (1 byte: 0x01 = OK, 0x00 = error)
    let mut ack = [0u8; 1];
    stream.read_exact(&mut ack).await?;

    if ack[0] != 0x01 {
        return Err(TransferError::IntegrityError {
            expected: sha256.clone(),
            actual: "receiver reported integrity failure".to_string(),
        });
    }

    let elapsed = start.elapsed().as_secs_f64();
    info!(
        bytes = file_size,
        elapsed_ms = (elapsed * 1000.0) as u64,
        "Upload complete"
    );

    let _ = event_tx.send(FileTransferEvent::Completed {
        token,
        direction: TransferDirection::Send,
        file_name,
        bytes_transferred: file_size,
        sha256: sha256.clone(),
        elapsed_secs: elapsed,
    });

    Ok(TransferResult {
        bytes_transferred: file_size,
        sha256,
        elapsed_secs: elapsed,
    })
}
