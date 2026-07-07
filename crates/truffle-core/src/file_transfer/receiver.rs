//! Receive handler — background task that handles incoming file transfer
//! requests from other peers.
//!
//! Instead of auto-accepting, offers are forwarded through an offer channel
//! so the application can decide whether to accept or reject each transfer.
//! Pull requests are served only when the canonicalized requested path lives
//! inside a configured pull root (see [`super::FileTransfer::add_pull_root`]);
//! with no roots configured all incoming PULL_REQUESTs are denied.

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

/// Reduce a peer-supplied file name to a safe base name.
///
/// A remote peer fully controls `file_name`/`save_path` in an OFFER, so these
/// must never be allowed to escape the receiver's chosen directory. This strips
/// every directory component (both `/` and `\`) and rejects empty, `.`, `..`,
/// or NUL-bearing names. Returns `None` when nothing safe remains.
fn safe_base_name(name: &str) -> Option<String> {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if last.is_empty() || last == "." || last == ".." || last.contains('\0') {
        return None;
    }
    Some(last.to_string())
}

/// Resolve the final on-disk destination for a received file.
///
/// When `save_path` is a directory (exists as one, or ends with a separator)
/// we append a **sanitized** base name derived from the peer-supplied
/// `file_name`, which prevents path-traversal / absolute-path escapes such as
/// `file_name = "../../../.ssh/authorized_keys"`. When `save_path` is an
/// explicit file path it was chosen by the local application, which owns that
/// decision.
fn resolve_dest_path(save_path: &str, file_name: &str) -> Result<String, TransferError> {
    let p = std::path::Path::new(save_path);
    let treat_as_dir = p.is_dir() || save_path.ends_with('/') || save_path.ends_with('\\');
    if treat_as_dir {
        let safe = safe_base_name(file_name).ok_or_else(|| {
            TransferError::Protocol(format!(
                "Rejected unsafe file name from peer: {file_name:?}"
            ))
        })?;
        Ok(format!(
            "{}/{}",
            save_path.trim_end_matches(['/', '\\']),
            safe
        ))
    } else {
        Ok(save_path.to_string())
    }
}

/// Authorize a peer-supplied PULL_REQUEST path against the allowlist.
///
/// Deny-by-default: an empty allowlist rejects everything. The path is
/// canonicalized (resolving symlinks and `..`) and must be a regular file
/// inside one of the canonicalized `roots`, no larger than `max_size`.
///
/// [`std::path::Path::starts_with`] is component-wise, so a root of `/shared`
/// does not match a sibling `/shared-evil`. Missing, non-file, and
/// outside-root cases all return the same "not shared" message to avoid
/// leaking an existence oracle to the requester.
fn authorize_pull_path(
    roots: &[std::path::PathBuf],
    requested: &str,
    max_size: u64,
) -> Result<std::path::PathBuf, TransferError> {
    if roots.is_empty() {
        return Err(TransferError::Rejected(
            "pull serving is not enabled on this peer".into(),
        ));
    }
    let canon = std::fs::canonicalize(requested)
        .map_err(|_| TransferError::Rejected("requested path is not shared".into()))?;
    if !roots.iter().any(|r| canon.starts_with(r)) {
        return Err(TransferError::Rejected(
            "requested path is not shared".into(),
        ));
    }
    let meta = std::fs::metadata(&canon)
        .map_err(|_| TransferError::Rejected("requested path is not shared".into()))?;
    if !meta.is_file() {
        return Err(TransferError::Rejected(
            "requested path is not shared".into(),
        ));
    }
    if meta.len() > max_size {
        return Err(TransferError::Rejected(format!(
            "file size {} exceeds max transfer size {max_size}",
            meta.len()
        )));
    }
    Ok(canon)
}

