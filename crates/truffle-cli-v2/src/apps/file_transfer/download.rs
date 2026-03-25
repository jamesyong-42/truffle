//! Download (pull) — request and receive a file from a remote peer.
//!
//! 1. Send PULL_REQUEST via WS
//! 2. Wait for OFFER from peer
//! 3. Send ACCEPT
//! 4. Listen for TCP connection from peer
//! 5. Read [size][sha256][file_bytes]
//! 6. Verify SHA-256, write to disk
//! 7. Send ACK

use std::time::Instant;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::info;
use truffle_core_v2::network::NetworkProvider;
use truffle_core_v2::node::Node;

use super::types::{FtMessage, TransferError, TransferResult};

/// Download a file from a remote peer.
///
/// `progress_cb` is called periodically with (bytes_received, total_bytes, speed_bps).
pub async fn download<N: NetworkProvider + 'static>(
    node: &Node<N>,
    peer_id: &str,
    remote_path: &str,
    local_path: &str,
    progress_cb: impl Fn(u64, u64, f64),
) -> Result<TransferResult, TransferError> {
    let start = Instant::now();
    let token = uuid::Uuid::new_v4().to_string();
    let requester_id = node.local_info().id;

    // 1. Send PULL_REQUEST
    let pull_req = FtMessage::PullRequest {
        path: remote_path.to_string(),
        requester_id,
        token: token.clone(),
    };

    let pull_bytes = serde_json::to_vec(&pull_req)
        .map_err(|e| TransferError::Protocol(format!("Failed to serialize pull request: {e}")))?;

    node.send(peer_id, "ft", &pull_bytes)
        .await
        .map_err(|e| TransferError::Node(e.to_string()))?;

    info!(peer = peer_id, path = remote_path, "Sent PULL_REQUEST");

    // 2. Wait for OFFER from peer
    let mut rx = node.subscribe("ft");
    let offer_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

    let (offer_sha256, offer_size, offer_token) = loop {
        let msg = tokio::time::timeout(
            offer_deadline.duration_since(tokio::time::Instant::now()),
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
            FtMessage::Offer {
                sha256,
                size,
                token: offer_token,
                ..
            } => {
                info!(size = size, sha256 = sha256.as_str(), "Received OFFER from peer");
                break (sha256, size, offer_token);
            }
            FtMessage::Reject { reason, .. } => {
                return Err(TransferError::Rejected(reason));
            }
            _ => continue,
        }
    };

    // 3. Start TCP listener, then send ACCEPT with our port
    let mut listener = node
        .listen_tcp(0)
        .await
        .map_err(|e| TransferError::Node(format!("Failed to listen TCP: {e}")))?;

    let accept = FtMessage::Accept {
        token: offer_token,
        tcp_port: listener.port,
    };

    let accept_bytes = serde_json::to_vec(&accept)
        .map_err(|e| TransferError::Protocol(format!("Failed to serialize accept: {e}")))?;

    node.send(peer_id, "ft", &accept_bytes)
        .await
        .map_err(|e| TransferError::Node(e.to_string()))?;

    info!(port = listener.port, "Sent ACCEPT, listening for TCP connection");

    // 4. Accept incoming TCP connection with timeout
    let incoming = tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        listener.accept(),
    )
    .await
    .map_err(|_| TransferError::Timeout)?
    .ok_or_else(|| TransferError::Protocol("Listener closed before accepting".to_string()))?;

    let mut stream = incoming.stream;
    info!("TCP connection accepted from peer");

    // 5. Read [8-byte size][64-byte sha256_hex][file_bytes]
    let mut size_buf = [0u8; 8];
    stream.read_exact(&mut size_buf).await?;
    let file_size = u64::from_be_bytes(size_buf);

    let mut sha_buf = [0u8; 64]; // hex-encoded SHA-256
    stream.read_exact(&mut sha_buf).await?;
    let received_sha = String::from_utf8_lossy(&sha_buf).to_string();

    // Verify the metadata matches the OFFER
    if received_sha != offer_sha256 {
        return Err(TransferError::IntegrityError {
            expected: offer_sha256,
            actual: received_sha,
        });
    }

    if file_size != offer_size {
        return Err(TransferError::Protocol(format!(
            "Size mismatch: offer said {offer_size}, stream header says {file_size}"
        )));
    }

    // Read file data in chunks
    let mut file_data = Vec::with_capacity(file_size as usize);
    let mut bytes_received: u64 = 0;
    let progress_start = Instant::now();
    let mut buf = vec![0u8; 64 * 1024];

    while bytes_received < file_size {
        let to_read = ((file_size - bytes_received) as usize).min(buf.len());
        let n = stream.read(&mut buf[..to_read]).await?;
        if n == 0 {
            return Err(TransferError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("Connection closed after {bytes_received}/{file_size} bytes"),
            )));
        }
        file_data.extend_from_slice(&buf[..n]);
        bytes_received += n as u64;

        let elapsed = progress_start.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            bytes_received as f64 / elapsed
        } else {
            0.0
        };
        progress_cb(bytes_received, file_size, speed);
    }

    // 6. Verify SHA-256
    let mut hasher = Sha256::new();
    hasher.update(&file_data);
    let actual_sha = hex::encode(hasher.finalize());

    if actual_sha != offer_sha256 {
        // Send NACK
        stream.write_all(&[0x00]).await?;
        return Err(TransferError::IntegrityError {
            expected: offer_sha256,
            actual: actual_sha,
        });
    }

    // 7. Write to disk
    // Create parent directories if needed
    if let Some(parent) = std::path::Path::new(local_path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(local_path, &file_data).await?;

    // 8. Send ACK
    stream.write_all(&[0x01]).await?;

    let elapsed = start.elapsed().as_secs_f64();
    info!(
        bytes = file_size,
        elapsed_ms = (elapsed * 1000.0) as u64,
        path = local_path,
        "Download complete"
    );

    Ok(TransferResult {
        bytes_transferred: file_size,
        sha256: actual_sha,
        elapsed_secs: elapsed,
    })
}
