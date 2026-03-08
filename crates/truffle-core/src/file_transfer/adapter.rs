use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use tracing::warn;

use super::manager::FileTransferManager;
use super::sender::DialFn;
use super::types::*;

/// Namespace for file transfer messages on the MessageBus.
pub const FILE_TRANSFER_NAMESPACE: &str = "file-transfer";

/// Message types for file transfer signaling.
pub mod message_types {
    pub const OFFER: &str = "OFFER";
    pub const ACCEPT: &str = "ACCEPT";
    pub const REJECT: &str = "REJECT";
    pub const CANCEL: &str = "CANCEL";
}

/// Offer payload: sender -> receiver via MessageBus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTransferOffer {
    pub transfer_id: String,
    pub sender_device_id: String,
    pub sender_addr: String,
    pub file: FileInfo,
    pub token: String,
}

/// Accept payload: receiver -> sender via MessageBus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTransferAccept {
    pub transfer_id: String,
    pub receiver_device_id: String,
    pub receiver_addr: String,
    pub token: String,
}

/// Reject payload: receiver -> sender via MessageBus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTransferReject {
    pub transfer_id: String,
    pub reason: String,
}

/// Cancel payload: either side via MessageBus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTransferCancel {
    pub transfer_id: String,
    pub cancelled_by: String,
    pub reason: String,
}

/// Adapter event emitted to the UI/API layer.
#[derive(Debug, Clone)]
pub enum AdapterEvent {
    /// An incoming offer from a remote sender.
    Offer(FileTransferOffer),
    /// Preparing progress (hashing).
    PreparingProgress(FileTransferEvent),
    /// Transfer progress.
    Progress(FileTransferEvent),
    /// Transfer completed.
    Completed(FileTransferEvent),
    /// Transfer failed.
    Failed(String, FileTransferEvent),
    /// Transfer cancelled.
    Cancelled(String),
}

/// Tracked state for a transfer from the adapter's perspective.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterTransferInfo {
    pub transfer_id: String,
    pub direction: String,
    pub state: String,
    pub peer_device_id: String,
    pub file: FileInfo,
    pub bytes_transferred: i64,
    pub percent: f64,
    pub bytes_per_second: f64,
    pub eta: f64,
}

/// Configuration for the FileTransferAdapter.
pub struct FileTransferAdapterConfig {
    pub local_device_id: String,
    pub local_addr: String,
    pub output_dir: String,
    pub dial_fn: DialFn,
}

/// FileTransferAdapter bridges the MessageBus (control plane) and the
/// FileTransferManager (data plane). Ports the TS FileTransferAdapter.
///
/// Responsibilities:
/// - Handles OFFER/ACCEPT/REJECT/CANCEL messages from the mesh
/// - Sends OFFER/ACCEPT/REJECT/CANCEL messages to the mesh
/// - Translates manager events (progress, complete, error) to adapter events
/// - Tracks file paths for sender-side transfers (needed when ACCEPT arrives)
pub struct FileTransferAdapter {
    config: FileTransferAdapterConfig,
    manager: Arc<FileTransferManager>,

    /// Track transfers for state queries.
    transfers: RwLock<HashMap<String, AdapterTransferInfo>>,
    /// Track original file paths for sender transfers (needed when ACCEPT arrives).
    file_paths: RwLock<HashMap<String, String>>,

    /// Channel for outgoing MessageBus messages.
    /// Format: (target_device_id, message_type, payload_json)
    bus_tx: mpsc::UnboundedSender<(String, String, String)>,

    /// Channel for adapter events (to UI/API layer).
    adapter_event_tx: mpsc::UnboundedSender<AdapterEvent>,
}

