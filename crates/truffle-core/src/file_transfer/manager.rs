use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::hasher::{hash_file, hash_partial_file, HashingReader};
use super::progress::{ProgressCallback, ProgressReader};
use super::receiver::json_error;
use super::resume::write_partial_meta;
use super::types::*;

/// FileTransferManager manages file transfers over the mesh network.
///
/// Ports Go's TransferManager. Key differences:
/// - Uses tokio RwLock instead of sync.RWMutex
/// - Atomic counter for recv semaphore instead of channel
/// - mpsc channel for events instead of IPC protocol
/// - CancellationToken for shutdown instead of context.CancelFunc
pub struct FileTransferManager {
    config: FileTransferConfig,

    transfers: RwLock<HashMap<TransferID, Arc<Transfer>>>,

    /// Atomic counter for concurrent recv semaphore (replaces Go's channel semaphore).
    recv_active: AtomicI32,

    /// Event channel for emitting file transfer events to the adapter layer.
    pub(crate) event_tx: mpsc::UnboundedSender<FileTransferEvent>,

    /// Shutdown token.
    shutdown_token: CancellationToken,
}

impl FileTransferManager {
    pub fn new(
        config: FileTransferConfig,
        event_tx: mpsc::UnboundedSender<FileTransferEvent>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            transfers: RwLock::new(HashMap::new()),
            recv_active: AtomicI32::new(0),
            event_tx,
            shutdown_token: CancellationToken::new(),
        })
    }

    pub fn config(&self) -> &FileTransferConfig {
        &self.config
    }

    pub fn shutdown_token(&self) -> &CancellationToken {
        &self.shutdown_token
    }

    // --- Transfer registry ---

    /// Look up a transfer by ID, validating the token.
    /// Token must be 64 hex chars.
    pub fn get_transfer(&self, id: &str, token: &str) -> Result<Arc<Transfer>, String> {
        // Validate token format: 64 hex chars
        if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("invalid token format for transfer: {id}"));
        }

        let transfers = self.transfers.blocking_read();
        let t = transfers
            .get(id)
            .ok_or_else(|| format!("transfer not found: {id}"))?;

        // Constant-time comparison for token
        use subtle::ConstantTimeEq;
        if t.token.as_bytes().ct_eq(token.as_bytes()).into() {
            Ok(Arc::clone(t))
        } else {
            Err(format!("invalid token for transfer: {id}"))
        }
    }

    /// Register a transfer for receiving.
    pub async fn register_receive(
        &self,
        id: TransferID,
        file: FileInfo,
        token: String,
        save_path: String,
    ) -> Result<(), String> {
        let cleaned = validate_save_path(&save_path)?;
        let transfer = Transfer::new_with_save_path(id.clone(), file, token, cleaned);

        let mut transfers = self.transfers.write().await;
        transfers.insert(id.clone(), transfer);
        info!("[FileTransfer] Registered receive for {id}");
        Ok(())
    }

    /// Register a transfer for sending and start the send task.
    pub async fn register_send(
        &self,
        id: TransferID,
        file: FileInfo,
        token: String,
        file_path: String,
    ) -> Arc<Transfer> {
        let transfer = Transfer::new_sender(id.clone(), file, token, file_path);

        let mut transfers = self.transfers.write().await;
        transfers.insert(id.clone(), Arc::clone(&transfer));
        transfer
    }

    /// Remove a transfer from the registry.
    pub async fn remove_transfer(&self, id: &str) {
        let mut transfers = self.transfers.write().await;
        transfers.remove(id);
    }

    /// Cancel a transfer: set state, fire token, emit event.
    pub async fn cancel_transfer(&self, id: &str) {
        let transfers = self.transfers.read().await;
        if let Some(t) = transfers.get(id) {
            t.cancel();
            self.emit_event(FileTransferEvent::Cancelled {
                transfer_id: id.to_string(),
            });
        }
    }

    /// List all active transfers.
    pub async fn list_transfers(&self) -> Vec<TransferInfo> {
        let transfers = self.transfers.read().await;
        transfers
            .values()
            .map(|t| TransferInfo {
                id: t.id.clone(),
                state: t.state().to_string(),
                file: t.file.clone(),
                direction: t.direction.to_string(),
                bytes_transferred: t.bytes_transferred.load(Ordering::Relaxed),
            })
            .collect()
    }

    // --- Semaphore ---

    /// Try to acquire a recv slot. Returns false if at capacity.
    pub fn try_acquire_recv_semaphore(&self) -> bool {
        if self.config.max_concurrent_recv == 0 {
            return true; // unlimited
        }
        loop {
            let current = self.recv_active.load(Ordering::SeqCst);
            if current >= self.config.max_concurrent_recv as i32 {
                return false;
            }
            if self
                .recv_active
                .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Release a recv slot.
    pub fn release_recv_semaphore(&self) {
        self.recv_active.fetch_sub(1, Ordering::SeqCst);
    }

    // --- File preparation ---

    /// Stat + hash a local file, emitting progress events.
    pub async fn prepare_file(&self, transfer_id: &str, file_path: &str) {
        let tid = transfer_id.to_string();
        let path = file_path.to_string();

        // Stat
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) => {
                self.emit_event(FileTransferEvent::Error {
                    transfer_id: tid,
                    code: error_codes::FILE_NOT_FOUND.to_string(),
                    message: e.to_string(),
                    resumable: false,
                });
                return;
            }
        };

        if metadata.is_dir() {
            self.emit_event(FileTransferEvent::Error {
                transfer_id: tid,
                code: error_codes::IS_DIRECTORY.to_string(),
                message: "cannot transfer directories".to_string(),
                resumable: false,
            });
            return;
        }

        let file_size = metadata.len() as i64;
        if file_size > self.config.max_file_size {
            self.emit_event(FileTransferEvent::Error {
                transfer_id: tid,
                code: error_codes::FILE_TOO_LARGE.to_string(),
                message: format!(
                    "file size {} exceeds limit {}",
                    file_size, self.config.max_file_size
                ),
                resumable: false,
            });
            return;
        }

        let file_name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // Hash with progress
        let event_tx = self.event_tx.clone();
        let tid_clone = tid.clone();
        let total = file_size;

        let progress_fn = move |bytes_hashed: i64| {
            let percent = if total > 0 {
                bytes_hashed as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            let _ = event_tx.send(FileTransferEvent::PreparingProgress {
                transfer_id: tid_clone.clone(),
                bytes_hashed,
                total_bytes: total,
                percent,
            });
        };

        match hash_file(&path, file_size, Some(&progress_fn)).await {
            Ok(hash) => {
                self.emit_event(FileTransferEvent::Prepared {
                    transfer_id: tid,
                    name: file_name,
                    size: file_size,
                    sha256: hash,
                });
            }
            Err(e) => {
                self.emit_event(FileTransferEvent::Error {
                    transfer_id: tid,
                    code: error_codes::HASH_ERROR.to_string(),
                    message: e.to_string(),
                    resumable: false,
                });
            }
        }
    }

    // --- Receive path ---

    /// Core streaming receive: writes body to .partial file with SHA-256 and progress.
    /// Called from receiver.rs handle_put after all validation is done.
    pub async fn receive_file_bytes(
        &self,
        transfer: &Arc<Transfer>,
        body: Body,
        offset: i64,
        expected_len: i64,
    ) -> Result<(), Response> {
        let save_path = transfer.save_path.as_deref().unwrap_or("");
        let partial_path = format!("{save_path}.partial");

        // Create parent directory
        if let Some(parent) = Path::new(&partial_path).parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                self.fail_transfer(transfer, error_codes::DISK_ERROR, &e.to_string());
                json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    error_codes::DISK_ERROR,
                    "failed to create directory",
                )
            })?;
        }

        // Open or create partial file
        let mut file = if offset > 0 {
            let f = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .open(&partial_path)
                .await
                .map_err(|e| {
                    self.fail_transfer(transfer, error_codes::DISK_ERROR, &e.to_string());
                    json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        error_codes::DISK_ERROR,
                        "failed to open partial file",
                    )
                })?;

            // Seek to offset
            use tokio::io::AsyncSeekExt;
            let mut f = f;
            f.seek(std::io::SeekFrom::Start(offset as u64))
                .await
                .map_err(|e: std::io::Error| {
                    self.fail_transfer(transfer, error_codes::DISK_ERROR, &e.to_string());
                    json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        error_codes::DISK_ERROR,
                        "failed to seek in partial file",
                    )
                })?;
            f
        } else {
            tokio::fs::File::create(&partial_path)
                .await
                .map_err(|e| {
                    self.fail_transfer(transfer, error_codes::DISK_ERROR, &e.to_string());
                    json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        error_codes::DISK_ERROR,
                        "failed to create file",
                    )
                })?
        };

        // Write partial meta for resume
        if let Err(e) = write_partial_meta(save_path, &transfer.id, &transfer.file, offset).await {
            warn!("[FileTransfer] Failed to write manifest: {e}");
        }

        // Build SHA-256 hasher
        let mut sha_hasher = Sha256::new();

        // If resuming, re-hash existing partial data to rebuild SHA-256 state
        if offset > 0 {
            let hash_start = Instant::now();
            if let Err(e) = hash_partial_file(&partial_path, offset, &mut sha_hasher).await {
                self.fail_transfer(transfer, error_codes::HASH_ERROR, &e.to_string());
                return Err(json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    error_codes::HASH_ERROR,
                    "failed to hash partial file",
                ));
            }
            info!(
                "[FileTransfer] Re-hashed {} bytes of partial {} in {:?}",
                offset,
                transfer.id,
                hash_start.elapsed()
            );
        }

        // Convert axum Body to AsyncRead via StreamReader
        let body_stream = http_body_util::BodyStream::new(body);
        let body_stream = tokio_stream::StreamExt::map(body_stream, |result| {
            result
                .map(|frame| {
                    frame
                        .into_data()
                        .unwrap_or_default()
                })
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        });
        let body_reader = tokio_util::io::StreamReader::new(body_stream);

        // Wrap with HashingReader (feeds sha256) + ProgressReader
        let hashing_reader = HashingReader::new(body_reader);

        let transfer_clone = Arc::clone(transfer);
        let event_tx = self.event_tx.clone();
        let config = self.config.clone();
        let total = transfer.file.size;
        let direction = transfer.direction.to_string();

        let progress_callback: ProgressCallback = Arc::new(move |bytes_transferred: i64| {
            transfer_clone
                .bytes_transferred
                .store(bytes_transferred, Ordering::Relaxed);

            let started_at = {
                // try_lock to avoid blocking hot path
                if let Ok(guard) = transfer_clone.started_at.try_lock() {
                    *guard
                } else {
                    None
                }
            };

            let elapsed = started_at
                .map(|s| s.elapsed().as_secs_f64())
                .unwrap_or(0.0);

            let speed = if elapsed > 0.0 {
                bytes_transferred as f64 / elapsed
            } else {
                0.0
            };

            let percent = if total > 0 {
                bytes_transferred as f64 / total as f64 * 100.0
            } else {
                0.0
            };

            let eta = if speed > 0.0 {
                (total - bytes_transferred) as f64 / speed
            } else {
                0.0
            };

            let _ = event_tx.send(FileTransferEvent::Progress {
                transfer_id: transfer_clone.id.clone(),
                bytes_transferred,
                total_bytes: total,
                percent,
                bytes_per_second: speed,
                eta,
                direction: direction.clone(),
            });
        });

        let mut progress_reader =
            ProgressReader::new(hashing_reader, total, offset, progress_callback, &config);

        // Stream body to file with limited read
        let mut written: i64 = 0;
        let mut buf = vec![0u8; 64 * 1024]; // 64KB buffer

        loop {
            // Check cancellation
            if transfer.is_cancelled() {
                // Clean up partial files
                let _ = tokio::fs::remove_file(&partial_path).await;
                let _ = tokio::fs::remove_file(format!("{save_path}.partial.meta")).await;
                return Ok(());
            }

            let remaining = expected_len - written;
            if remaining <= 0 {
                break;
            }

            let to_read = std::cmp::min(remaining as usize, buf.len());
            match progress_reader.read(&mut buf[..to_read]).await {
                Ok(0) => break,
                Ok(n) => {
                    use tokio::io::AsyncWriteExt;
                    if let Err(e) = file.write_all(&buf[..n]).await {
                        if transfer.is_cancelled() {
                            let _ = tokio::fs::remove_file(&partial_path).await;
                            let _ =
                                tokio::fs::remove_file(format!("{save_path}.partial.meta")).await;
                            return Ok(());
                        }
                        self.soft_fail_transfer(transfer, error_codes::WRITE_ERROR, &e.to_string());
                        return Ok(());
                    }
                    // Feed bytes to SHA-256 hasher
                    sha_hasher.update(&buf[..n]);
                    written += n as i64;
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    if transfer.is_cancelled() {
                        let _ = tokio::fs::remove_file(&partial_path).await;
                        let _ = tokio::fs::remove_file(format!("{save_path}.partial.meta")).await;
                        return Ok(());
                    }
                    self.soft_fail_transfer(transfer, error_codes::WRITE_ERROR, &err_msg);
                    return Ok(());
                }
            }
        }

        // Verify exact byte count
        if written != expected_len {
            self.soft_fail_transfer(
                transfer,
                error_codes::INCOMPLETE_BODY,
                &format!(
                    "received {} bytes, expected {}",
                    written + offset,
                    transfer.file.size
                ),
            );
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                error_codes::INCOMPLETE_BODY,
                &format!(
                    "received {} bytes, expected {}",
                    written + offset,
                    transfer.file.size
                ),
            ));
        }

        info!(
            "[FileTransfer] Received {} bytes for {}",
            written + offset,
            transfer.id
        );

        // Verify SHA-256
        let computed_hash = hex::encode(sha_hasher.finalize());
        let expected_hash = &transfer.file.sha256;

        if !expected_hash.is_empty() && computed_hash != *expected_hash {
            self.fail_transfer(
                transfer,
                error_codes::INTEGRITY_MISMATCH,
                &format!("expected {expected_hash}, got {computed_hash}"),
            );
            return Err(json_error(
                StatusCode::CONFLICT,
                error_codes::INTEGRITY_MISMATCH,
                &format!(
                    "SHA-256 mismatch: expected {expected_hash}, got {computed_hash}"
                ),
            ));
        }

        // Rename .partial -> final path
        if let Err(e) = tokio::fs::rename(&partial_path, save_path).await {
            self.fail_transfer(transfer, error_codes::RENAME_ERROR, &e.to_string());
            return Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                error_codes::RENAME_ERROR,
                "failed to finalize file",
            ));
        }

        // Clean up manifest
        let _ = tokio::fs::remove_file(format!("{save_path}.partial.meta")).await;

        // Mark complete
        transfer.set_state(TransferState::Completed);
        transfer
            .bytes_transferred
            .store(written + offset, Ordering::Relaxed);

        let started_at = {
            let guard = transfer.started_at.lock().await;
            *guard
        };
        let duration_ms = started_at
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0);

        self.emit_event(FileTransferEvent::Complete {
            transfer_id: transfer.id.clone(),
            sha256: computed_hash,
            size: written + offset,
            duration_ms,
            direction: "receive".to_string(),
            path: Some(save_path.to_string()),
        });

        info!(
            "[FileTransfer] Receive complete: {} -> {} ({} bytes)",
            transfer.id,
            save_path,
            written + offset
        );

        Ok(())
    }

    // --- Failure helpers ---

    /// Mark a transfer as terminally failed and emit an error event.
    pub fn fail_transfer(&self, transfer: &Transfer, code: &str, message: &str) {
        transfer.set_state(TransferState::Failed);
        self.emit_event(FileTransferEvent::Error {
            transfer_id: transfer.id.clone(),
            code: code.to_string(),
            message: message.to_string(),
            resumable: error_codes::is_resumable(code),
        });
        warn!("[FileTransfer] Failed {}: {}: {}", transfer.id, code, message);
    }

    /// Emit a resumable error WITHOUT changing state (for connection drops).
    pub fn soft_fail_transfer(&self, transfer: &Transfer, code: &str, message: &str) {
        self.emit_event(FileTransferEvent::Error {
            transfer_id: transfer.id.clone(),
            code: code.to_string(),
            message: message.to_string(),
            resumable: true,
        });
        warn!(
            "[FileTransfer] Soft-failed {} (resumable): {}: {}",
            transfer.id, code, message
        );
    }

    // --- Events ---

    fn emit_event(&self, event: FileTransferEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Emit a progress event for a transfer.
    pub fn emit_progress(&self, transfer: &Transfer, bytes_transferred: i64) {
        transfer
            .bytes_transferred
            .store(bytes_transferred, Ordering::Relaxed);

        let started_at = {
            if let Ok(guard) = transfer.started_at.try_lock() {
                *guard
            } else {
                None
            }
        };

        let elapsed = started_at
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        let speed = if elapsed > 0.0 {
            bytes_transferred as f64 / elapsed
        } else {
            0.0
        };

        let percent = if transfer.file.size > 0 {
            bytes_transferred as f64 / transfer.file.size as f64 * 100.0
        } else {
            0.0
        };

        let eta = if speed > 0.0 {
            (transfer.file.size - bytes_transferred) as f64 / speed
        } else {
            0.0
        };

        self.emit_event(FileTransferEvent::Progress {
            transfer_id: transfer.id.clone(),
            bytes_transferred,
            total_bytes: transfer.file.size,
            percent,
            bytes_per_second: speed,
            eta,
            direction: transfer.direction.to_string(),
        });
    }

    // --- Cleanup ---

    /// Run the cleanup loop: sweep terminal and stale transfers periodically.
    pub async fn cleanup_loop(self: &Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.sweep_completed_transfers().await;
                }
                _ = self.shutdown_token.cancelled() => {
                    return;
                }
            }
        }
    }

    /// Remove transfers in terminal states that have been idle for CompletedRetainTTL,
    /// and registered transfers that were never started within RegisteredTimeoutTTL.
    async fn sweep_completed_transfers(&self) {
        let now = Instant::now();
        let mut transfers = self.transfers.write().await;

        let ids_to_remove: Vec<TransferID> = transfers
            .iter()
            .filter_map(|(id, t)| {
                let state = t.state();

                let last_progress = t.last_progress_at.try_lock().ok().and_then(|g| *g);
                let started = t.started_at.try_lock().ok().and_then(|g| *g);
                let last_activity = last_progress.or(started).unwrap_or(t.registered_at);

                // Terminal states: sweep after CompletedRetainTTL
                if matches!(
                    state,
                    TransferState::Completed | TransferState::Failed | TransferState::Cancelled
                ) && now.duration_since(last_activity) > self.config.completed_retain_ttl
                {
                    info!("[FileTransfer] Swept transfer {id} (state={state})");
                    return Some(id.clone());
                }

                // Registered but never started: sweep after RegisteredTimeoutTTL
                if state == TransferState::Registered {
                    let last_seen = if last_activity > t.registered_at {
                        last_activity
                    } else {
                        t.registered_at
                    };
                    if now.duration_since(last_seen) > self.config.registered_timeout_ttl {
                        t.cancel();
                        info!(
                            "[FileTransfer] Swept stale registered transfer {id} (last seen {:?} ago)",
                            now.duration_since(last_seen)
                        );
                        return Some(id.clone());
                    }
                }

                None
            })
            .collect();

        for id in ids_to_remove {
            transfers.remove(&id);
        }
    }

    /// Graceful shutdown: cancel all transfers, shut down cleanup loop.
    pub async fn stop(&self) {
        self.shutdown_token.cancel();

        let transfers = self.transfers.read().await;
        for t in transfers.values() {
            t.cancel();
        }
    }
}

