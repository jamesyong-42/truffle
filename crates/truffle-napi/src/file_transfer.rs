use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use tokio::sync::mpsc;

use truffle_core::file_transfer::adapter::AdapterEvent;
use truffle_core::file_transfer::types::FileTransferEvent;

// ═══════════════════════════════════════════════════════════════════════════
// NAPI object types for file transfer
// ═══════════════════════════════════════════════════════════════════════════

/// File transfer configuration (JS representation).
#[napi(object)]
pub struct NapiFileTransferConfig {
    pub max_file_size: Option<f64>,
    pub max_concurrent_recv: Option<u32>,
    pub progress_interval_ms: Option<u32>,
    pub progress_bytes: Option<f64>,
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
        AdapterEvent::PreparingProgress(ft_event) => ft_event_to_napi("preparingProgress", ft_event),
        AdapterEvent::Progress(ft_event) => ft_event_to_napi("progress", ft_event),
        AdapterEvent::Completed(ft_event) => ft_event_to_napi("completed", ft_event),
        AdapterEvent::Failed(reason, ft_event) => {
            let mut napi = ft_event_to_napi("failed", ft_event);
            if let serde_json::Value::Object(ref mut map) = napi.payload {
                map.insert("reason".to_string(), serde_json::Value::String(reason.clone()));
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

// ═══════════════════════════════════════════════════════════════════════════
// NapiFileTransferAdapter
// ═══════════════════════════════════════════════════════════════════════════

/// NapiFileTransferAdapter - Node.js wrapper for file transfer signaling.
///
/// Handles file transfer offer/accept/reject/cancel via the mesh message bus.
/// Progress events use the broadcast channel (drops allowed for lag recovery).
#[napi]
pub struct NapiFileTransferAdapter {
    _inner: Arc<()>,
    event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<AdapterEvent>>>,
}

#[napi]
impl NapiFileTransferAdapter {
    /// Subscribe to file transfer events.
    ///
    /// Progress events may be dropped if the JS handler is slow (broadcast channel).
    /// Critical events (offer, completed, failed, cancelled) are never dropped.
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

    async fn event_loop(
        mut rx: mpsc::Receiver<AdapterEvent>,
        callback: ThreadsafeFunction<NapiFileTransferEvent>,
    ) {
        while let Some(event) = rx.recv().await {
            let napi_event = adapter_event_to_napi(&event);
            let status = callback.call(Ok(napi_event), ThreadsafeFunctionCallMode::Blocking);
            if status != Status::Ok {
                tracing::warn!("Failed to deliver file transfer event to JS: {:?}", status);
                if status == Status::Closing || status == Status::InvalidArg {
                    tracing::info!("File transfer event callback closed, stopping event loop");
                    break;
                }
            }
        }
        tracing::info!("File transfer event loop ended (channel closed)");
    }
}
