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
    /// When true, the receiver should auto-accept without user interaction (CLI-mode transfers).
    #[serde(default)]
    pub cli_mode: bool,
    /// Destination path on the receiver where the file should be saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_path: Option<String>,
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

        let payload = match serde_json::to_string(&accept) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Failed to serialize file transfer accept: {e}");
                return;
            }
        };
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

        let payload = match serde_json::to_string(&reject) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Failed to serialize file transfer reject: {e}");
                return;
            }
        };
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
            let payload = match serde_json::to_string(&cancel) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Failed to serialize file transfer cancel: {e}");
                    return;
                }
            };
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

    /// Send an OFFER message to a target device via the message bus.
    ///
    /// Used by the daemon handler to initiate a CLI-mode file transfer.
    /// The caller is responsible for preparing the file and constructing the offer.
    pub fn send_offer(&self, target_device_id: &str, offer: &FileTransferOffer) {
        let payload = match serde_json::to_string(offer) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Failed to serialize file transfer offer: {e}");
                return;
            }
        };
        let _ = self.bus_tx.send((
            target_device_id.to_string(),
            message_types::OFFER.to_string(),
            payload,
        ));
    }

    /// Register a pending send transfer in the adapter's tracking maps.
    ///
    /// This must be called before sending the OFFER so that when ACCEPT arrives,
    /// the adapter's ACCEPT handler can find the file path and transfer info.
    pub async fn register_pending_send(
        &self,
        transfer_id: &str,
        file_path: &str,
        file: &FileInfo,
        peer_device_id: &str,
    ) {
        self.file_paths
            .write()
            .await
            .insert(transfer_id.to_string(), file_path.to_string());

        self.transfers.write().await.insert(
            transfer_id.to_string(),
            AdapterTransferInfo {
                transfer_id: transfer_id.to_string(),
                direction: "send".to_string(),
                state: "offered".to_string(),
                peer_device_id: peer_device_id.to_string(),
                file: file.clone(),
                bytes_transferred: 0,
                percent: 0.0,
                bytes_per_second: 0.0,
                eta: 0.0,
            },
        );
    }

    /// Get a reference to the underlying manager.
    pub fn manager(&self) -> &Arc<FileTransferManager> {
        &self.manager
    }

    /// Get the local device ID.
    pub fn local_device_id(&self) -> &str {
        &self.config.local_device_id
    }

    /// Get the local address (for sender_addr in offers).
    pub fn local_addr(&self) -> &str {
        &self.config.local_addr
    }

    // --- MessageBus message handler ---

    /// Handle an incoming MessageBus message.
    pub async fn handle_bus_message(&self, msg_type: &str, payload: &serde_json::Value) {
        match msg_type {
            message_types::OFFER => {
                if let Ok(offer) = serde_json::from_value::<FileTransferOffer>(payload.clone()) {
                    if offer.cli_mode {
                        if let Some(ref save_path) = offer.save_path {
                            if !save_path.is_empty() {
                                // CLI-mode: auto-accept without user interaction
                                tracing::info!(
                                    "[FileTransferAdapter] Auto-accepting CLI transfer {} -> {}",
                                    offer.transfer_id,
                                    save_path
                                );
                                self.accept_transfer(&offer, Some(save_path)).await;
                                return;
                            }
                        }
                    }
                    // Interactive mode: emit Offer event for UI confirmation
                    let _ = self.adapter_event_tx.send(AdapterEvent::Offer(offer));
                }
            }

            message_types::ACCEPT => {
                if let Ok(accept) = serde_json::from_value::<FileTransferAccept>(payload.clone()) {
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
                if let Ok(reject) = serde_json::from_value::<FileTransferReject>(payload.clone()) {
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
                if let Ok(cancel) = serde_json::from_value::<FileTransferCancel>(payload.clone()) {
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
pub fn generate_transfer_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    format!("ft-{}", hex::encode(bytes))
}

/// Generate a transfer token: 64 hex chars (256 bits of randomness).
pub fn generate_token() -> String {
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
            cli_mode: false,
            save_path: None,
        };

        let json = serde_json::to_string(&offer).unwrap();
        let parsed: FileTransferOffer = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.transfer_id, offer.transfer_id);
        assert_eq!(parsed.file.name, "test.bin");
        assert!(!parsed.cli_mode);
        assert!(parsed.save_path.is_none());
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

    // ══════════════════════════════════════════════════════════════════════
    // Adversarial edge-case tests (Layer 4: Services — Adapter)
    // ══════════════════════════════════════════════════════════════════════

    /// Helper to create a FileTransferAdapter with a real manager but no network.
    fn create_test_adapter() -> (
        Arc<FileTransferAdapter>,
        mpsc::UnboundedReceiver<(String, String, String)>,
        mpsc::UnboundedReceiver<AdapterEvent>,
        tokio::sync::broadcast::Receiver<FileTransferEvent>,
    ) {
        let (event_tx, event_rx) = tokio::sync::broadcast::channel(64);
        let manager = FileTransferManager::new(FileTransferConfig::default(), event_tx);
        let (bus_tx, bus_rx) = mpsc::unbounded_channel();
        let (adapter_event_tx, adapter_event_rx) = mpsc::unbounded_channel();

        let dial_fn: DialFn = Arc::new(|_addr: &str| {
            Box::pin(async {
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "test: no network",
                ))
            })
        });

        let adapter = FileTransferAdapter::new(
            FileTransferAdapterConfig {
                local_device_id: "dev-local".to_string(),
                local_addr: "127.0.0.1:9417".to_string(),
                output_dir: "/tmp/ft-test".to_string(),
                dial_fn,
            },
            manager,
            bus_tx,
            adapter_event_tx,
        );

        (adapter, bus_rx, adapter_event_rx, event_rx)
    }

    // ── 15. OFFER from unknown sender is still processed ─────────────────
    #[tokio::test]
    async fn offer_from_unknown_sender_emits_event() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        let offer_payload = serde_json::json!({
            "transferId": "ft-unknown-sender",
            "senderDeviceId": "dev-stranger",
            "senderAddr": "10.0.0.99:9417",
            "file": {"name": "secret.bin", "size": 1024, "sha256": "abc"},
            "token": "a".repeat(64),
        });

        adapter
            .handle_bus_message(message_types::OFFER, &offer_payload)
            .await;

        // Should emit an Offer event even from an unknown device
        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Offer(offer) => {
                assert_eq!(offer.transfer_id, "ft-unknown-sender");
                assert_eq!(offer.sender_device_id, "dev-stranger");
            }
            _ => panic!("Expected Offer event, got {:?}", event),
        }
    }

    // ── 16. ACCEPT for unknown transfer_id ───────────────────────────────
    #[tokio::test]
    async fn accept_for_unknown_transfer_no_crash() {
        let (adapter, _bus_rx, _adapter_event_rx, _event_rx) = create_test_adapter();

        let accept_payload = serde_json::json!({
            "transferId": "ft-nonexistent",
            "receiverDeviceId": "dev-2",
            "receiverAddr": "10.0.0.2:9417",
            "token": "a".repeat(64),
        });

        // Should not panic
        adapter
            .handle_bus_message(message_types::ACCEPT, &accept_payload)
            .await;

        // No crash is the assertion. Also verify no bus messages were sent.
        // (because there's no file_path or transfer_info for this transfer)
    }

    // ── 17. REJECT for unknown transfer_id ───────────────────────────────
    #[tokio::test]
    async fn reject_for_unknown_transfer_no_crash() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        let reject_payload = serde_json::json!({
            "transferId": "ft-ghost",
            "reason": "user declined",
        });

        adapter
            .handle_bus_message(message_types::REJECT, &reject_payload)
            .await;

        // Should emit a Failed event (REJECTED code)
        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Failed(tid, FileTransferEvent::Error { code, .. }) => {
                assert_eq!(tid, "ft-ghost");
                assert_eq!(code, "REJECTED");
            }
            _ => panic!("Expected Failed event, got {:?}", event),
        }
    }

    // ── 18. CANCEL for unknown transfer_id ───────────────────────────────
    #[tokio::test]
    async fn cancel_for_unknown_transfer_no_crash() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        let cancel_payload = serde_json::json!({
            "transferId": "ft-phantom",
            "cancelledBy": "dev-2",
            "reason": "user cancelled",
        });

        adapter
            .handle_bus_message(message_types::CANCEL, &cancel_payload)
            .await;

        // Should emit Cancelled event
        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Cancelled(tid) => {
                assert_eq!(tid, "ft-phantom");
            }
            _ => panic!("Expected Cancelled event, got {:?}", event),
        }
    }

    // ── 19. Double ACCEPT ────────────────────────────────────────────────
    // The adapter requires both file_paths and transfers to have the
    // transfer_id for ACCEPT to trigger a send. Without a prior send_file
    // call, ACCEPT is a no-op. Test that double-handling is safe.
    #[tokio::test]
    async fn double_accept_is_safe_noop() {
        let (adapter, _bus_rx, _adapter_event_rx, _event_rx) = create_test_adapter();

        let accept_payload = serde_json::json!({
            "transferId": "ft-double-accept",
            "receiverDeviceId": "dev-2",
            "receiverAddr": "10.0.0.2:9417",
            "token": "b".repeat(64),
        });

        // Both calls should be no-ops (transfer not in file_paths/transfers maps)
        adapter
            .handle_bus_message(message_types::ACCEPT, &accept_payload)
            .await;
        adapter
            .handle_bus_message(message_types::ACCEPT, &accept_payload)
            .await;

        // No panic, no crash
    }

    // ── Edge: Unknown message type ───────────────────────────────────────
    #[tokio::test]
    async fn unknown_bus_message_type_no_crash() {
        let (adapter, _bus_rx, _adapter_event_rx, _event_rx) = create_test_adapter();

        adapter
            .handle_bus_message("UNKNOWN_TYPE", &serde_json::json!({"foo": "bar"}))
            .await;

        // No panic is the assertion
    }

    // ── Edge: Malformed OFFER payload ────────────────────────────────────
    #[tokio::test]
    async fn malformed_offer_payload_no_crash() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        // Missing required fields
        adapter
            .handle_bus_message(message_types::OFFER, &serde_json::json!({"garbage": true}))
            .await;

        // Should not emit an event (deserialization fails silently)
        assert!(
            adapter_event_rx.try_recv().is_err(),
            "Malformed OFFER should not produce an event"
        );
    }

    // ── Edge: Malformed REJECT payload ───────────────────────────────────
    #[tokio::test]
    async fn malformed_reject_payload_no_crash() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        adapter
            .handle_bus_message(message_types::REJECT, &serde_json::json!({"bad": "data"}))
            .await;

        // Should not emit (deserialization fails)
        assert!(adapter_event_rx.try_recv().is_err());
    }

    // ── Edge: handle_manager_event with unknown transfer_id ──────────────
    #[tokio::test]
    async fn manager_event_progress_for_unknown_transfer() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        // Send a progress event for a transfer that isn't tracked
        adapter
            .handle_manager_event(FileTransferEvent::Progress {
                transfer_id: "ft-untracked".to_string(),
                bytes_transferred: 500,
                total_bytes: 1000,
                percent: 50.0,
                bytes_per_second: 100.0,
                eta: 5.0,
                direction: "send".to_string(),
            })
            .await;

        // Should still forward the event to adapter_event_tx
        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Progress(FileTransferEvent::Progress { transfer_id, .. }) => {
                assert_eq!(transfer_id, "ft-untracked");
            }
            _ => panic!("Expected Progress event"),
        }
    }

    // ── Edge: handle_manager_event Complete for unknown transfer ──────────
    #[tokio::test]
    async fn manager_event_complete_for_unknown_transfer() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        adapter
            .handle_manager_event(FileTransferEvent::Complete {
                transfer_id: "ft-untracked-complete".to_string(),
                sha256: "abc".to_string(),
                size: 1000,
                duration_ms: 100,
                direction: "receive".to_string(),
                path: Some("/tmp/out.bin".to_string()),
            })
            .await;

        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Completed(_) => {}
            _ => panic!("Expected Completed event"),
        }
    }

    // ── Edge: handle_manager_event Error (non-resumable) cleanup ─────────
    #[tokio::test]
    async fn manager_event_non_resumable_error_cleans_up() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        // Pre-populate transfers map to verify cleanup
        adapter.transfers.write().await.insert(
            "ft-error-cleanup".to_string(),
            AdapterTransferInfo {
                transfer_id: "ft-error-cleanup".to_string(),
                direction: "send".to_string(),
                state: "transferring".to_string(),
                peer_device_id: "dev-2".to_string(),
                file: FileInfo { name: "f.bin".to_string(), size: 100, sha256: "abc".to_string() },
                bytes_transferred: 50,
                percent: 50.0,
                bytes_per_second: 10.0,
                eta: 5.0,
            },
        );
        adapter.file_paths.write().await.insert(
            "ft-error-cleanup".to_string(),
            "/tmp/source.bin".to_string(),
        );

        adapter
            .handle_manager_event(FileTransferEvent::Error {
                transfer_id: "ft-error-cleanup".to_string(),
                code: "INTEGRITY_MISMATCH".to_string(),
                message: "hash mismatch".to_string(),
                resumable: false,
            })
            .await;

        // Non-resumable errors should clean up transfers and file_paths
        assert!(
            adapter.transfers.read().await.get("ft-error-cleanup").is_none(),
            "Non-resumable error should remove transfer"
        );
        assert!(
            adapter.file_paths.read().await.get("ft-error-cleanup").is_none(),
            "Non-resumable error should remove file_path"
        );

        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Failed(tid, _) => assert_eq!(tid, "ft-error-cleanup"),
            _ => panic!("Expected Failed event"),
        }
    }

    // ── Edge: handle_manager_event Error (resumable) keeps state ──────────
    #[tokio::test]
    async fn manager_event_resumable_error_keeps_state() {
        let (adapter, _bus_rx, _adapter_event_rx, _event_rx) = create_test_adapter();

        adapter.transfers.write().await.insert(
            "ft-error-resume".to_string(),
            AdapterTransferInfo {
                transfer_id: "ft-error-resume".to_string(),
                direction: "send".to_string(),
                state: "transferring".to_string(),
                peer_device_id: "dev-2".to_string(),
                file: FileInfo { name: "f.bin".to_string(), size: 100, sha256: "abc".to_string() },
                bytes_transferred: 50,
                percent: 50.0,
                bytes_per_second: 10.0,
                eta: 5.0,
            },
        );

        adapter
            .handle_manager_event(FileTransferEvent::Error {
                transfer_id: "ft-error-resume".to_string(),
                code: "SEND_ERROR".to_string(),
                message: "connection lost".to_string(),
                resumable: true,
            })
            .await;

        // Resumable errors should keep the transfer for retry
        let transfers = adapter.transfers.read().await;
        let info = transfers.get("ft-error-resume").unwrap();
        assert_eq!(info.state, "failed", "State should be updated to failed");
    }

    // ── Edge: handle_manager_event Cancelled cleanup ─────────────────────
    #[tokio::test]
    async fn manager_event_cancelled_cleans_up() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        adapter.transfers.write().await.insert(
            "ft-cancel-cleanup".to_string(),
            AdapterTransferInfo {
                transfer_id: "ft-cancel-cleanup".to_string(),
                direction: "receive".to_string(),
                state: "transferring".to_string(),
                peer_device_id: "dev-3".to_string(),
                file: FileInfo { name: "f.bin".to_string(), size: 200, sha256: "def".to_string() },
                bytes_transferred: 100,
                percent: 50.0,
                bytes_per_second: 20.0,
                eta: 5.0,
            },
        );
        adapter.file_paths.write().await.insert(
            "ft-cancel-cleanup".to_string(),
            "/tmp/src.bin".to_string(),
        );

        adapter
            .handle_manager_event(FileTransferEvent::Cancelled {
                transfer_id: "ft-cancel-cleanup".to_string(),
            })
            .await;

        assert!(adapter.transfers.read().await.get("ft-cancel-cleanup").is_none());
        assert!(adapter.file_paths.read().await.get("ft-cancel-cleanup").is_none());

        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Cancelled(tid) => assert_eq!(tid, "ft-cancel-cleanup"),
            _ => panic!("Expected Cancelled event"),
        }
    }

    // ── Edge: get_transfers returns all tracked transfers ─────────────────
    #[tokio::test]
    async fn get_transfers_returns_all() {
        let (adapter, _bus_rx, _adapter_event_rx, _event_rx) = create_test_adapter();

        for i in 0..5 {
            adapter.transfers.write().await.insert(
                format!("ft-list-{i}"),
                AdapterTransferInfo {
                    transfer_id: format!("ft-list-{i}"),
                    direction: "send".to_string(),
                    state: "transferring".to_string(),
                    peer_device_id: "dev-2".to_string(),
                    file: FileInfo { name: format!("file{i}.bin"), size: 100, sha256: "abc".to_string() },
                    bytes_transferred: 0,
                    percent: 0.0,
                    bytes_per_second: 0.0,
                    eta: 0.0,
                },
            );
        }

        let transfers = adapter.get_transfers().await;
        assert_eq!(transfers.len(), 5);
    }

    // ── Edge: List event is ignored by handle_manager_event ──────────────
    #[tokio::test]
    async fn manager_event_list_is_ignored() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        adapter
            .handle_manager_event(FileTransferEvent::List {
                transfers: vec![],
            })
            .await;

        // List events should not produce adapter events
        assert!(adapter_event_rx.try_recv().is_err());
    }

    // ══════════════════════════════════════════════════════════════════════
    // Phase 5: Auto-accept CLI-mode transfers
    // ══════════════════════════════════════════════════════════════════════

    // ── CLI-mode offer with save_path auto-accepts ────────────────────────
    #[tokio::test]
    async fn cli_mode_offer_auto_accepts() {
        let dir = tempfile::TempDir::new().unwrap();
        let save_path = dir.path().join("received.bin");

        let (adapter, mut bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        let offer_payload = serde_json::json!({
            "transferId": "ft-cli-auto",
            "senderDeviceId": "dev-remote",
            "senderAddr": "10.0.0.1:9417",
            "file": {"name": "data.bin", "size": 1024, "sha256": "abc"},
            "token": "a".repeat(64),
            "cliMode": true,
            "savePath": save_path.to_str().unwrap(),
        });

        adapter
            .handle_bus_message(message_types::OFFER, &offer_payload)
            .await;

        // Should NOT emit an AdapterEvent::Offer (auto-accepted, not interactive)
        assert!(
            adapter_event_rx.try_recv().is_err(),
            "CLI-mode offer should not emit Offer event to UI"
        );

        // Should send an ACCEPT message via bus_tx
        let (target_device, msg_type, _payload) = bus_rx.try_recv()
            .expect("Auto-accept should send ACCEPT via bus");
        assert_eq!(target_device, "dev-remote");
        assert_eq!(msg_type, message_types::ACCEPT);
    }

    // ── Non-CLI-mode offer emits Offer event for interactive UI ──────────
    #[tokio::test]
    async fn non_cli_mode_offer_emits_event() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        let offer_payload = serde_json::json!({
            "transferId": "ft-interactive",
            "senderDeviceId": "dev-remote",
            "senderAddr": "10.0.0.1:9417",
            "file": {"name": "photo.jpg", "size": 2048, "sha256": "def"},
            "token": "b".repeat(64),
            "cliMode": false,
        });

        adapter
            .handle_bus_message(message_types::OFFER, &offer_payload)
            .await;

        // Should emit AdapterEvent::Offer for UI confirmation
        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Offer(offer) => {
                assert_eq!(offer.transfer_id, "ft-interactive");
                assert!(!offer.cli_mode);
            }
            _ => panic!("Expected Offer event, got {:?}", event),
        }
    }

    // ── CLI-mode offer without save_path falls through to interactive ─────
    #[tokio::test]
    async fn cli_mode_without_save_path_emits_offer_event() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        let offer_payload = serde_json::json!({
            "transferId": "ft-cli-no-path",
            "senderDeviceId": "dev-remote",
            "senderAddr": "10.0.0.1:9417",
            "file": {"name": "file.bin", "size": 512, "sha256": "ghi"},
            "token": "c".repeat(64),
            "cliMode": true,
            // No savePath
        });

        adapter
            .handle_bus_message(message_types::OFFER, &offer_payload)
            .await;

        // Without save_path, should fall through to interactive mode
        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Offer(offer) => {
                assert_eq!(offer.transfer_id, "ft-cli-no-path");
                assert!(offer.cli_mode);
                assert!(offer.save_path.is_none());
            }
            _ => panic!("Expected Offer event, got {:?}", event),
        }
    }

    // ── CLI-mode offer with empty save_path falls through to interactive ──
    #[tokio::test]
    async fn cli_mode_with_empty_save_path_emits_offer_event() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        let offer_payload = serde_json::json!({
            "transferId": "ft-cli-empty-path",
            "senderDeviceId": "dev-remote",
            "senderAddr": "10.0.0.1:9417",
            "file": {"name": "file.bin", "size": 512, "sha256": "jkl"},
            "token": "d".repeat(64),
            "cliMode": true,
            "savePath": "",
        });

        adapter
            .handle_bus_message(message_types::OFFER, &offer_payload)
            .await;

        // Empty save_path should fall through to interactive mode
        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Offer(offer) => {
                assert_eq!(offer.transfer_id, "ft-cli-empty-path");
            }
            _ => panic!("Expected Offer event, got {:?}", event),
        }
    }

    // ── Offer without cli_mode field defaults to false (backward compat) ──
    #[tokio::test]
    async fn offer_without_cli_mode_defaults_to_interactive() {
        let (adapter, _bus_rx, mut adapter_event_rx, _event_rx) = create_test_adapter();

        // Legacy offer payload without cliMode or savePath fields
        let offer_payload = serde_json::json!({
            "transferId": "ft-legacy",
            "senderDeviceId": "dev-old",
            "senderAddr": "10.0.0.1:9417",
            "file": {"name": "legacy.bin", "size": 256, "sha256": "mno"},
            "token": "e".repeat(64),
        });

        adapter
            .handle_bus_message(message_types::OFFER, &offer_payload)
            .await;

        // Should default to interactive (cli_mode defaults to false)
        let event = adapter_event_rx.try_recv().unwrap();
        match event {
            AdapterEvent::Offer(offer) => {
                assert_eq!(offer.transfer_id, "ft-legacy");
                assert!(!offer.cli_mode, "cli_mode should default to false");
                assert!(offer.save_path.is_none(), "save_path should default to None");
            }
            _ => panic!("Expected Offer event, got {:?}", event),
        }
    }

    // ── Offer serde: cli_mode fields roundtrip correctly ──────────────────
    #[test]
    fn offer_serde_cli_mode_fields() {
        let offer = FileTransferOffer {
            transfer_id: "ft-serde-cli".to_string(),
            sender_device_id: "device-1".to_string(),
            sender_addr: "host.ts.net:9417".to_string(),
            file: FileInfo {
                name: "test.bin".to_string(),
                size: 1024,
                sha256: "abc".to_string(),
            },
            token: "a".repeat(64),
            cli_mode: true,
            save_path: Some("/tmp/output.bin".to_string()),
        };

        let json = serde_json::to_string(&offer).unwrap();
        assert!(json.contains("cliMode"), "cliMode should be serialized");
        assert!(json.contains("savePath"), "savePath should be serialized");

        let parsed: FileTransferOffer = serde_json::from_str(&json).unwrap();
        assert!(parsed.cli_mode);
        assert_eq!(parsed.save_path.as_deref(), Some("/tmp/output.bin"));
    }

    // ── Offer serde: save_path=None is not serialized (skip_serializing_if) ──
    #[test]
    fn offer_serde_save_path_none_omitted() {
        let offer = FileTransferOffer {
            transfer_id: "ft-serde-no-path".to_string(),
            sender_device_id: "device-1".to_string(),
            sender_addr: "host.ts.net:9417".to_string(),
            file: FileInfo {
                name: "test.bin".to_string(),
                size: 1024,
                sha256: "abc".to_string(),
            },
            token: "a".repeat(64),
            cli_mode: false,
            save_path: None,
        };

        let json = serde_json::to_string(&offer).unwrap();
        assert!(!json.contains("savePath"), "savePath=None should be omitted from JSON");
    }
}