/// Validate save path for path traversal attacks.
fn validate_save_path(p: &str) -> Result<String, String> {
    let path = Path::new(p);
    if !path.is_absolute() {
        return Err(format!("path must be absolute: {p:?}"));
    }

    let cleaned = path
        .canonicalize()
        .ok()
        .map(|c| c.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            // If the file doesn't exist yet, clean the parent and append filename
            if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
                if let Ok(clean_parent) = parent.canonicalize() {
                    return clean_parent.join(name).to_string_lossy().to_string();
                }
            }
            p.to_string()
        });

    // Check for traversal
    for component in Path::new(&cleaned).components() {
        if let std::path::Component::ParentDir = component {
            return Err(format!("path contains traversal: {p:?}"));
        }
    }

    // Verify parent directory exists and is a directory
    let parent = Path::new(&cleaned)
        .parent()
        .ok_or_else(|| format!("no parent directory: {p:?}"))?;

    if !parent.exists() {
        return Err(format!("parent directory does not exist: {parent:?}"));
    }
    if !parent.is_dir() {
        return Err(format!("parent is not a directory: {parent:?}"));
    }

    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validate_save_path_absolute() {
        let err = validate_save_path("relative/path.txt").unwrap_err();
        assert!(err.contains("absolute"));
    }

    #[test]
    fn validate_save_path_valid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("output.bin");
        let result = validate_save_path(path.to_str().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_save_path_missing_parent() {
        let err = validate_save_path("/nonexistent_abc_xyz/file.txt").unwrap_err();
        assert!(err.contains("parent directory"));
    }

    #[tokio::test]
    async fn manager_register_and_list() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mgr = FileTransferManager::new(FileTransferConfig::default(), tx);

        mgr.register_receive(
            "ft-test-1".to_string(),
            FileInfo {
                name: "file.txt".to_string(),
                size: 1024,
                sha256: "abc".to_string(),
            },
            "a".repeat(64),
            "/tmp/ft_test_output.txt".to_string(),
        )
        .await
        .unwrap();

        let list = mgr.list_transfers().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "ft-test-1");
        assert_eq!(list[0].direction, "receive");
    }

    #[tokio::test]
    async fn manager_cancel_transfer() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mgr = FileTransferManager::new(FileTransferConfig::default(), tx);

        mgr.register_receive(
            "ft-cancel-1".to_string(),
            FileInfo {
                name: "file.txt".to_string(),
                size: 100,
                sha256: "abc".to_string(),
            },
            "b".repeat(64),
            "/tmp/ft_test_cancel.txt".to_string(),
        )
        .await
        .unwrap();

        mgr.cancel_transfer("ft-cancel-1").await;

        // Should receive cancelled event
        let event = rx.recv().await.unwrap();
        match event {
            FileTransferEvent::Cancelled { transfer_id } => {
                assert_eq!(transfer_id, "ft-cancel-1");
            }
            _ => panic!("expected Cancelled event"),
        }
    }

    #[test]
    fn semaphore_acquire_release() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut config = FileTransferConfig::default();
        config.max_concurrent_recv = 2;
        let mgr = FileTransferManager::new(config, tx);

        assert!(mgr.try_acquire_recv_semaphore());
        assert!(mgr.try_acquire_recv_semaphore());
        assert!(!mgr.try_acquire_recv_semaphore()); // at capacity

        mgr.release_recv_semaphore();
        assert!(mgr.try_acquire_recv_semaphore()); // freed one
    }

    #[test]
    fn semaphore_unlimited() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut config = FileTransferConfig::default();
        config.max_concurrent_recv = 0; // unlimited
        let mgr = FileTransferManager::new(config, tx);

        for _ in 0..100 {
            assert!(mgr.try_acquire_recv_semaphore());
        }
    }

    #[test]
    fn get_transfer_validates_token_format() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mgr = FileTransferManager::new(FileTransferConfig::default(), tx);

        // Short token
        assert!(mgr.get_transfer("ft-1", "short").is_err());
        // Non-hex
        assert!(mgr.get_transfer("ft-1", &"g".repeat(64)).is_err());
        // Not found
        assert!(mgr.get_transfer("ft-1", &"a".repeat(64)).is_err());
    }
}