impl FileTransferAdapter {
    pub fn new(
        config: FileTransferAdapterConfig,
        manager: Arc<FileTransferManager>,
        bus_tx: mpsc::UnboundedSender<(String, String, String)>,
        adapter_event_tx: mpsc::UnboundedSender<AdapterEvent>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            manager,
            transfers: RwLock::new(HashMap::new()),
            file_paths: RwLock::new(HashMap::new()),
            bus_tx,
            adapter_event_tx,
        })
    }

    // --- Public API ---

    /// Initiate sending a file to a target device.
    /// 1. Ask manager to prepare (stat + hash)
    /// 2. Wait for Prepared event
    /// 3. Send OFFER via MessageBus
    pub async fn send_file(&self, _target_device_id: &str, file_path: &str) -> String {
        let transfer_id = generate_transfer_id();
        let _token = generate_token();

        // Ask manager to prepare the file
        self.manager.prepare_file(&transfer_id, file_path).await;

        // Store file path for when ACCEPT arrives
        self.file_paths
            .write()
            .await
            .insert(transfer_id.clone(), file_path.to_string());

        // Note: in the full implementation, we'd wait for the Prepared event
        // before sending the OFFER. For now, the event loop handles this.

        transfer_id
    }

    /// Accept an incoming transfer offer.
    pub async fn accept_transfer(&self, offer: &FileTransferOffer, save_path: Option<&str>) {
        let final_path = save_path
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{}/{}", self.config.output_dir, offer.file.name));

        // Track the transfer
        self.transfers.write().await.insert(
            offer.transfer_id.clone(),
            AdapterTransferInfo {
                transfer_id: offer.transfer_id.clone(),
                direction: "receive".to_string(),
                state: "accepted".to_string(),
                peer_device_id: offer.sender_device_id.clone(),
                file: offer.file.clone(),
                bytes_transferred: 0,
                percent: 0.0,
                bytes_per_second: 0.0,
                eta: 0.0,
            },
        );

        // 1. Register in manager BEFORE signaling accept (critical ordering)
        if let Err(e) = self
            .manager
            .register_receive(
                offer.transfer_id.clone(),
                offer.file.clone(),
                offer.token.clone(),
                final_path,
            )
            .await
        {
            warn!("[FileTransferAdapter] Failed to register receive: {e}");
            return;
        }

        // 2. Signal acceptance via MessageBus
        let accept = FileTransferAccept {
            transfer_id: offer.transfer_id.clone(),
            receiver_device_id: self.config.local_device_id.clone(),
            receiver_addr: self.config.local_addr.clone(),
            token: offer.token.clone(),
        };

        let payload = serde_json::to_string(&accept).unwrap_or_default();
        let _ = self.bus_tx.send((
            offer.sender_device_id.clone(),
            message_types::ACCEPT.to_string(),
            payload,
        ));
    }

    /// Reject an incoming transfer offer.
    pub async fn reject_transfer(&self, offer: &FileTransferOffer, reason: &str) {
        self.manager.remove_transfer(&offer.transfer_id).await;

        let reject = FileTransferReject {
            transfer_id: offer.transfer_id.clone(),
            reason: reason.to_string(),
        };

        let payload = serde_json::to_string(&reject).unwrap_or_default();
        let _ = self.bus_tx.send((
            offer.sender_device_id.clone(),
            message_types::REJECT.to_string(),
            payload,
        ));
    }

    /// Cancel an active transfer.
    pub async fn cancel_transfer(&self, transfer_id: &str) {
        self.manager.cancel_transfer(transfer_id).await;

        let transfers = self.transfers.read().await;
        if let Some(info) = transfers.get(transfer_id) {
            let cancel = FileTransferCancel {
                transfer_id: transfer_id.to_string(),
                cancelled_by: self.config.local_device_id.clone(),
                reason: "cancelled by user".to_string(),
            };
            let payload = serde_json::to_string(&cancel).unwrap_or_default();
            let _ = self.bus_tx.send((
                info.peer_device_id.clone(),
                message_types::CANCEL.to_string(),
                payload,
            ));
        }
    }

    /// Get all tracked transfers.
    pub async fn get_transfers(&self) -> Vec<AdapterTransferInfo> {
        self.transfers.read().await.values().cloned().collect()
    }

    // --- MessageBus message handler ---

    /// Handle an incoming MessageBus message.
    pub async fn handle_bus_message(&self, msg_type: &str, payload: &str) {
        match msg_type {
            message_types::OFFER => {
                if let Ok(offer) = serde_json::from_str::<FileTransferOffer>(payload) {
                    let _ = self.adapter_event_tx.send(AdapterEvent::Offer(offer));
                }
            }

            message_types::ACCEPT => {
                if let Ok(accept) = serde_json::from_str::<FileTransferAccept>(payload) {
                    let file_paths = self.file_paths.read().await;
                    let transfers = self.transfers.read().await;

                    let original_path = file_paths.get(&accept.transfer_id).cloned();
                    let transfer_info = transfers.get(&accept.transfer_id).cloned();
                    drop(file_paths);
                    drop(transfers);

                    if let (Some(path), Some(info)) = (original_path, transfer_info) {
                        // Update state
                        self.transfers
                            .write()
                            .await
                            .entry(accept.transfer_id.clone())
                            .and_modify(|t| t.state = "transferring".to_string());

                        // Register send and start transfer
                        let transfer = self
                            .manager
                            .register_send(
                                accept.transfer_id.clone(),
                                info.file.clone(),
                                accept.token.clone(),
                                path,
                            )
                            .await;

                        let mgr = Arc::clone(&self.manager);
                        let dial_fn = self.config.dial_fn.clone();
                        let addr = accept.receiver_addr.clone();
                        tokio::spawn(async move {
                            mgr.send_file(transfer, addr, dial_fn).await;
                        });
                    }
                }
            }

            message_types::REJECT => {
                if let Ok(reject) = serde_json::from_str::<FileTransferReject>(payload) {
                    self.transfers.write().await.remove(&reject.transfer_id);
                    self.file_paths.write().await.remove(&reject.transfer_id);
                    let _ = self
                        .adapter_event_tx
                        .send(AdapterEvent::Failed(
                            reject.transfer_id.clone(),
                            FileTransferEvent::Error {
                                transfer_id: reject.transfer_id,
                                code: "REJECTED".to_string(),
                                message: reject.reason,
                                resumable: false,
                            },
                        ));
                }
            }

            message_types::CANCEL => {
                if let Ok(cancel) = serde_json::from_str::<FileTransferCancel>(payload) {
                    self.transfers.write().await.remove(&cancel.transfer_id);
                    self.file_paths.write().await.remove(&cancel.transfer_id);
                    let _ = self
                        .adapter_event_tx
                        .send(AdapterEvent::Cancelled(cancel.transfer_id));
                }
            }

            _ => {
                warn!("[FileTransferAdapter] Unknown message type: {msg_type}");
            }
        }
    }

    // --- Manager event handler ---

    /// Process events from the FileTransferManager event channel.
    /// Should be called in a loop by the owner.
    pub async fn handle_manager_event(&self, event: FileTransferEvent) {
        match &event {
            FileTransferEvent::PreparingProgress { .. } => {
                let _ = self
                    .adapter_event_tx
                    .send(AdapterEvent::PreparingProgress(event));
            }

            FileTransferEvent::Prepared { .. } => {
                // Forward to adapter layer. The orchestrator that called send_file()
                // uses this to construct the OFFER message.
                let _ = self
                    .adapter_event_tx
                    .send(AdapterEvent::PreparingProgress(event));
            }

            FileTransferEvent::Progress {
                transfer_id,
                bytes_transferred,
                percent,
                bytes_per_second,
                eta,
                ..
            } => {
                if let Some(info) = self.transfers.write().await.get_mut(transfer_id) {
                    info.bytes_transferred = *bytes_transferred;
                    info.percent = *percent;
                    info.bytes_per_second = *bytes_per_second;
                    info.eta = *eta;
                    info.state = "transferring".to_string();
                }
                let _ = self.adapter_event_tx.send(AdapterEvent::Progress(event));
            }

            FileTransferEvent::Complete { transfer_id, .. } => {
                if let Some(info) = self.transfers.write().await.get_mut(transfer_id) {
                    info.state = "completed".to_string();
                }
                let _ = self.adapter_event_tx.send(AdapterEvent::Completed(event));
                // Cleanup handled by manager's sweep loop after CompletedRetainTTL
            }

            FileTransferEvent::Error {
                transfer_id,
                resumable,
                ..
            } => {
                if let Some(info) = self.transfers.write().await.get_mut(transfer_id) {
                    info.state = "failed".to_string();
                }
                let tid = transfer_id.clone();
                let is_resumable = *resumable;
                let _ = self
                    .adapter_event_tx
                    .send(AdapterEvent::Failed(tid.clone(), event));

                if !is_resumable {
                    self.transfers.write().await.remove(&tid);
                    self.file_paths.write().await.remove(&tid);
                }
            }

            FileTransferEvent::Cancelled { transfer_id } => {
                let tid = transfer_id.clone();
                self.transfers.write().await.remove(&tid);
                self.file_paths.write().await.remove(&tid);
                let _ = self.adapter_event_tx.send(AdapterEvent::Cancelled(tid));
            }

            FileTransferEvent::List { .. } => {
                // List events are handled directly via get_transfers()
            }
        }
    }
}

