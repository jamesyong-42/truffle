use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use tokio::sync::mpsc;

use truffle_core::store_sync::adapter::{StoreSyncAdapter as CoreAdapter, StoreSyncAdapterConfig};
use truffle_core::store_sync::types::{DeviceSlice, OutgoingSyncMessage, SyncMessage};

// ═══════════════════════════════════════════════════════════════════════════
// NAPI object types for store sync
// ═══════════════════════════════════════════════════════════════════════════

/// Store sync configuration (JS representation).
#[napi(object)]
pub struct NapiStoreSyncConfig {
    pub local_device_id: String,
}

/// Outgoing sync message delivered to JS for broadcasting.
#[napi(object)]
pub struct NapiOutgoingSyncMessage {
    pub msg_type: String,
    pub payload: serde_json::Value,
}

/// Device slice (JS representation).
#[napi(object)]
pub struct NapiDeviceSlice {
    pub device_id: String,
    pub data: serde_json::Value,
    pub updated_at: f64,
    pub version: f64,
}

// ═══════════════════════════════════════════════════════════════════════════
// NapiStoreSyncAdapter
// ═══════════════════════════════════════════════════════════════════════════

/// NapiStoreSyncAdapter - Node.js wrapper for cross-device state sync.
///
/// In the JS layer, stores are registered via the SyncableStore trait.
/// For NAPI, we expose the adapter's lifecycle and message handling.
#[napi]
pub struct NapiStoreSyncAdapter {
    inner: Arc<CoreAdapter>,
    outgoing_rx: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<OutgoingSyncMessage>>>,
}

#[napi]
impl NapiStoreSyncAdapter {
    /// Create a new StoreSyncAdapter.
    ///
    /// Note: In the NAPI layer, stores must be registered separately.
    /// This constructor creates the adapter with an empty store list.
    #[napi(constructor)]
    pub fn new(config: NapiStoreSyncConfig) -> Result<Self> {
        let (outgoing_tx, outgoing_rx) = mpsc::unbounded_channel();

        let core_config = StoreSyncAdapterConfig {
            local_device_id: config.local_device_id,
        };

        let inner = CoreAdapter::new(core_config, vec![], outgoing_tx);

        Ok(Self {
            inner,
            outgoing_rx: tokio::sync::Mutex::new(Some(outgoing_rx)),
        })
    }

    /// Start syncing.
    #[napi]
    pub async fn start(&self) -> Result<()> {
        self.inner.start().await;
        Ok(())
    }

    /// Stop syncing.
    #[napi]
    pub async fn stop(&self) -> Result<()> {
        self.inner.stop().await;
        Ok(())
    }

    /// Dispose the adapter.
    #[napi]
    pub async fn dispose(&self) -> Result<()> {
        self.inner.dispose().await;
        Ok(())
    }

    /// Handle an incoming sync message from a remote device.
    #[napi]
    pub async fn handle_sync_message(
        &self,
        from: Option<String>,
        msg_type: String,
        payload: serde_json::Value,
    ) -> Result<()> {
        let message = SyncMessage {
            from,
            msg_type,
            payload,
        };
        self.inner.handle_sync_message(&message).await;
        Ok(())
    }

    /// Handle a device going offline.
    #[napi]
    pub async fn handle_device_offline(&self, device_id: String) -> Result<()> {
        self.inner.handle_device_offline(&device_id).await;
        Ok(())
    }

    /// Handle a new device being discovered.
    #[napi]
    pub async fn handle_device_discovered(&self, device_id: String) -> Result<()> {
        self.inner.handle_device_discovered(&device_id).await;
        Ok(())
    }

    /// Notify that a local store changed (triggers sync broadcast).
    #[napi]
    pub async fn handle_local_changed(&self, store_id: String, slice: NapiDeviceSlice) -> Result<()> {
        let core_slice = DeviceSlice {
            device_id: slice.device_id,
            data: slice.data,
            updated_at: slice.updated_at as u64,
            version: slice.version as u64,
        };
        self.inner.handle_local_changed(&store_id, &core_slice).await;
        Ok(())
    }

    /// Subscribe to outgoing sync messages.
    ///
    /// The callback receives messages that should be broadcast to all devices
    /// via the mesh message bus.
    #[napi(ts_args_type = "callback: (err: null | Error, message: NapiOutgoingSyncMessage) => void")]
    pub fn on_outgoing(
        &self,
        callback: ThreadsafeFunction<NapiOutgoingSyncMessage>,
    ) -> Result<()> {
        let mut guard = self
            .outgoing_rx
            .try_lock()
            .map_err(|_| Error::from_reason("outgoing receiver lock contention"))?;

        let rx = guard.take().ok_or_else(|| {
            Error::from_reason("on_outgoing already called - only one subscriber allowed")
        })?;

        tokio::spawn(async move {
            Self::outgoing_loop(rx, callback).await;
        });

        Ok(())
    }

    async fn outgoing_loop(
        mut rx: mpsc::UnboundedReceiver<OutgoingSyncMessage>,
        callback: ThreadsafeFunction<NapiOutgoingSyncMessage>,
    ) {
        while let Some(msg) = rx.recv().await {
            let napi_msg = NapiOutgoingSyncMessage {
                msg_type: msg.msg_type,
                payload: msg.payload,
            };
            let status = callback.call(Ok(napi_msg), ThreadsafeFunctionCallMode::Blocking);
            if status != Status::Ok {
                tracing::warn!("Failed to deliver outgoing sync message to JS: {:?}", status);
                if status == Status::Closing || status == Status::InvalidArg {
                    break;
                }
            }
        }
        tracing::info!("Store sync outgoing loop ended (channel closed)");
    }
}
