use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use tokio::sync::mpsc;

use truffle_core::file_transfer::adapter::{
    AdapterEvent, FileTransferAdapter as CoreAdapter, FileTransferAdapterConfig,
    FileTransferOffer,
};
use truffle_core::file_transfer::manager::FileTransferManager;
use truffle_core::file_transfer::types::{FileInfo, FileTransferConfig, FileTransferEvent};

// ═══════════════════════════════════════════════════════════════════════════
// NAPI object types for file transfer
// ═══════════════════════════════════════════════════════════════════════════

/// File transfer adapter configuration (JS representation).
#[napi(object)]
pub struct NapiFileTransferAdapterConfig {
    /// Local device ID.
    pub device_id: String,
    /// Local address for file transfers (e.g. "host.ts.net:9417").
    pub local_addr: String,
    /// Default output directory for received files.
    pub output_dir: String,
    /// Maximum file size in bytes.
    pub max_file_size: Option<f64>,
    /// Max concurrent incoming transfers.
    pub max_concurrent_recv: Option<u32>,
    /// Progress event interval in milliseconds.
    pub progress_interval_ms: Option<u32>,
    /// Progress event byte threshold.
    pub progress_bytes: Option<f64>,
}

/// File transfer offer (JS representation, for accept/reject).
#[napi(object)]
pub struct NapiFileTransferOffer {
    pub transfer_id: String,
    pub sender_device_id: String,
    pub sender_addr: String,
    pub file_name: String,
    pub file_size: f64,
    pub file_sha256: String,
    pub token: String,
}

/// Transfer info returned to JS.
#[napi(object)]
pub struct NapiAdapterTransferInfo {
    pub transfer_id: String,
    pub direction: String,
    pub state: String,
    pub peer_device_id: String,
    pub file_name: String,
    pub file_size: f64,
    pub bytes_transferred: f64,
    pub percent: f64,
    pub bytes_per_second: f64,
    pub eta: f64,
}

/// Outgoing bus message (sent from adapter to be relayed via mesh).
#[napi(object)]
pub struct NapiBusMessage {
    pub target_device_id: String,
    pub message_type: String,
    pub payload: String,
}

/// File transfer event delivered to JS.
#[napi(object)]
pub struct NapiFileTransferEvent {
    pub event_type: String,
    pub transfer_id: Option<String>,
    pub payload: serde_json::Value,
}

// ═══════════════════════════════════════════════════════════════════════════
// Conversions
// ═══════════════════════════════════════════════════════════════════════════

/// Convert an AdapterEvent to a NapiFileTransferEvent.
/// IMPORTANT: Must NEVER panic.
fn adapter_event_to_napi(event: &AdapterEvent) -> NapiFileTransferEvent {
    match event {
        AdapterEvent::Offer(offer) => NapiFileTransferEvent {
            event_type: "offer".to_string(),
            transfer_id: Some(offer.transfer_id.clone()),
            payload: serde_json::to_value(offer).unwrap_or(serde_json::Value::Null),
        },
        AdapterEvent::PreparingProgress(ft_event) => {
            ft_event_to_napi("preparingProgress", ft_event)
        }
        AdapterEvent::Progress(ft_event) => ft_event_to_napi("progress", ft_event),
        AdapterEvent::Completed(ft_event) => ft_event_to_napi("completed", ft_event),
        AdapterEvent::Failed(reason, ft_event) => {
            let mut napi = ft_event_to_napi("failed", ft_event);
            if let serde_json::Value::Object(ref mut map) = napi.payload {
                map.insert(
                    "reason".to_string(),
                    serde_json::Value::String(reason.clone()),
                );
            }
            napi
        }
        AdapterEvent::Cancelled(transfer_id) => NapiFileTransferEvent {
            event_type: "cancelled".to_string(),
            transfer_id: Some(transfer_id.clone()),
            payload: serde_json::Value::Null,
        },
    }
}

fn ft_event_to_napi(event_type: &str, event: &FileTransferEvent) -> NapiFileTransferEvent {
    let transfer_id = match event {
        FileTransferEvent::PreparingProgress { transfer_id, .. } => Some(transfer_id.clone()),
        FileTransferEvent::Prepared { transfer_id, .. } => Some(transfer_id.clone()),
        FileTransferEvent::Progress { transfer_id, .. } => Some(transfer_id.clone()),
        FileTransferEvent::Complete { transfer_id, .. } => Some(transfer_id.clone()),
        FileTransferEvent::Error { transfer_id, .. } => Some(transfer_id.clone()),
        FileTransferEvent::Cancelled { transfer_id } => Some(transfer_id.clone()),
        FileTransferEvent::List { .. } => None,
    };

    NapiFileTransferEvent {
        event_type: event_type.to_string(),
        transfer_id,
        payload: serde_json::to_value(event).unwrap_or(serde_json::Value::Null),
    }
}

