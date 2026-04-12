//! CrdtDoc — CRDT document synchronization across the mesh.
//!
//! Wraps a [Loro](https://loro.dev) `LoroDoc` with automatic peer-to-peer
//! sync via the truffle mesh. Each document has a unique ID and syncs on
//! namespace `"crdt:{doc_id}"`.
//!
//! # Quick start
//!
//! ```ignore
//! use std::sync::Arc;
//! use truffle_core::crdt_doc::CrdtDoc;
//!
//! let node = Arc::new(node);
//! let doc = CrdtDoc::new(node, "my-doc").await.unwrap();
//!
//! // Get a Map container and insert a value.
//! let map = doc.map("root");
//! doc.with_doc(|d| {
//!     map.insert_with_doc(d, "key", "value").unwrap();
//! });
//! doc.commit();
//! ```

pub mod backend;
pub mod sync;
pub mod types;

#[cfg(test)]
mod tests;

pub use backend::{CrdtBackend, CrdtFileBackend, MemoryCrdtBackend};
pub use types::{CrdtDocError, CrdtDocEvent, CrdtSyncMessage};

use std::sync::Arc;

use loro::LoroDoc;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::network::NetworkProvider;
use crate::node::Node;

// ---------------------------------------------------------------------------
// CrdtDoc — public API
// ---------------------------------------------------------------------------

/// A CRDT document synchronized across the mesh.
///
/// Wraps a Loro `LoroDoc` with automatic background sync. Local changes
/// are broadcast to peers; remote changes are merged automatically via
/// Loro's CRDT algorithms.
///
/// Created via [`CrdtDoc::new`] or [`Node::crdt_doc`].
pub struct CrdtDoc {
    /// Document identifier.
    doc_id: String,
    /// The underlying Loro document, behind a std::sync::Mutex.
    ///
    /// We use `std::sync::Mutex` (not `tokio::sync::Mutex`) because
    /// `LoroDoc` is not `Send` across `.await` points — we always lock,
    /// operate, and drop the guard synchronously before any `.await`.
    doc: Arc<std::sync::Mutex<LoroDoc>>,
    /// Event broadcast channel.
    event_tx: broadcast::Sender<CrdtDocEvent>,
    /// Handle to the background sync task.
    task_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    /// Persistence backend.
    backend: Arc<dyn CrdtBackend>,
    /// Channel to notify the sync task that a manual compaction occurred,
    /// so it can reset `accumulated_update_bytes`.
    compact_tx: mpsc::UnboundedSender<()>,
    /// Subscription from Loro's `subscribe_local_update`. MUST be kept
    /// alive for the duration of this CrdtDoc — dropping it unsubscribes.
    _local_update_sub: loro::Subscription,
}

