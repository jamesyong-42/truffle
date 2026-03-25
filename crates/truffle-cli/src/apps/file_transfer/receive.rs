//! Receive handler — background task that handles incoming file transfer
//! requests from other peers.
//!
//! This runs inside the daemon and automatically accepts incoming OFFER and
//! PULL_REQUEST messages.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info, warn};
use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::node::Node;

use super::types::FtMessage;

/// Spawn a background task that handles incoming file transfer messages.
///
/// This listens on the "ft" namespace and:
/// - For OFFER: auto-accepts, opens TCP listener, receives file, verifies, ACKs
/// - For PULL_REQUEST: reads file, sends OFFER, waits for ACCEPT, streams via TCP
///
/// Uses concrete `TailscaleProvider` to ensure futures are Send.
pub fn spawn_receive_handler(
    node: Arc<Node<TailscaleProvider>>,
    output_dir: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = node.subscribe("ft");
        info!(output_dir = output_dir.as_str(), "File transfer receive handler started");

        loop {
            let msg = match rx.recv().await {
                Ok(m) => m,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("FT receive handler lagged, missed {n} messages");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    info!("FT receive handler: channel closed, exiting");
                    break;
                }
            };

            let ft_msg: FtMessage = match serde_json::from_value(msg.payload.clone()) {
                Ok(m) => m,
                Err(e) => {
                    warn!(from = msg.from.as_str(), "Bad FT message: {e}");
                    continue;
                }
            };

            let node = node.clone();
            let from = msg.from.clone();
            let output_dir = output_dir.clone();

            match ft_msg {
                FtMessage::Offer {
                    file_name,
                    size,
                    sha256,
                    save_path,
                    token,
                    tcp_port: _,
                } => {
                    // Handle incoming OFFER (someone wants to push a file to us)
                    tokio::spawn(async move {
                        if let Err(e) = handle_incoming_offer(
                            &node,
                            &from,
                            &file_name,
                            size,
                            &sha256,
                            &save_path,
                            &token,
                            &output_dir,
                        )
                        .await
                        {
                            error!(from = from.as_str(), file = file_name.as_str(), "Failed to receive file: {e}");
                        }
                    });
                }
                FtMessage::PullRequest {
                    path,
                    requester_id: _,
                    token,
                } => {
                    // Handle incoming PULL_REQUEST (someone wants to download from us)
                    tokio::spawn(async move {
                        if let Err(e) =
                            handle_pull_request(&node, &from, &path, &token).await
                        {
                            error!(from = from.as_str(), path = path.as_str(), "Failed to serve file: {e}");
                        }
                    });
                }
                _ => {
                    // ACCEPT / REJECT are handled by the upload/download initiators
                }
            }
        }
    })
}

/// Handle an incoming OFFER: accept, receive file via TCP, verify, ACK.
async fn handle_incoming_offer(
    node: &Node<TailscaleProvider>,
    from: &str,
    file_name: &str,
    size: u64,
    sha256: &str,
    save_path: &str,
    token: &str,
    output_dir: &str,
) -> Result<(), String> {
    info!(
        from = from,
        file = file_name,
        size = size,
        "Accepting incoming file offer"
    );

    // Determine save location
    let dest = if save_path.is_empty() || save_path == "." {
        format!("{output_dir}/{file_name}")
    } else {
        save_path.to_string()
    };

    // Create parent directories
    if let Some(parent) = std::path::Path::new(&dest).parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    // Start TCP listener
    let mut listener = node
        .listen_tcp(0)
        .await
        .map_err(|e| format!("Failed to listen TCP: {e}"))?;

    // Send ACCEPT
    let accept = FtMessage::Accept {
        token: token.to_string(),
        tcp_port: listener.port,
    };
    let accept_bytes =
        serde_json::to_vec(&accept).map_err(|e| format!("Serialize error: {e}"))?;
    node.send(from, "ft", &accept_bytes)
        .await
        .map_err(|e| format!("Failed to send ACCEPT: {e}"))?;

    // Wait for TCP connection
    let incoming = tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        listener.accept(),
    )
    .await
    .map_err(|_| "Timeout waiting for TCP connection".to_string())?
    .ok_or_else(|| "Listener closed".to_string())?;

    let mut stream = incoming.stream;

    // Read header: [8-byte size][64-byte sha256_hex]
    let mut size_buf = [0u8; 8];
    stream
        .read_exact(&mut size_buf)
        .await
        .map_err(|e| format!("Read size: {e}"))?;
    let file_size = u64::from_be_bytes(size_buf);

    let mut sha_buf = [0u8; 64];
    stream
        .read_exact(&mut sha_buf)
        .await
        .map_err(|e| format!("Read SHA: {e}"))?;

    // Read file data
    let mut data = Vec::with_capacity(file_size as usize);
    let mut received = 0u64;
    let mut buf = vec![0u8; 64 * 1024];

    while received < file_size {
        let to_read = ((file_size - received) as usize).min(buf.len());
        let n = stream
            .read(&mut buf[..to_read])
            .await
            .map_err(|e| format!("Read data: {e}"))?;
        if n == 0 {
            return Err(format!(
                "Connection closed after {received}/{file_size} bytes"
            ));
        }
        data.extend_from_slice(&buf[..n]);
        received += n as u64;
    }

    // Verify SHA-256
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let actual_sha = hex::encode(hasher.finalize());

    if actual_sha != sha256 {
        stream
            .write_all(&[0x00])
            .await
            .map_err(|e| format!("Write NACK: {e}"))?;
        return Err(format!(
            "SHA-256 mismatch: expected {sha256}, got {actual_sha}"
        ));
    }

    // Write to disk
    tokio::fs::write(&dest, &data)
        .await
        .map_err(|e| format!("Write file: {e}"))?;

    // Send ACK
    stream
        .write_all(&[0x01])
        .await
        .map_err(|e| format!("Write ACK: {e}"))?;

    info!(
        file = dest.as_str(),
        bytes = file_size,
        "File received and verified"
    );

    Ok(())
}