/// Generate a transfer ID: "ft-<16 hex chars>" (64 bits of randomness).
fn generate_transfer_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    format!("ft-{}", hex::encode(bytes))
}

/// Generate a transfer token: 64 hex chars (256 bits of randomness).
fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_transfer_id_format() {
        let id = generate_transfer_id();
        assert!(id.starts_with("ft-"));
        assert_eq!(id.len(), 3 + 16); // "ft-" + 16 hex chars
    }

    #[test]
    fn generate_token_format() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn offer_serde() {
        let offer = FileTransferOffer {
            transfer_id: "ft-abc123".to_string(),
            sender_device_id: "device-1".to_string(),
            sender_addr: "host.ts.net:9417".to_string(),
            file: FileInfo {
                name: "test.bin".to_string(),
                size: 1024,
                sha256: "abc".to_string(),
            },
            token: "a".repeat(64),
        };

        let json = serde_json::to_string(&offer).unwrap();
        let parsed: FileTransferOffer = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.transfer_id, offer.transfer_id);
        assert_eq!(parsed.file.name, "test.bin");
    }

    #[test]
    fn accept_serde() {
        let accept = FileTransferAccept {
            transfer_id: "ft-abc123".to_string(),
            receiver_device_id: "device-2".to_string(),
            receiver_addr: "host2.ts.net:9417".to_string(),
            token: "b".repeat(64),
        };

        let json = serde_json::to_string(&accept).unwrap();
        let parsed: FileTransferAccept = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.receiver_device_id, "device-2");
    }
}