fn napi_offer_to_core(offer: &NapiFileTransferOffer) -> FileTransferOffer {
    FileTransferOffer {
        transfer_id: offer.transfer_id.clone(),
        sender_device_id: offer.sender_device_id.clone(),
        sender_addr: offer.sender_addr.clone(),
        file: FileInfo {
            name: offer.file_name.clone(),
            size: offer.file_size as i64,
            sha256: offer.file_sha256.clone(),
        },
        token: offer.token.clone(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// NapiFileTransferAdapter
// ═══════════════════════════════════════════════════════════════════════════

/// NapiFileTransferAdapter - Node.js wrapper for file transfer signaling.
///
/// Handles file transfer offer/accept/reject/cancel via the mesh message bus.
/// Progress events use the broadcast channel (drops allowed for lag recovery).
#[napi]
pub struct NapiFileTransferAdapter {
    inner: Arc<CoreAdapter>,
    event_rx: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<AdapterEvent>>>,
    bus_rx: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<(String, String, String)>>>,
    manager_event_rx:
        tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<FileTransferEvent>>>,
}

#[napi]
impl NapiFileTransferAdapter {
    /// Create a new FileTransferAdapter.
    ///
    /// Sets up the underlying FileTransferManager and adapter with default TCP dial.
    #[napi(constructor)]
    pub fn new(config: NapiFileTransferAdapterConfig) -> Result<Self> {
        // Build FileTransferConfig from NAPI config
        let mut ft_config = FileTransferConfig::default();
        if let Some(max_size) = config.max_file_size {
            ft_config.max_file_size = max_size as i64;
        }
        if let Some(max_recv) = config.max_concurrent_recv {
            ft_config.max_concurrent_recv = max_recv as usize;
        }
        if let Some(interval_ms) = config.progress_interval_ms {
            ft_config.progress_interval = Duration::from_millis(interval_ms as u64);
        }
        if let Some(progress_bytes) = config.progress_bytes {
            ft_config.progress_bytes = progress_bytes as i64;
        }

        // Create manager event channel
        let (manager_event_tx, manager_event_rx) = mpsc::unbounded_channel();

        // Create the FileTransferManager
        let manager = FileTransferManager::new(ft_config, manager_event_tx);

        // Create bus channel for outgoing messages
        let (bus_tx, bus_rx) = mpsc::unbounded_channel();

        // Create adapter event channel
        let (adapter_event_tx, adapter_event_rx) = mpsc::unbounded_channel();

        // Default dial function: standard TCP connect
        let dial_fn: Arc<
            dyn Fn(
                    &str,
                )
                    -> std::pin::Pin<
                    Box<
                        dyn std::future::Future<Output = std::io::Result<tokio::net::TcpStream>>
                            + Send,
                    >,
                > + Send
                + Sync,
        > = Arc::new(|addr: &str| {
            let addr = addr.to_string();
            Box::pin(async move { tokio::net::TcpStream::connect(&addr).await })
        });

        let adapter_config = FileTransferAdapterConfig {
            local_device_id: config.device_id,
            local_addr: config.local_addr,
            output_dir: config.output_dir,
            dial_fn,
        };

        let adapter = CoreAdapter::new(adapter_config, manager, bus_tx, adapter_event_tx);

        Ok(Self {
            inner: adapter,
            event_rx: tokio::sync::Mutex::new(Some(adapter_event_rx)),
            bus_rx: tokio::sync::Mutex::new(Some(bus_rx)),
            manager_event_rx: tokio::sync::Mutex::new(Some(manager_event_rx)),
        })
    }

    /// Initiate sending a file to a target device.
    /// Returns the transfer ID.
    #[napi]
    pub async fn send_file(
        &self,
        target_device_id: String,
        file_path: String,
    ) -> Result<String> {
        let transfer_id = self.inner.send_file(&target_device_id, &file_path).await;
        Ok(transfer_id)
    }

    /// Accept an incoming file transfer offer.
    #[napi]
    pub async fn accept_transfer(
        &self,
        offer: NapiFileTransferOffer,
        save_path: Option<String>,
    ) -> Result<()> {
        let core_offer = napi_offer_to_core(&offer);
        self.inner
            .accept_transfer(&core_offer, save_path.as_deref())
            .await;
        Ok(())
    }

    /// Reject an incoming file transfer offer.
    #[napi]
    pub async fn reject_transfer(
        &self,
        offer: NapiFileTransferOffer,
        reason: String,
    ) -> Result<()> {
        let core_offer = napi_offer_to_core(&offer);
        self.inner.reject_transfer(&core_offer, &reason).await;
        Ok(())
    }

    /// Cancel an active transfer.
    #[napi]
    pub async fn cancel_transfer(&self, transfer_id: String) -> Result<()> {
        self.inner.cancel_transfer(&transfer_id).await;
        Ok(())
    }

    /// Get all tracked transfers.
    #[napi]
    pub async fn get_transfers(&self) -> Result<Vec<NapiAdapterTransferInfo>> {
        let transfers = self.inner.get_transfers().await;
        Ok(transfers
            .into_iter()
            .map(|t| NapiAdapterTransferInfo {
                transfer_id: t.transfer_id,
                direction: t.direction,
                state: t.state,
                peer_device_id: t.peer_device_id,
                file_name: t.file.name,
                file_size: t.file.size as f64,
                bytes_transferred: t.bytes_transferred as f64,
                percent: t.percent,
                bytes_per_second: t.bytes_per_second,
                eta: t.eta,
            })
            .collect())
    }

    /// Handle an incoming message from the mesh bus.
    /// Call this when a file-transfer namespace message arrives.
    #[napi]
    pub async fn handle_bus_message(
        &self,
        msg_type: String,
        payload: String,
    ) -> Result<()> {
        self.inner.handle_bus_message(&msg_type, &payload).await;
        Ok(())
    }

    /// Subscribe to adapter events (offers, progress, completed, failed, cancelled).
    ///
    /// Only one subscriber allowed. Progress events may be dropped if the JS
    /// handler is slow (broadcast channel). Critical events are never dropped.
    #[napi(ts_args_type = "callback: (err: null | Error, event: NapiFileTransferEvent) => void")]
    pub fn on_event(
        &self,
        callback: ThreadsafeFunction<NapiFileTransferEvent>,
    ) -> Result<()> {
        let mut guard = self
            .event_rx
            .try_lock()
            .map_err(|_| Error::from_reason("event receiver lock contention"))?;

        let rx = guard.take().ok_or_else(|| {
            Error::from_reason("on_event already called - only one subscriber allowed")
        })?;

        tokio::spawn(async move {
            Self::event_loop(rx, callback).await;
        });

        Ok(())
    }

    /// Subscribe to outgoing bus messages.
    ///
    /// The adapter generates outgoing messages (OFFER, ACCEPT, REJECT, CANCEL)
    /// that must be sent via the mesh message bus. This callback delivers them
    /// so the JS layer can relay them.
    #[napi(ts_args_type = "callback: (err: null | Error, message: NapiBusMessage) => void")]
    pub fn on_bus_message(
        &self,
        callback: ThreadsafeFunction<NapiBusMessage>,
    ) -> Result<()> {
        let mut guard = self
            .bus_rx
            .try_lock()
            .map_err(|_| Error::from_reason("bus receiver lock contention"))?;

        let rx = guard.take().ok_or_else(|| {
            Error::from_reason("on_bus_message already called - only one subscriber allowed")
        })?;

        tokio::spawn(async move {
            Self::bus_message_loop(rx, callback).await;
        });

        Ok(())
    }

    /// Start forwarding manager events to the adapter.
    ///
    /// The FileTransferManager emits events (progress, complete, error) that
    /// the adapter needs to process. Call this once after construction.
    #[napi]
    pub fn start_manager_events(&self) -> Result<()> {
        let mut guard = self
            .manager_event_rx
            .try_lock()
            .map_err(|_| Error::from_reason("manager event receiver lock contention"))?;

        let rx = guard.take().ok_or_else(|| {
            Error::from_reason(
                "start_manager_events already called - only one subscriber allowed",
            )
        })?;

        let adapter = Arc::clone(&self.inner);
        tokio::spawn(async move {
            Self::manager_event_loop(rx, adapter).await;
        });

        Ok(())
    }

    // --- Internal event loops ---

    async fn event_loop(
        mut rx: mpsc::UnboundedReceiver<AdapterEvent>,
        callback: ThreadsafeFunction<NapiFileTransferEvent>,
    ) {
        while let Some(event) = rx.recv().await {
            let napi_event = adapter_event_to_napi(&event);
            let status = callback.call(Ok(napi_event), ThreadsafeFunctionCallMode::Blocking);
            if status != Status::Ok {
                tracing::warn!("Failed to deliver file transfer event to JS: {:?}", status);
                if status == Status::Closing || status == Status::InvalidArg {
                    tracing::info!(
                        "File transfer event callback closed, stopping event loop"
                    );
                    break;
                }
            }
        }
        tracing::info!("File transfer event loop ended (channel closed)");
    }

    async fn bus_message_loop(
        mut rx: mpsc::UnboundedReceiver<(String, String, String)>,
        callback: ThreadsafeFunction<NapiBusMessage>,
    ) {
        while let Some((target_device_id, message_type, payload)) = rx.recv().await {
            let msg = NapiBusMessage {
                target_device_id,
                message_type,
                payload,
            };
            let status = callback.call(Ok(msg), ThreadsafeFunctionCallMode::Blocking);
            if status != Status::Ok {
                tracing::warn!("Failed to deliver bus message to JS: {:?}", status);
                if status == Status::Closing || status == Status::InvalidArg {
                    tracing::info!("Bus message callback closed, stopping loop");
                    break;
                }
            }
        }
        tracing::info!("Bus message loop ended (channel closed)");
    }

    async fn manager_event_loop(
        mut rx: mpsc::UnboundedReceiver<FileTransferEvent>,
        adapter: Arc<CoreAdapter>,
    ) {
        while let Some(event) = rx.recv().await {
            adapter.handle_manager_event(event).await;
        }
        tracing::info!("Manager event loop ended (channel closed)");
    }
}