/// Handle a PULL_REQUEST: read file, send OFFER, wait for ACCEPT, stream via TCP.
async fn handle_pull_request(
    node: &Node<TailscaleProvider>,
    from: &str,
    path: &str,
    _token: &str,
) -> Result<(), String> {
    info!(from = from, path = path, "Processing PULL_REQUEST");

    // Read and hash the file
    let data = tokio::fs::read(path)
        .await
        .map_err(|e| format!("Failed to read {path}: {e}"))?;
    let size = data.len() as u64;

    let mut hasher = Sha256::new();
    hasher.update(&data);
    let sha256 = hex::encode(hasher.finalize());

    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let offer_token = uuid::Uuid::new_v4().to_string();

    // Send OFFER back
    let offer = FtMessage::Offer {
        file_name,
        size,
        sha256: sha256.clone(),
        save_path: String::new(),
        token: offer_token.clone(),
        tcp_port: 0,
    };
    let offer_bytes =
        serde_json::to_vec(&offer).map_err(|e| format!("Serialize error: {e}"))?;
    node.send(from, "ft", &offer_bytes)
        .await
        .map_err(|e| format!("Failed to send OFFER: {e}"))?;

    // Wait for ACCEPT
    let mut rx = node.subscribe("ft");
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

    let accept_port = loop {
        let msg = tokio::time::timeout(
            deadline.duration_since(tokio::time::Instant::now()),
            rx.recv(),
        )
        .await
        .map_err(|_| "Timeout waiting for ACCEPT".to_string())?
        .map_err(|e| format!("Channel error: {e}"))?;

        if msg.from != from {
            continue;
        }

        let ft_msg: FtMessage =
            serde_json::from_value(msg.payload).map_err(|e| format!("Bad message: {e}"))?;

        match ft_msg {
            FtMessage::Accept {
                token: at,
                tcp_port,
            } if at == offer_token => {
                break tcp_port;
            }
            FtMessage::Reject { reason, .. } => {
                return Err(format!("Peer rejected: {reason}"));
            }
            _ => continue,
        }
    };

    // Open TCP to peer and stream the file
    let mut stream = node
        .open_tcp(from, accept_port)
        .await
        .map_err(|e| format!("Failed to open TCP to {from}:{accept_port}: {e}"))?;

    // Write [size][sha256_hex][file_data]
    stream
        .write_all(&size.to_be_bytes())
        .await
        .map_err(|e| format!("Write size: {e}"))?;
    stream
        .write_all(sha256.as_bytes())
        .await
        .map_err(|e| format!("Write SHA: {e}"))?;

    let chunk_size = 64 * 1024;
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + chunk_size).min(data.len());
        stream
            .write_all(&data[offset..end])
            .await
            .map_err(|e| format!("Write chunk: {e}"))?;
        offset = end;
    }

    stream.flush().await.map_err(|e| format!("Flush: {e}"))?;

    // Read ACK
    let mut ack = [0u8; 1];
    stream
        .read_exact(&mut ack)
        .await
        .map_err(|e| format!("Read ACK: {e}"))?;

    if ack[0] != 0x01 {
        return Err("Peer reported integrity failure".to_string());
    }

    info!(path = path, bytes = size, "File served successfully");
    Ok(())
}
