use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use hyper::Request;
use tokio::net::TcpStream;
use tracing::{info, warn};

use super::manager::FileTransferManager;
use super::progress::{ProgressCallback, ProgressReader};
use super::types::*;

/// Type alias for a custom dial function (e.g. dialing through Tailscale).
pub type DialFn =
    Arc<dyn Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<TcpStream>> + Send>> + Send + Sync>;

impl FileTransferManager {
    /// Send a file to a remote receiver via HTTP PUT.
    /// Ports Go's sendFile. Runs as a spawned task.
    pub async fn send_file(
        self: &Arc<Self>,
        transfer: Arc<Transfer>,
        target_addr: String,
        dial_fn: DialFn,
    ) {
        let file_path = match &transfer.file_path {
            Some(p) => p.clone(),
            None => {
                self.fail_transfer(&transfer, error_codes::FILE_OPEN_ERROR, "no file path");
                return;
            }
        };

        // Open the file
        let file = match tokio::fs::File::open(&file_path).await {
            Ok(f) => f,
            Err(e) => {
                self.fail_transfer(&transfer, error_codes::FILE_OPEN_ERROR, &e.to_string());
                return;
            }
        };

        // Query resume offset via HEAD
        let offset = match self
            .query_resume_offset(&transfer, &target_addr, &dial_fn)
            .await
        {
            Ok(o) => o,
            Err(e) => {
                self.fail_transfer(
                    &transfer,
                    error_codes::RESUME_QUERY_ERROR,
                    &e.to_string(),
                );
                return;
            }
        };

        // Seek if resuming
        let mut file = file;
        if offset > 0 {
            use tokio::io::AsyncSeekExt;
            if let Err(e) = file.seek(std::io::SeekFrom::Start(offset as u64)).await {
                self.fail_transfer(&transfer, error_codes::SEEK_ERROR, &e.to_string());
                return;
            }
            info!("[FileTransfer] Resuming {} from offset {}", transfer.id, offset);
        }

        transfer
            .bytes_transferred
            .store(offset, Ordering::Relaxed);
        {
            let mut started = transfer.started_at.lock().await;
            *started = Some(Instant::now());
        }

        // Build progress-reporting reader
        let mgr = Arc::clone(self);
        let transfer_for_progress = Arc::clone(&transfer);
        let progress_callback: ProgressCallback = Arc::new(move |bytes_read: i64| {
            mgr.emit_progress(&transfer_for_progress, bytes_read);
        });

        let progress_reader = ProgressReader::new(
            file,
            transfer.file.size,
            offset,
            progress_callback,
            self.config(),
        );

        // Build the streaming body
        let content_length = transfer.file.size - offset;
        let body_reader = tokio_util::io::ReaderStream::new(progress_reader);
        let body = http_body_util::StreamBody::new(
            tokio_stream::StreamExt::map(body_reader, |result| {
                result.map(hyper::body::Frame::data)
            }),
        );

        // Build HTTP request
        let uri = format!("http://{}/transfer/{}", target_addr, transfer.id);
        let mut builder = Request::builder()
            .method(hyper::Method::PUT)
            .uri(&uri)
            .header("x-transfer-token", &transfer.token)
            .header("x-file-name", &transfer.file.name)
            .header("x-file-sha256", &transfer.file.sha256)
            .header("content-length", content_length.to_string());

        if offset > 0 {
            builder = builder.header(
                "content-range",
                format!(
                    "bytes {}-{}/{}",
                    offset,
                    transfer.file.size - 1,
                    transfer.file.size
                ),
            );
        }

        let req = match builder.body(body) {
            Ok(r) => r,
            Err(e) => {
                self.fail_transfer(&transfer, error_codes::REQUEST_ERROR, &e.to_string());
                return;
            }
        };

        // Dial through Tailscale and send via hyper
        let stream = match (dial_fn)(&target_addr).await {
            Ok(s) => s,
            Err(e) => {
                if transfer.cancel_token.is_cancelled() {
                    return;
                }
                self.fail_transfer(&transfer, error_codes::SEND_ERROR, &e.to_string());
                return;
            }
        };

        let io = hyper_util::rt::TokioIo::new(stream);

        let (mut sender, conn) = match hyper::client::conn::http1::handshake(io).await {
            Ok(parts) => parts,
            Err(e) => {
                if transfer.cancel_token.is_cancelled() {
                    return;
                }
                self.fail_transfer(&transfer, error_codes::SEND_ERROR, &e.to_string());
                return;
            }
        };

        // Spawn connection driver
        let cancel_token = transfer.cancel_token.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = conn => {
                    if let Err(e) = result {
                        warn!("[FileTransfer] Connection error: {e}");
                    }
                }
                _ = cancel_token.cancelled() => {}
            }
        });

        info!(
            "[FileTransfer] Sending {} ({} bytes, offset {}) to {}",
            transfer.id, transfer.file.size, offset, target_addr
        );

        let resp = match sender.send_request(req).await {
            Ok(r) => r,
            Err(e) => {
                if transfer.cancel_token.is_cancelled() {
                    return;
                }
                self.fail_transfer(&transfer, error_codes::SEND_ERROR, &e.to_string());
                return;
            }
        };

        let status = resp.status();
        if status != hyper::StatusCode::OK {
            // Try to parse JSON error response
            let body_bytes = match http_body_util::BodyExt::collect(resp.into_body()).await {
                Ok(collected) => collected.to_bytes(),
                Err(_) => Bytes::new(),
            };

            if let Ok(err_resp) =
                serde_json::from_slice::<HttpErrorResponse>(&body_bytes)
            {
                // Special case: receiver says TRANSFER_NOT_ACCEPTING after full send
                if err_resp.code == error_codes::TRANSFER_NOT_ACCEPTING
                    && offset + content_length >= transfer.file.size
                {
                    info!(
                        "[FileTransfer] {}: receiver returned TRANSFER_NOT_ACCEPTING after full send - treating as success",
                        transfer.id
                    );
                    transfer.set_state(TransferState::Completed);
                    let duration_ms = {
                        let guard = transfer.started_at.lock().await;
                        guard.map(|s| s.elapsed().as_millis() as u64).unwrap_or(0)
                    };
                    self.emit_event_internal(FileTransferEvent::Complete {
                        transfer_id: transfer.id.clone(),
                        sha256: transfer.file.sha256.clone(),
                        size: transfer.file.size,
                        duration_ms,
                        direction: "send".to_string(),
                        path: None,
                    });
                    return;
                }
                self.fail_transfer(
                    &transfer,
                    &err_resp.code,
                    &format!("HTTP {status}: {}", err_resp.message),
                );
            } else {
                let body_str = String::from_utf8_lossy(&body_bytes);
                self.fail_transfer(
                    &transfer,
                    error_codes::REMOTE_ERROR,
                    &format!("HTTP {status}: {body_str}"),
                );
            }
            return;
        }

        // Transfer complete
        transfer.set_state(TransferState::Completed);

        let duration_ms = {
            let guard = transfer.started_at.lock().await;
            guard.map(|s| s.elapsed().as_millis() as u64).unwrap_or(0)
        };

        self.emit_event_internal(FileTransferEvent::Complete {
            transfer_id: transfer.id.clone(),
            sha256: transfer.file.sha256.clone(),
            size: transfer.file.size,
            duration_ms,
            direction: "send".to_string(),
            path: None,
        });

        info!(
            "[FileTransfer] Send complete: {} ({} bytes)",
            transfer.id, transfer.file.size
        );
    }

    /// Query the receiver's resume offset via HEAD.
    async fn query_resume_offset(
        &self,
        transfer: &Transfer,
        target_addr: &str,
        dial_fn: &DialFn,
    ) -> Result<i64, String> {
        let stream = (dial_fn)(target_addr)
            .await
            .map_err(|e| e.to_string())?;

        let io = hyper_util::rt::TokioIo::new(stream);

        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| e.to_string())?;

        tokio::spawn(async move {
            let _ = conn.await;
        });

        let uri = format!("http://{}/transfer/{}", target_addr, transfer.id);
        let req = Request::builder()
            .method(hyper::Method::HEAD)
            .uri(&uri)
            .header("x-transfer-token", &transfer.token)
            .body(http_body_util::Empty::<Bytes>::new())
            .map_err(|e| e.to_string())?;

        let resp = sender
            .send_request(req)
            .await
            .map_err(|e| e.to_string())?;

        match resp.status().as_u16() {
            200 => {}
            403 => return Err("resume query: forbidden (invalid token)".to_string()),
            404 => {
                return Err("resume query: transfer not registered on receiver".to_string())
            }
            other => {
                return Err(format!("resume query: unexpected status {other}"));
            }
        }

        let offset_str = resp
            .headers()
            .get("upload-offset")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("0");

        offset_str
            .parse::<i64>()
            .map_err(|_| format!("invalid Upload-Offset header: {offset_str:?}"))
    }

    /// Internal event emitter (avoids name clash with public emit_event in manager).
    fn emit_event_internal(&self, event: FileTransferEvent) {
        let _ = self.event_tx.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dial_fn_type_compiles() {
        // Just verify the DialFn type alias compiles
        let _: Option<DialFn> = None;
    }
}