impl CrdtDoc {
    /// Create a new CrdtDoc and start the background sync task.
    ///
    /// Uses the default [`MemoryCrdtBackend`] (no persistence).
    pub async fn new<N: NetworkProvider + 'static>(
        node: Arc<Node<N>>,
        doc_id: &str,
    ) -> Result<Arc<Self>, CrdtDocError> {
        Self::new_with_backend(node, doc_id, Arc::new(MemoryCrdtBackend)).await
    }

    /// Create a new CrdtDoc with a custom persistence backend.
    ///
    /// On startup, attempts to restore from the backend: first loads the
    /// snapshot (if any), then replays incremental updates on top.
    pub async fn new_with_backend<N: NetworkProvider + 'static>(
        node: Arc<Node<N>>,
        doc_id: &str,
        backend: Arc<dyn CrdtBackend>,
    ) -> Result<Arc<Self>, CrdtDocError> {
        let doc = LoroDoc::new();

        // Restore from backend if available.
        if let Some(snapshot) = backend.load_snapshot(doc_id) {
            doc.import(&snapshot)
                .map_err(|e| CrdtDocError::Decode(format!("failed to import snapshot: {e}")))?;
        }
        for update in backend.load_updates(doc_id) {
            doc.import(&update)
                .map_err(|e| CrdtDocError::Decode(format!("failed to import update: {e}")))?;
        }

        let doc = Arc::new(std::sync::Mutex::new(doc));
        let (event_tx, _) = broadcast::channel(256);
        let (local_update_tx, local_update_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (compact_tx, compact_rx) = mpsc::unbounded_channel::<()>();

        // Subscribe to local updates from Loro. The callback sends bytes
        // into the mpsc channel which the sync task consumes.
        let sub = {
            let d = doc.lock().unwrap();
            d.subscribe_local_update(Box::new(move |bytes| {
                let _ = local_update_tx.send(bytes.to_vec());
                true // return true to keep the subscription alive
            }))
        };

        // Spawn the background sync task.
        let task_handle = sync::spawn_sync_task(
            node,
            doc.clone(),
            doc_id.to_string(),
            backend.clone(),
            event_tx.clone(),
            local_update_rx,
            compact_rx,
        );

        Ok(Arc::new(Self {
            doc_id: doc_id.to_string(),
            doc,
            event_tx,
            task_handle: tokio::sync::Mutex::new(Some(task_handle)),
            backend,
            compact_tx,
            _local_update_sub: sub,
        }))
    }

    // ── Container accessors ────────────────────────────────────────────

    /// Get a `LoroMap` container by name.
    pub fn map(&self, name: &str) -> loro::LoroMap {
        let doc = self.doc.lock().unwrap();
        doc.get_map(name)
    }

    /// Get a `LoroList` container by name.
    pub fn list(&self, name: &str) -> loro::LoroList {
        let doc = self.doc.lock().unwrap();
        doc.get_list(name)
    }

    /// Get a `LoroText` container by name.
    pub fn text(&self, name: &str) -> loro::LoroText {
        let doc = self.doc.lock().unwrap();
        doc.get_text(name)
    }

    /// Get a `LoroTree` container by name.
    pub fn tree(&self, name: &str) -> loro::LoroTree {
        let doc = self.doc.lock().unwrap();
        doc.get_tree(name)
    }

    /// Get a `LoroMovableList` container by name.
    pub fn movable_list(&self, name: &str) -> loro::LoroMovableList {
        let doc = self.doc.lock().unwrap();
        doc.get_movable_list(name)
    }

    /// Get a `LoroCounter` container by name.
    pub fn counter(&self, name: &str) -> loro::LoroCounter {
        let doc = self.doc.lock().unwrap();
        doc.get_counter(name)
    }

    // ── Document operations ────────────────────────────────────────────

    /// Get the deep JSON value of the entire document.
    pub fn get_deep_value(&self) -> loro::LoroValue {
        let doc = self.doc.lock().unwrap();
        doc.get_deep_value()
    }

    /// Commit pending changes. This triggers the `subscribe_local_update`
    /// callback, which feeds the sync task.
    pub fn commit(&self) {
        let doc = self.doc.lock().unwrap();
        doc.commit();
    }

    /// Export the document as a full snapshot.
    pub fn export_snapshot(&self) -> Result<Vec<u8>, CrdtDocError> {
        let doc = self.doc.lock().unwrap();
        doc.export(loro::ExportMode::Snapshot)
            .map_err(|e| CrdtDocError::Encode(format!("snapshot export failed: {e}")))
    }

    /// Export incremental updates since the given version vector.
    pub fn export_updates_since(&self, vv: &loro::VersionVector) -> Result<Vec<u8>, CrdtDocError> {
        let doc = self.doc.lock().unwrap();
        doc.export(loro::ExportMode::updates(vv))
            .map_err(|e| CrdtDocError::Encode(format!("updates export failed: {e}")))
    }

    /// Import binary data (snapshot or updates) into the document.
    pub fn import(&self, data: &[u8]) -> Result<(), CrdtDocError> {
        let doc = self.doc.lock().unwrap();
        doc.import(data)
            .map(|_| ())
            .map_err(|e| CrdtDocError::Decode(format!("import failed: {e}")))
    }

    /// Get the current version vector of the document.
    pub fn version_vector(&self) -> loro::VersionVector {
        let doc = self.doc.lock().unwrap();
        doc.state_vv()
    }

    /// Subscribe to document events.
    pub fn subscribe(&self) -> broadcast::Receiver<CrdtDocEvent> {
        self.event_tx.subscribe()
    }

    /// The document identifier.
    pub fn doc_id(&self) -> &str {
        &self.doc_id
    }

    /// Compact the document: export a snapshot and save it to the backend,
    /// clearing incremental updates.
    pub fn compact(&self) -> Result<(), CrdtDocError> {
        let snapshot = {
            let doc = self.doc.lock().unwrap();
            doc.export(loro::ExportMode::Snapshot)
                .map_err(|e| CrdtDocError::Encode(format!("compact snapshot failed: {e}")))?
        };
        self.backend.save_snapshot(&self.doc_id, &snapshot);
        // Notify the sync task so it resets accumulated_update_bytes.
        let _ = self.compact_tx.send(());
        Ok(())
    }

    /// Checkout the document to a specific version (frontiers).
    ///
    /// The document becomes "detached" — local edits are not allowed until
    /// [`checkout_to_latest`](Self::checkout_to_latest) is called.
    pub fn checkout(&self, frontiers: &loro::Frontiers) -> Result<(), CrdtDocError> {
        let doc = self.doc.lock().unwrap();
        doc.checkout(frontiers)
            .map_err(|e| CrdtDocError::Loro(format!("checkout failed: {e}")))
    }

    /// Return to the latest version after a `checkout`.
    pub fn checkout_to_latest(&self) {
        let doc = self.doc.lock().unwrap();
        doc.checkout_to_latest();
    }

    /// Whether the document is in "detached" mode (checked out to a
    /// historical version).
    pub fn is_detached(&self) -> bool {
        let doc = self.doc.lock().unwrap();
        doc.is_detached()
    }

    /// Fork the document — create an independent `LoroDoc` clone.
    pub fn fork(&self) -> Result<LoroDoc, CrdtDocError> {
        let doc = self.doc.lock().unwrap();
        Ok(doc.fork())
    }

    /// Access the underlying LoroDoc behind the mutex.
    ///
    /// The closure runs with the mutex held. Do NOT call any async
    /// functions inside the closure.
    pub fn with_doc<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&LoroDoc) -> R,
    {
        let doc = self.doc.lock().unwrap();
        f(&doc)
    }

    // ── Lifecycle ──────────────────────────────────────────────────────

    /// Stop the background sync task.
    pub async fn stop(&self) {
        let mut handle = self.task_handle.lock().await;
        if let Some(h) = handle.take() {
            h.abort();
            tracing::info!(doc_id = self.doc_id.as_str(), "crdt_doc: stopped");
        }
    }
}

impl Drop for CrdtDoc {
    fn drop(&mut self) {
        // Best-effort abort on drop.
        if let Ok(mut handle) = self.task_handle.try_lock() {
            if let Some(h) = handle.take() {
                h.abort();
            }
        }
    }
}