/// Spawn a background task that listens for incoming file transfer messages.
///
/// - **OFFER**: Creates a [`FileOffer`] + [`OfferResponder`] pair, sends them
///   on the `offer_tx` channel, and waits up to 60 seconds for a decision.
/// - **PULL_REQUEST**: Serves the requested file only if its canonicalized path
///   is inside a configured pull root (see [`super::FileTransfer::add_pull_root`]);
///   denied by default.
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
                            &node, &from, &file_name, size, &sha256, &save_path, &token, &offer_tx,
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
#[allow(clippy::too_many_arguments)]
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

    // M1: reject over-size offers before doing any work. The peer controls
    // `size`, so an unbounded value would otherwise let an auto-accepting node
    // be driven to disk exhaustion.
    let max_size = node
        .file_transfer_state
        .max_transfer_size
        .load(std::sync::atomic::Ordering::Relaxed);
    if size > max_size {
        return Err(TransferError::Protocol(format!(
            "Offered file size {size} exceeds max transfer size {max_size}"
        )));
    }

    // Build the FileOffer. `suggested_path` is peer-controlled, so we surface it
    // only as a sanitized base-name hint — never a path a naive integrator could
    // pass straight to `accept()` and have it escape their download directory.
    let offer = FileOffer {
        from_peer: from.to_string(),
        from_name: from.to_string(), // Best we have — the peer ID
        file_name: file_name.to_string(),
        size,
        sha256: sha256.to_string(),
        suggested_path: safe_base_name(save_path).unwrap_or_default(),
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
    let decision = tokio::time::timeout(tokio::time::Duration::from_secs(60), decision_rx)
        .await
        .map_err(|_| TransferError::Timeout)?
        .map_err(|_| {
            TransferError::Protocol("Offer responder dropped without decision".to_string())
        })?;

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
#[allow(clippy::too_many_arguments)]
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

    // F6: resolve the final destination SAFELY up front. When `save_path` is a
    // directory we append a sanitized base name from the peer's `file_name`,
    // blocking path-traversal / absolute-path escapes.
    let final_path = resolve_dest_path(save_path, file_name)?;

    // Create parent directories for the resolved destination.
    if let Some(parent) = std::path::Path::new(&final_path).parent() {
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
    let incoming = tokio::time::timeout(tokio::time::Duration::from_secs(30), listener.accept())
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

    // M1: defense-in-depth — enforce the max size against the stream header too.
    let max_size = node
        .file_transfer_state
        .max_transfer_size
        .load(std::sync::atomic::Ordering::Relaxed);
    if file_size > max_size {
        return Err(TransferError::Protocol(format!(
            "File size {file_size} exceeds max transfer size {max_size}"
        )));
    }

    // Stream file data to disk (instead of memory buffer)
    let temp_path = format!("{final_path}.truffle-tmp");
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

    // (final_path was resolved and its parent created before streaming.)

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
    token: &str,
    event_tx: &broadcast::Sender<FileTransferEvent>,
) -> Result<(), TransferError> {
    info!(from = from, path = path, "Processing PULL_REQUEST");

    // Authorize the peer-supplied path against the deny-by-default pull-root
    // allowlist BEFORE any filesystem I/O. This blocks arbitrary absolute-path
    // reads (e.g. `/etc/hosts`) and enforces the max transfer size + regular-file
    // check on the serving side.
    let max_size = node
        .file_transfer_state
        .max_transfer_size
        .load(std::sync::atomic::Ordering::Relaxed);
    let roots = node.file_transfer_state.pull_roots.read().unwrap().clone();
    let serve_path = match authorize_pull_path(&roots, path, max_size) {
        Ok(p) => p,
        Err(e) => {
            warn!(from = from, path = path, "Denying PULL_REQUEST: {e}");
            // Best-effort Reject so the requester fails fast instead of timing out.
            let reject = FtMessage::Reject {
                token: token.to_string(),
                reason: "pull denied by peer".to_string(),
            };
            if let Ok(payload) = serde_json::to_value(&reject) {
                if let Err(send_err) = node.send_typed(from, "ft", "reject", &payload).await {
                    warn!(from = from, "Failed to send pull REJECT: {send_err}");
                }
            }
            return Err(e);
        }
    };

    // Read and hash the file
    let data = tokio::fs::read(&serve_path)
        .await
        .map_err(TransferError::Io)?;
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
                } if *t == offer_token => Some(Err(TransferError::Rejected(format!(
                    "Peer rejected: {reason}"
                )))),
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
    })??;

    // Open TCP to peer and stream the file
    let mut stream = node.open_tcp(from, _accept_port).await.map_err(|e| {
        TransferError::Node(format!("Failed to open TCP to {from}:{_accept_port}: {e}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    // Extra imports for the behavioral mock-node test (`handle_pull_request_*`).
    use crate::envelope::codec::JsonCodec;
    use crate::envelope::EnvelopeCodec;
    use crate::network::*;
    use crate::session::PeerRegistry;
    use crate::transport::websocket::WebSocketTransport;
    use crate::transport::WsConfig;
    use std::time::Duration;

    #[test]
    fn safe_base_name_strips_directories() {
        assert_eq!(safe_base_name("file.txt").as_deref(), Some("file.txt"));
        assert_eq!(
            safe_base_name("../../etc/passwd").as_deref(),
            Some("passwd")
        );
        assert_eq!(safe_base_name("/abs/path/x").as_deref(), Some("x"));
        assert_eq!(
            safe_base_name("..\\..\\win.exe").as_deref(),
            Some("win.exe")
        );
    }

    #[test]
    fn safe_base_name_rejects_unsafe() {
        assert_eq!(safe_base_name(""), None);
        assert_eq!(safe_base_name("."), None);
        assert_eq!(safe_base_name(".."), None);
        assert_eq!(safe_base_name("../.."), None); // final component is ".."
        assert_eq!(safe_base_name("dir/"), None); // trailing separator => empty base
    }

    #[test]
    fn resolve_dest_contains_traversal_into_directory() {
        // A directory destination + a malicious peer file_name must stay inside
        // the directory (F6): the "../.." escape is stripped to a base name.
        let got = resolve_dest_path("/downloads/", "../../../.ssh/authorized_keys").unwrap();
        assert_eq!(got, "/downloads/authorized_keys");
        assert!(!got.contains(".."));
    }

    #[test]
    fn resolve_dest_rejects_dotdot_filename() {
        assert!(resolve_dest_path("/downloads/", "..").is_err());
    }

    #[test]
    fn resolve_dest_passes_through_explicit_file() {
        // A non-directory explicit path chosen by the local app is used as-is.
        assert_eq!(
            resolve_dest_path("/tmp/truffle-explicit-file.bin", "ignored").unwrap(),
            "/tmp/truffle-explicit-file.bin"
        );
    }

    // ── PULL_REQUEST authorization (pull-arbitrary-read fix) ──────────

    #[test]
    fn pull_denied_when_no_roots() {
        // Deny-by-default: with no roots registered, even a real file is denied.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, b"hi").unwrap();
        let err = authorize_pull_path(&[], file.to_str().unwrap(), u64::MAX).unwrap_err();
        assert!(matches!(err, TransferError::Rejected(_)));
    }

    #[test]
    fn pull_allowlisted_file_authorized() {
        // A regular file inside a registered root is served (returns its canonical path).
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file = root.join("f.txt");
        std::fs::write(&file, b"hi").unwrap();
        let got = authorize_pull_path(&[root], file.to_str().unwrap(), u64::MAX).unwrap();
        assert_eq!(got, std::fs::canonicalize(&file).unwrap());
    }

    #[test]
    fn pull_outside_root_rejected() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let root_a = std::fs::canonicalize(dir_a.path()).unwrap();
        let file_b = dir_b.path().join("secret.txt");
        std::fs::write(&file_b, b"secret").unwrap();

        // (1) A file that lives entirely outside the root is rejected.
        let err =
            authorize_pull_path(&[root_a.clone()], file_b.to_str().unwrap(), u64::MAX).unwrap_err();
        assert!(matches!(err, TransferError::Rejected(_)));

        // (2) A `..` escape that climbs out of the root is rejected: canonicalize
        //     resolves the `..`, so the resulting path fails the starts_with check.
        let dir_b_name = dir_b.path().file_name().unwrap().to_str().unwrap();
        let dotdot = format!("{}/../{}/secret.txt", dir_a.path().display(), dir_b_name);
        let err = authorize_pull_path(&[root_a], &dotdot, u64::MAX).unwrap_err();
        assert!(matches!(err, TransferError::Rejected(_)));
    }

    #[test]
    fn pull_prefix_collision_rejected() {
        // "shared-evil" shares a string prefix with the "shared" root but is a
        // different path component — Path::starts_with must not be fooled.
        let base = tempfile::tempdir().unwrap();
        let shared = base.path().join("shared");
        let evil = base.path().join("shared-evil");
        std::fs::create_dir(&shared).unwrap();
        std::fs::create_dir(&evil).unwrap();
        let evil_file = evil.join("f.txt");
        std::fs::write(&evil_file, b"x").unwrap();

        let root = std::fs::canonicalize(&shared).unwrap();
        let err = authorize_pull_path(&[root], evil_file.to_str().unwrap(), u64::MAX).unwrap_err();
        assert!(matches!(err, TransferError::Rejected(_)));
    }

    #[cfg(unix)]
    #[test]
    fn pull_symlink_escape_rejected() {
        // A symlink inside the root pointing at a file outside it must not leak
        // the target: canonicalize resolves the link to its out-of-root target.
        let root_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root_dir.path()).unwrap();

        let secret = outside_dir.path().join("secret.txt");
        std::fs::write(&secret, b"secret").unwrap();

        let link = root.join("link.txt");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        let err = authorize_pull_path(&[root], link.to_str().unwrap(), u64::MAX).unwrap_err();
        assert!(matches!(err, TransferError::Rejected(_)));
    }

    #[test]
    fn pull_oversize_file_rejected() {
        // A 4-byte file under the root is rejected when max_size is 3.
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file = root.join("big.bin");
        std::fs::write(&file, b"1234").unwrap();
        let err = authorize_pull_path(&[root], file.to_str().unwrap(), 3).unwrap_err();
        assert!(matches!(err, TransferError::Rejected(_)));
    }

    // ── Mock network provider (per-module copy, mirrors request_reply.rs) ──

    struct MockNetworkProvider {
        identity: NodeIdentity,
        local_addr: PeerAddr,
        peer_event_tx: tokio::sync::broadcast::Sender<NetworkPeerEvent>,
        mock_peers: Arc<tokio::sync::RwLock<Vec<NetworkPeer>>>,
    }

    impl MockNetworkProvider {
        fn new(id: &str) -> Self {
            let (peer_event_tx, _) = tokio::sync::broadcast::channel(64);
            Self {
                identity: NodeIdentity {
                    app_id: "test".to_string(),
                    device_id: id.to_string(),
                    device_name: format!("Test Node {id}"),
                    tailscale_hostname: format!("truffle-test-{id}"),
                    tailscale_id: id.to_string(),
                    dns_name: None,
                    ip: Some("127.0.0.1".parse().unwrap()),
                },
                local_addr: PeerAddr {
                    ip: Some("127.0.0.1".parse().unwrap()),
                    hostname: format!("truffle-test-{id}"),
                    dns_name: None,
                },
                peer_event_tx,
                mock_peers: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            }
        }

        fn event_sender(&self) -> tokio::sync::broadcast::Sender<NetworkPeerEvent> {
            self.peer_event_tx.clone()
        }
    }

    impl NetworkProvider for MockNetworkProvider {
        fn local_identity(&self) -> NodeIdentity {
            self.identity.clone()
        }
        fn local_addr(&self) -> PeerAddr {
            self.local_addr.clone()
        }
        fn peer_events(&self) -> tokio::sync::broadcast::Receiver<NetworkPeerEvent> {
            self.peer_event_tx.subscribe()
        }

        async fn start(&mut self) -> Result<(), NetworkError> {
            Ok(())
        }
        async fn stop(&self) -> Result<(), NetworkError> {
            Ok(())
        }
        async fn peers(&self) -> Vec<NetworkPeer> {
            self.mock_peers.read().await.clone()
        }
        async fn dial_tcp(
            &self,
            _addr: &str,
            _port: u16,
        ) -> Result<tokio::net::TcpStream, NetworkError> {
            Err(NetworkError::DialFailed("mock".into()))
        }
        async fn listen_tcp(&self, _port: u16) -> Result<NetworkTcpListener, NetworkError> {
            Err(NetworkError::ListenFailed("mock".into()))
        }
        async fn unlisten_tcp(&self, _port: u16) -> Result<(), NetworkError> {
            Ok(())
        }
        async fn bind_udp(&self, _port: u16) -> Result<NetworkUdpSocket, NetworkError> {
            Err(NetworkError::NotRunning)
        }
        async fn ping(&self, _addr: &str) -> Result<PingResult, NetworkError> {
            Ok(PingResult {
                latency: Duration::from_millis(1),
                connection: "direct".to_string(),
                peer_addr: None,
            })
        }
        async fn health(&self) -> HealthInfo {
            HealthInfo {
                state: "running".to_string(),
                healthy: true,
                ..Default::default()
            }
        }
    }

    fn ws_config(port: u16) -> WsConfig {
        WsConfig {
            port,
            ping_interval: Duration::from_secs(300),
            pong_timeout: Duration::from_secs(300),
            ..Default::default()
        }
    }

    async fn random_port() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    }

    async fn make_test_node(
        id: &str,
        ws_port: u16,
    ) -> (
        Node<MockNetworkProvider>,
        tokio::sync::broadcast::Sender<NetworkPeerEvent>,
    ) {
        let provider = MockNetworkProvider::new(id);
        let event_tx = provider.event_sender();
        let network = Arc::new(provider);
        let ws_transport = Arc::new(WebSocketTransport::new(network.clone(), ws_config(ws_port)));
        let session = Arc::new(PeerRegistry::new(network.clone(), ws_transport));
        session.start().await;

        let codec: Arc<dyn EnvelopeCodec> = Arc::new(JsonCodec);
        let node = Node::from_parts(network, session, codec);
        (node, event_tx)
    }

    /// Regression test for the pull-arbitrary-read finding: a PULL_REQUEST for a
    /// real, readable file is denied when the node has registered no pull roots,
    /// and the denial happens WITHOUT any network/file transfer taking place.
    ///
    /// Before the fix this returned `TransferError::Node(...)` (the file was read,
    /// then the OFFER send to the unknown peer failed); after the fix it is a
    /// `Rejected` produced before any file I/O.
    #[tokio::test]
    async fn handle_pull_request_denied_by_default() {
        let (node, _event_tx) = make_test_node("node-a", random_port().await).await;

        // A real, readable file the attacker would love to exfiltrate.
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();
        let secret_path = secret.to_str().unwrap();

        let (event_tx, _rx) = tokio::sync::broadcast::channel(16);
        let err = handle_pull_request(&node, "peer-x", secret_path, "tok-1", &event_tx)
            .await
            .unwrap_err();

        assert!(
            matches!(err, TransferError::Rejected(_)),
            "expected Rejected, got {err}"
        );
    }
}
