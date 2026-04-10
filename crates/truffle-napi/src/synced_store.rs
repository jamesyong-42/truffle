//! NapiSyncedStore — Node.js wrapper for the SyncedStore subsystem.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use tokio::task::JoinHandle;

use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::synced_store::{StoreEvent, SyncedStore};
use truffle_core::Node;

use crate::types::{NapiSlice, NapiStoreEvent};

/// Synchronized store handle exposed to JavaScript.
///
/// Obtained via `NapiNode.syncedStore(storeId)`. Each store instance manages
/// device-owned slices of JSON data that are automatically synchronized
/// across the mesh.
#[napi]
pub struct NapiSyncedStore {
    inner: Arc<SyncedStore<serde_json::Value>>,
    task_handles: Vec<JoinHandle<()>>,
}

impl NapiSyncedStore {
    pub(crate) fn new(node: Arc<Node<TailscaleProvider>>, store_id: &str) -> Self {
        Self {
            inner: SyncedStore::new(node, store_id),
            task_handles: Vec::new(),
        }
    }
}

#[napi]
impl NapiSyncedStore {
    /// Update this device's data in the store.
    #[napi]
    pub async fn set(&self, data: serde_json::Value) -> Result<()> {
        self.inner.set(data).await;
        Ok(())
    }

    /// Get this device's current data, or `null` if `set()` hasn't been called.
    #[napi]
    pub async fn local(&self) -> Result<Option<serde_json::Value>> {
        Ok(self.inner.local().await)
    }

    /// Get a specific peer's slice by device ID.
    #[napi]
    pub async fn get(&self, device_id: String) -> Result<Option<NapiSlice>> {
        Ok(self.inner.get(&device_id).await.map(|s| NapiSlice {
            device_id: s.device_id,
            data: s.data,
            version: s.version as f64,
            updated_at: s.updated_at as f64,
        }))
    }

    /// Get all slices (local + remote) as an array.
    #[napi]
    pub async fn all(&self) -> Result<Vec<NapiSlice>> {
        let map = self.inner.all().await;
        Ok(map
            .into_values()
            .map(|s| NapiSlice {
                device_id: s.device_id,
                data: s.data,
                version: s.version as f64,
                updated_at: s.updated_at as f64,
            })
            .collect())
    }

    /// Get all device IDs that have data in this store.
    #[napi]
    pub async fn device_ids(&self) -> Result<Vec<String>> {
        Ok(self.inner.device_ids().await)
    }

    /// The store identifier.
    #[napi]
    pub fn store_id(&self) -> String {
        self.inner.store_id().to_string()
    }

    /// Current local version number.
    #[napi]
    pub fn version(&self) -> f64 {
        self.inner.version() as f64
    }

    /// Subscribe to store change events.
    ///
    /// The callback receives `NapiStoreEvent` objects whenever local data
    /// changes, a peer's data is updated, or a peer is removed.
    #[napi(ts_args_type = "callback: (event: StoreEvent) => void")]
    pub fn on_change(&mut self, callback: ThreadsafeFunction<NapiStoreEvent>) -> Result<()> {
        let mut rx = self.inner.subscribe();

        let handle = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let napi_event = convert_store_event(&event);
                        let status =
                            callback.call(Ok(napi_event), ThreadsafeFunctionCallMode::NonBlocking);
                        if status != Status::Ok {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "on_change lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        self.task_handles.push(handle);

        Ok(())
    }

    /// Stop the store and cancel all event-forwarding tasks.
    ///
    /// # Safety
    /// This takes `&mut self` in an async context. The caller must ensure that
    /// no other calls are made on this NapiSyncedStore while `stop()` is in progress.
    #[napi]
    pub async unsafe fn stop(&mut self) -> Result<()> {
        for handle in self.task_handles.drain(..) {
            handle.abort();
        }
        self.inner.stop().await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// StoreEvent conversion
// ---------------------------------------------------------------------------

fn convert_store_event(event: &StoreEvent<serde_json::Value>) -> NapiStoreEvent {
    match event {
        StoreEvent::LocalChanged(data) => NapiStoreEvent {
            event_type: "local_changed".to_string(),
            device_id: None,
            data: Some(data.clone()),
            version: None,
        },
        StoreEvent::PeerUpdated {
            device_id,
            data,
            version,
        } => NapiStoreEvent {
            event_type: "peer_updated".to_string(),
            device_id: Some(device_id.clone()),
            data: Some(data.clone()),
            version: Some(*version as f64),
        },
        StoreEvent::PeerRemoved { device_id } => NapiStoreEvent {
            event_type: "peer_removed".to_string(),
            device_id: Some(device_id.clone()),
            data: None,
            version: None,
        },
    }
}
