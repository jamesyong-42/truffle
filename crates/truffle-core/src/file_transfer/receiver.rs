//! Receive handler — background task that handles incoming file transfer
//! requests from other peers.
//!
//! Instead of auto-accepting, offers are forwarded through an offer channel
//! so the application can decide whether to accept or reject each transfer.
//! Pull requests are auto-served (same as the CLI version).

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{error, info, warn};

use crate::network::NetworkProvider;
use crate::node::Node;

use super::types::{
    FileOffer, FileTransferEvent, FtMessage, OfferDecision, OfferResponder, TransferDirection,
    TransferError, TransferProgress,
};

/// Spawn a background task that listens for incoming file transfer messages.
///
/// - **OFFER**: Creates a [`FileOffer`] + [`OfferResponder`] pair, sends them
///   on the `offer_tx` channel, and waits up to 60 seconds for a decision.
/// - **PULL_REQUEST**: Auto-serves the requested file (same as CLI).
/// - **ACCEPT / REJECT**: Ignored (handled by send/pull initiators).
pub fn spawn_receive_handler<N: NetworkProvider + 'static>(
    node: Arc<Node<N>>,
    offer_tx: mpsc::UnboundedSender<(FileOffer, OfferResponder)>,
    event_tx: broadcast::Sender<FileTransferEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = node.subscribe("ft");
        info!("File transfer receive handler started");

        loop {
            let msg = match rx.recv().await {
                Ok(m) => m,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("FT receive handler lagged, missed {n} messages");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
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
            let offer_tx = offer_tx.clone();
            let event_tx = event_tx.clone();

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
                            &offer_tx,
                            &event_tx,
                        )
                        .await
                        {
                            // Distinguish rejection from actual failures
                            match &e {
                                TransferError::Rejected(reason) => {
                                    info!(
                                        from = from.as_str(),
                                        file = file_name.as_str(),
                                        "File offer rejected: {reason}"
                                    );
                                    let _ = event_tx.send(FileTransferEvent::Rejected {
                                        token,
                                        file_name,
                                        reason: reason.clone(),
                                    });
                                }
                                _ => {
                                    error!(
                                        from = from.as_str(),
                                        file = file_name.as_str(),
                                        "Failed to receive file: {e}"
                                    );
                                    let _ = event_tx.send(FileTransferEvent::Failed {
                                        token,
                                        direction: TransferDirection::Receive,
                                        file_name,
                                        reason: e.to_string(),
                                    });
                                }
                            }
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
                            handle_pull_request(&node, &from, &path, &token, &event_tx).await
                        {
                            error!(
                                from = from.as_str(),
                                path = path.as_str(),
                                "Failed to serve file: {e}"
                            );
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

/// Handle an incoming OFFER: forward to offer channel, wait for decision,
/// then accept/reject accordingly.
async fn handle_incoming_offer<N: NetworkProvider + 'static>(
    node: &Node<N>,
    from: &str,
    file_name: &str,
    size: u64,
    sha256: &str,
    save_path: &str,
    token: &str,
    offer_tx: &mpsc::UnboundedSender<(FileOffer, OfferResponder)>,
    event_tx: &broadcast::Sender<FileTransferEvent>,
) -> Result<(), TransferError> {
    info!(
        from = from,
        file = file_name,
        size = size,
        "Received incoming file offer"
    );

    // Build the FileOffer
    let offer = FileOffer {
        from_peer: from.to_string(),
        from_name: from.to_string(), // Best we have — the peer ID
        file_name: file_name.to_string(),
        size,
        sha256: sha256.to_string(),
        suggested_path: save_path.to_string(),
        token: token.to_string(),
    };

    // Emit OfferReceived event (informational)
    let _ = event_tx.send(FileTransferEvent::OfferReceived(offer.clone()));

    // Create oneshot channel for the decision
    let (decision_tx, decision_rx) = oneshot::channel::<OfferDecision>();
    let responder = OfferResponder::new(decision_tx);

    // Send offer + responder to the offer channel
    offer_tx
        .send((offer, responder))
        .map_err(|_| TransferError::Protocol("Offer channel closed".to_string()))?;

    // Wait for decision with 60s timeout
    let decision = tokio::time::timeout(
        tokio::time::Duration::from_secs(60),
        decision_rx,
    )
    .await
    .map_err(|_| TransferError::Timeout)?
    .map_err(|_| TransferError::Protocol("Offer responder dropped without decision".to_string()))?;

    match decision {
        OfferDecision::Accept { save_path: dest } => {
            accept_and_receive(node, from, file_name, size, sha256, token, &dest, event_tx).await
        }
        OfferDecision::Reject { reason } => {
            // Send REJECT message to sender
            let reject = FtMessage::Reject {
                token: token.to_string(),
                reason: reason.clone(),
            };
            let reject_payload = serde_json::to_value(&reject)
                .map_err(|e| TransferError::Protocol(format!("Serialize error: {e}")))?;
            node.send_typed(from, "ft", "reject", &reject_payload)
                .await
                .map_err(|e| TransferError::Node(format!("Failed to send REJECT: {e}")))?;

            info!(
                from = from,
                file = file_name,
                reason = reason.as_str(),
                "Rejected file offer"
            );

            Err(TransferError::Rejected(reason))
        }
    }
}

/// Accept an incoming offer: open TCP listener, receive file streaming to
/// disk, verify SHA-256, and send ACK.
async fn accept_and_receive<N: NetworkProvider + 'static>(
    node: &Node<N>,
    from: &str,
    file_name: &str,
    size: u64,
    sha256: &str,
    token: &str,
    save_path: &str,
    event_tx: &broadcast::Sender<FileTransferEvent>,
) -> Result<(), TransferError> {
    let start = std::time::Instant::now();

    // Create parent directories
    if let Some(parent) = std::path::Path::new(save_path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Start TCP listener
    let mut listener = node
        .listen_tcp(0)
        .await
        .map_err(|e| TransferError::Node(format!("Failed to listen TCP: {e}")))?;

    // Send ACCEPT
    let accept = FtMessage::Accept {
        token: token.to_string(),
        tcp_port: listener.port,
    };
    let accept_payload = serde_json::to_value(&accept)
        .map_err(|e| TransferError::Protocol(format!("Serialize error: {e}")))?;
    node.send_typed(from, "ft", "accept", &accept_payload)
        .await
        .map_err(|e| TransferError::Node(format!("Failed to send ACCEPT: {e}")))?;

    info!(
        port = listener.port,
        "Sent ACCEPT, listening for TCP connection"
    );

    // Wait for TCP connection with 30s timeout
    let incoming = tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        listener.accept(),
    )
    .await
    .map_err(|_| TransferError::Timeout)?
    .ok_or_else(|| TransferError::Protocol("Listener closed before accepting".to_string()))?;

    let mut stream = incoming.stream;

    // Read header: [8-byte size][64-byte sha256_hex]
    let mut size_buf = [0u8; 8];
    stream.read_exact(&mut size_buf).await?;
    let file_size = u64::from_be_bytes(size_buf);

    let mut sha_buf = [0u8; 64];
    stream.read_exact(&mut sha_buf).await?;
    let received_sha = String::from_utf8_lossy(&sha_buf).to_string();

    // Verify the metadata matches the OFFER
    if received_sha != sha256 {
        return Err(TransferError::IntegrityError {
            expected: sha256.to_string(),
            actual: received_sha,
        });
    }

    if file_size != size {
        return Err(TransferError::Protocol(format!(
            "Size mismatch: offer said {size}, stream header says {file_size}"
        )));
    }

    // Stream file data to disk (instead of memory buffer)
    let temp_path = format!("{save_path}.truffle-tmp");
    let mut temp_file = tokio::fs::File::create(&temp_path).await?;
    let mut hasher = Sha256::new();
    let mut bytes_received: u64 = 0;
    let progress_start = std::time::Instant::now();
    let mut last_progress = std::time::Instant::now();
    let mut buf = vec![0u8; 64 * 1024];

    while bytes_received < file_size {
        let to_read = ((file_size - bytes_received) as usize).min(buf.len());
        let n = stream.read(&mut buf[..to_read]).await?;
        if n == 0 {
            tokio::fs::remove_file(&temp_path).await.ok();
            return Err(TransferError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("Connection closed after {bytes_received}/{file_size} bytes"),
            )));
        }
        hasher.update(&buf[..n]);
        tokio::io::AsyncWriteExt::write_all(&mut temp_file, &buf[..n]).await?;
        bytes_received += n as u64;

        // Throttle progress events to max 4/sec
        if last_progress.elapsed() >= std::time::Duration::from_millis(250) {
            let elapsed = progress_start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                bytes_received as f64 / elapsed
            } else {
                0.0
            };
            let _ = event_tx.send(FileTransferEvent::Progress(TransferProgress {
                token: token.to_string(),
                direction: TransferDirection::Receive,
                file_name: file_name.to_string(),
                bytes_transferred: bytes_received,
                total_bytes: file_size,
                speed_bps: speed,
            }));
            last_progress = std::time::Instant::now();
        }
    }

    // Flush temp file
    tokio::io::AsyncWriteExt::flush(&mut temp_file).await?;

    // Verify SHA-256
    let actual_sha = hex::encode(hasher.finalize());

    if actual_sha != sha256 {
        // Send NACK
        stream.write_all(&[0x00]).await?;
        // Clean up temp file
        tokio::fs::remove_file(&temp_path).await.ok();
        return Err(TransferError::IntegrityError {
            expected: sha256.to_string(),
            actual: actual_sha,
        });
    }

    // Resolve final path: if save_path is a directory, append the file name
    let final_path = {
        let p = std::path::Path::new(save_path);
        if p.is_dir() || save_path.ends_with('/') || save_path.ends_with('\\') {
            format!("{}/{}", save_path.trim_end_matches(['/', '\\']), file_name)
        } else {
            save_path.to_string()
        }
    };

    // Create parent directories for the final path
    if let Some(parent) = std::path::Path::new(&final_path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Move temp file to final path. Use rename first (fast, atomic on same
    // filesystem), fall back to copy+delete for cross-device moves.
    info!(
        temp = temp_path.as_str(),
        final_path = final_path.as_str(),
        "Moving temp file to final destination"
    );
    if let Err(rename_err) = tokio::fs::rename(&temp_path, &final_path).await {
        info!(
            err = %rename_err,
            "Rename failed, trying copy+delete fallback"
        );
        tokio::fs::copy(&temp_path, &final_path).await?;
        tokio::fs::remove_file(&temp_path).await.ok();
    }
    info!(
        final_path = final_path.as_str(),
        exists = std::path::Path::new(&final_path).exists(),
        "File save completed"
    );

    // Send ACK and flush
    stream.write_all(&[0x01]).await?;
    tokio::io::AsyncWriteExt::flush(&mut stream).await?;

    // Wait briefly for the ACK to propagate through the Go bridge.
    // The bridge uses bidirectional io.Copy — when we drop the stream,
    // the bridge may close both directions before the ACK byte has been
    // forwarded to the sender. This small delay ensures the ACK is delivered.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let elapsed = start.elapsed().as_secs_f64();
    info!(
        file = final_path.as_str(),
        bytes = file_size,
        elapsed_ms = (elapsed * 1000.0) as u64,
        "File received and verified"
    );

    // Emit completed event
    let _ = event_tx.send(FileTransferEvent::Completed {
        token: token.to_string(),
        direction: TransferDirection::Receive,
        file_name: file_name.to_string(),
        bytes_transferred: file_size,
        sha256: actual_sha,
        elapsed_secs: elapsed,
    });

    Ok(())
}

/// Handle a PULL_REQUEST: read file, send OFFER, wait for ACCEPT, stream via TCP.
async fn handle_pull_request<N: NetworkProvider + 'static>(
    node: &Node<N>,
    from: &str,
    path: &str,
    _token: &str,
    event_tx: &broadcast::Sender<FileTransferEvent>,
) -> Result<(), TransferError> {
    info!(from = from, path = path, "Processing PULL_REQUEST");

    // Read and hash the file
    let data = tokio::fs::read(path)
        .await
        .map_err(|e| TransferError::Io(e))?;
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

    // Send OFFER and wait for ACCEPT
    let offer = FtMessage::Offer {
        file_name: file_name.clone(),
        size,
        sha256: sha256.clone(),
        save_path: String::new(),
        token: offer_token.clone(),
        tcp_port: 0,
    };
    let offer_payload = serde_json::to_value(&offer)
        .map_err(|e| TransferError::Protocol(format!("Serialize error: {e}")))?;

    let _accept_port = crate::request_reply::send_and_wait(
        node,
        from,
        "ft",
        "offer",
        &offer_payload,
        std::time::Duration::from_secs(30),
        |msg| {
            if msg.from != from {
                return None;
            }
            let ft_msg: FtMessage = serde_json::from_value(msg.payload.clone()).ok()?;
            match ft_msg {
                FtMessage::Accept {
                    token: ref t,
                    tcp_port,
                } if *t == offer_token => Some(Ok(tcp_port)),
                FtMessage::Reject {
                    token: ref t,
                    reason,
                } if *t == offer_token => {
                    Some(Err(TransferError::Rejected(format!("Peer rejected: {reason}"))))
                }
                _ => None,
            }
        },
    )
    .await
    .map_err(|e| match e {
        crate::request_reply::RequestError::Timeout => TransferError::Timeout,
        crate::request_reply::RequestError::Send(e) => {
            TransferError::Node(format!("Failed to send OFFER: {e}"))
        }
        crate::request_reply::RequestError::ChannelClosed => {
            TransferError::Protocol("Channel closed".into())
        }
    })?
    .map_err(|e| e)?;

    // Open TCP to peer and stream the file
    let mut stream = node
        .open_tcp(from, _accept_port)
        .await
        .map_err(|e| {
            TransferError::Node(format!(
                "Failed to open TCP to {from}:{_accept_port}: {e}"
            ))
        })?;

    let start = std::time::Instant::now();

    // Write [size][sha256_hex][file_data]
    stream.write_all(&size.to_be_bytes()).await?;
    stream.write_all(sha256.as_bytes()).await?;

    let chunk_size = 64 * 1024;
    let mut offset = 0;
    let mut bytes_sent: u64 = 0;

    while offset < data.len() {
        let end = (offset + chunk_size).min(data.len());
        stream.write_all(&data[offset..end]).await?;
        bytes_sent += (end - offset) as u64;
        offset = end;

        let elapsed = start.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            bytes_sent as f64 / elapsed
        } else {
            0.0
        };

        // Emit progress event (best-effort)
        let _ = event_tx.send(FileTransferEvent::Progress(TransferProgress {
            token: offer_token.clone(),
            direction: TransferDirection::Send,
            file_name: file_name.clone(),
            bytes_transferred: bytes_sent,
            total_bytes: size,
            speed_bps: speed,
        }));
    }

    stream.flush().await?;

    // Read ACK
    let mut ack = [0u8; 1];
    stream.read_exact(&mut ack).await?;

    if ack[0] != 0x01 {
        return Err(TransferError::IntegrityError {
            expected: sha256,
            actual: "peer reported integrity failure".to_string(),
        });
    }

    let elapsed = start.elapsed().as_secs_f64();
    info!(path = path, bytes = size, "File served successfully");

    // Emit completed event
    let _ = event_tx.send(FileTransferEvent::Completed {
        token: offer_token,
        direction: TransferDirection::Send,
        file_name,
        bytes_transferred: size,
        sha256,
        elapsed_secs: elapsed,
    });

    Ok(())
}
