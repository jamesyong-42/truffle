//! NapiCrdtDoc — Node.js wrapper for the CrdtDoc subsystem.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Status, Unknown};
use napi_derive::napi;
use tokio::task::JoinHandle;

use truffle_core::crdt_doc::{CrdtDoc, CrdtDocEvent};
use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::Node;

use crate::types::NapiCrdtDocEvent;

/// A CRDT document synchronized across the mesh, exposed to JavaScript.
///
/// Obtained via `NapiNode.crdtDoc(docId)`. Each document wraps a Loro
/// `LoroDoc` with automatic peer-to-peer sync.
#[napi]
pub struct NapiCrdtDoc {
    inner: Arc<CrdtDoc>,
    task_handles: Vec<JoinHandle<()>>,
}

impl NapiCrdtDoc {
    /// Create a new `NapiCrdtDoc` synchronously.
    ///
    /// `CrdtDoc::new` is async and calls `tokio::spawn` internally. We enter
    /// the napi-rs managed Tokio runtime so the spawn hits a live reactor,
    /// mirroring the pattern used by `NapiSyncedStore::new`.
    pub(crate) fn new(node: Arc<Node<TailscaleProvider>>, doc_id: &str) -> napi::Result<Self> {
        let inner = napi::bindgen_prelude::within_runtime_if_available(|| {
            tokio::runtime::Handle::current().block_on(node.crdt_doc(doc_id))
        })
        .map_err(|e| Error::from_reason(format!("Failed to create CrdtDoc: {e}")))?;
        Ok(Self {
            inner,
            task_handles: Vec::new(),
        })
    }
}

#[napi]
impl NapiCrdtDoc {
    // ── Container access: Map ─────────────────────────────────────────

    /// Insert a key-value pair into a named map container.
    #[napi]
    pub fn map_insert(
        &self,
        container: String,
        key: String,
        value: serde_json::Value,
    ) -> Result<()> {
        self.inner.with_doc(|doc| {
            let map = doc.get_map(&*container);
            let loro_value = loro::LoroValue::from(value);
            map.insert(&key, loro_value)
                .map_err(|e| Error::from_reason(format!("map_insert failed: {e}")))
        })?;
        self.inner.commit();
        Ok(())
    }

    /// Delete a key from a named map container.
    #[napi]
    pub fn map_delete(&self, container: String, key: String) -> Result<()> {
        self.inner.with_doc(|doc| {
            let map = doc.get_map(&*container);
            map.delete(&key)
                .map_err(|e| Error::from_reason(format!("map_delete failed: {e}")))
        })?;
        self.inner.commit();
        Ok(())
    }

    // ── Container access: List ────────────────────────────────────────

    /// Push a value to the end of a named list container.
    #[napi]
    pub fn list_push(&self, container: String, value: serde_json::Value) -> Result<()> {
        self.inner.with_doc(|doc| {
            let list = doc.get_list(&*container);
            let loro_value = loro::LoroValue::from(value);
            list.push(loro_value)
                .map_err(|e| Error::from_reason(format!("list_push failed: {e}")))
        })?;
        self.inner.commit();
        Ok(())
    }

    /// Insert a value at a specific index in a named list container.
    #[napi]
    pub fn list_insert(
        &self,
        container: String,
        index: u32,
        value: serde_json::Value,
    ) -> Result<()> {
        self.inner.with_doc(|doc| {
            let list = doc.get_list(&*container);
            let loro_value = loro::LoroValue::from(value);
            list.insert(index as usize, loro_value)
                .map_err(|e| Error::from_reason(format!("list_insert failed: {e}")))
        })?;
        self.inner.commit();
        Ok(())
    }

    /// Delete elements from a named list container.
    #[napi]
    pub fn list_delete(&self, container: String, index: u32, len: u32) -> Result<()> {
        self.inner.with_doc(|doc| {
            let list = doc.get_list(&*container);
            list.delete(index as usize, len as usize)
                .map_err(|e| Error::from_reason(format!("list_delete failed: {e}")))
        })?;
        self.inner.commit();
        Ok(())
    }

    // ── Container access: Text ────────────────────────────────────────

    /// Insert a string at a unicode position in a named text container.
    #[napi]
    pub fn text_insert(&self, container: String, pos: u32, text: String) -> Result<()> {
        self.inner.with_doc(|doc| {
            let text_container = doc.get_text(&*container);
            text_container
                .insert(pos as usize, &text)
                .map_err(|e| Error::from_reason(format!("text_insert failed: {e}")))
        })?;
        self.inner.commit();
        Ok(())
    }

    /// Delete text at a unicode position in a named text container.
    #[napi]
    pub fn text_delete(&self, container: String, pos: u32, len: u32) -> Result<()> {
        self.inner.with_doc(|doc| {
            let text_container = doc.get_text(&*container);
            text_container
                .delete(pos as usize, len as usize)
                .map_err(|e| Error::from_reason(format!("text_delete failed: {e}")))
        })?;
        self.inner.commit();
        Ok(())
    }

    // ── Container access: Counter ─────────────────────────────────────

    /// Increment a named counter container by the given value.
    #[napi]
    pub fn counter_increment(&self, container: String, value: f64) -> Result<()> {
        self.inner.with_doc(|doc| {
            let counter = doc.get_counter(&*container);
            counter
                .increment(value)
                .map_err(|e| Error::from_reason(format!("counter_increment failed: {e}")))
        })?;
        self.inner.commit();
        Ok(())
    }

    // ── Document-level ────────────────────────────────────────────────

    /// Get the deep JSON value of the entire document.
    #[napi]
    pub fn get_deep_value(&self) -> Result<serde_json::Value> {
        let loro_value = self.inner.get_deep_value();
        serde_json::to_value(&loro_value)
            .map_err(|e| Error::from_reason(format!("get_deep_value serialization failed: {e}")))
    }

    /// Commit pending changes. Triggers sync to peers.
    #[napi]
    pub fn commit(&self) {
        self.inner.commit();
    }

    /// The document identifier.
    #[napi]
    pub fn doc_id(&self) -> String {
        self.inner.doc_id().to_string()
    }

    // ── Events ────────────────────────────────────────────────────────

    /// Subscribe to document change events.
    ///
    /// The callback receives `NapiCrdtDocEvent` objects whenever a local
    /// change is applied, a remote change is received, or a peer syncs.
    #[napi(ts_args_type = "callback: (event: CrdtDocEvent) => void")]
    pub fn on_change(
        &mut self,
        callback: ThreadsafeFunction<
            NapiCrdtDocEvent,
            Unknown<'static>,
            NapiCrdtDocEvent,
            Status,
            false,
        >,
    ) -> Result<()> {
        let mut rx = self.inner.subscribe();

        let handle = napi::bindgen_prelude::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let napi_event = convert_crdt_doc_event(&event);
                        let status =
                            callback.call(napi_event, ThreadsafeFunctionCallMode::NonBlocking);
                        if status != Status::Ok {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "crdt_doc on_change lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        self.task_handles.push(handle);

        Ok(())
    }

    // ── Lifecycle ─────────────────────────────────────────────────────

    /// Stop the document and cancel all event-forwarding tasks.
    ///
    /// # Safety
    /// This takes `&mut self` in an async context. The caller must ensure that
    /// no other calls are made on this NapiCrdtDoc while `stop()` is in progress.
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
// CrdtDocEvent conversion
// ---------------------------------------------------------------------------

fn convert_crdt_doc_event(event: &CrdtDocEvent) -> NapiCrdtDocEvent {
    match event {
        CrdtDocEvent::LocalChange => NapiCrdtDocEvent {
            event_type: "local_change".to_string(),
            peer_id: None,
        },
        CrdtDocEvent::RemoteChange { from } => NapiCrdtDocEvent {
            event_type: "remote_change".to_string(),
            peer_id: Some(from.clone()),
        },
        CrdtDocEvent::PeerSynced { peer_id } => NapiCrdtDocEvent {
            event_type: "peer_synced".to_string(),
            peer_id: Some(peer_id.clone()),
        },
        CrdtDocEvent::PeerLeft { peer_id } => NapiCrdtDocEvent {
            event_type: "peer_left".to_string(),
            peer_id: Some(peer_id.clone()),
        },
    }
}
