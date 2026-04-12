//! Persistence backends for CrdtDoc.

use std::path::PathBuf;

/// Backend for persisting CRDT document data across restarts.
///
/// Methods are synchronous — same reasoning as [`StoreBackend`](crate::synced_store::StoreBackend):
/// file I/O for binary blobs is fast enough that async overhead isn't warranted.
pub trait CrdtBackend: Send + Sync + 'static {
    /// Load the latest snapshot for a document, if any.
    fn load_snapshot(&self, doc_id: &str) -> Option<Vec<u8>>;

    /// Load all incremental updates since the last snapshot, in order.
    fn load_updates(&self, doc_id: &str) -> Vec<Vec<u8>>;

    /// Append an incremental update.
    fn save_update(&self, doc_id: &str, data: &[u8]);

    /// Save a snapshot and clear all prior updates.
    fn save_snapshot(&self, doc_id: &str, data: &[u8]);

    /// Remove all persisted data for a document.
    fn remove(&self, doc_id: &str);
}

// ---------------------------------------------------------------------------
// MemoryCrdtBackend — no persistence (default)
// ---------------------------------------------------------------------------

/// In-memory backend (no persistence). Default.
///
/// All methods are no-ops. Data lives only in the `CrdtDoc`'s in-memory
/// `LoroDoc` and is lost on process exit.
#[derive(Debug, Default)]
pub struct MemoryCrdtBackend;

impl CrdtBackend for MemoryCrdtBackend {
    fn load_snapshot(&self, _doc_id: &str) -> Option<Vec<u8>> {
        None
    }

    fn load_updates(&self, _doc_id: &str) -> Vec<Vec<u8>> {
        Vec::new()
    }

    fn save_update(&self, _doc_id: &str, _data: &[u8]) {}

    fn save_snapshot(&self, _doc_id: &str, _data: &[u8]) {}

    fn remove(&self, _doc_id: &str) {}
}

// ---------------------------------------------------------------------------
// CrdtFileBackend — snapshot.bin + updates/{000001..N}.bin
// ---------------------------------------------------------------------------

/// File-backed persistence for CRDT documents.
///
/// Directory layout per document:
/// ```text
/// {base_dir}/{doc_id}/snapshot.bin
/// {base_dir}/{doc_id}/updates/000001.bin
/// {base_dir}/{doc_id}/updates/000002.bin
/// ...
/// ```
///
/// `save_snapshot` atomically writes the snapshot (write to `.tmp`, then
/// rename) and then removes the `updates/` directory since the snapshot
/// subsumes all prior updates.
pub struct CrdtFileBackend {
    base_dir: PathBuf,
}

impl CrdtFileBackend {
    /// Create a new `CrdtFileBackend` rooted at `base_dir`.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// Document directory: `{base_dir}/{sanitized_doc_id}`.
    fn doc_dir(&self, doc_id: &str) -> PathBuf {
        self.base_dir.join(sanitize(doc_id))
    }

    /// Snapshot path: `{doc_dir}/snapshot.bin`.
    fn snapshot_path(&self, doc_id: &str) -> PathBuf {
        self.doc_dir(doc_id).join("snapshot.bin")
    }

    /// Updates directory: `{doc_dir}/updates/`.
    fn updates_dir(&self, doc_id: &str) -> PathBuf {
        self.doc_dir(doc_id).join("updates")
    }
}

/// Replace characters that could cause path traversal or filesystem issues.
fn sanitize(s: &str) -> String {
    let replaced = s.replace('/', "_").replace('\\', "_").replace("..", "__");
    if replaced.is_empty() || replaced == "." {
        "_".to_string()
    } else {
        replaced
    }
}

impl CrdtBackend for CrdtFileBackend {
    fn load_snapshot(&self, doc_id: &str) -> Option<Vec<u8>> {
        let path = self.snapshot_path(doc_id);
        std::fs::read(&path).ok()
    }

    fn load_updates(&self, doc_id: &str) -> Vec<Vec<u8>> {
        let dir = self.updates_dir(doc_id);
        let mut entries: Vec<_> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .map(|ext| ext == "bin")
                        .unwrap_or(false)
                })
                .collect(),
            Err(_) => return Vec::new(),
        };

        // Sort by filename to preserve insertion order.
        entries.sort_by_key(|e| e.file_name());

        entries
            .into_iter()
            .filter_map(|e| std::fs::read(e.path()).ok())
            .collect()
    }

    fn save_update(&self, doc_id: &str, data: &[u8]) {
        let dir = self.updates_dir(doc_id);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::error!(
                path = %dir.display(),
                "crdt_file_backend: failed to create updates directory: {e}"
            );
            return;
        }

        // Determine next sequence number from the max existing .bin file number.
        let next_seq = match std::fs::read_dir(&dir) {
            Ok(rd) => {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if name.ends_with(".bin") {
                            name.trim_end_matches(".bin").parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .max()
                    .unwrap_or(0)
                    + 1
            }
            Err(_) => 1,
        };

        let path = dir.join(format!("{next_seq:06}.bin"));
        if let Err(e) = std::fs::write(&path, data) {
            tracing::error!(
                path = %path.display(),
                "crdt_file_backend: failed to write update: {e}"
            );
        }
    }

    fn save_snapshot(&self, doc_id: &str, data: &[u8]) {
        let doc_dir = self.doc_dir(doc_id);
        if let Err(e) = std::fs::create_dir_all(&doc_dir) {
            tracing::error!(
                path = %doc_dir.display(),
                "crdt_file_backend: failed to create doc directory: {e}"
            );
            return;
        }

        let path = self.snapshot_path(doc_id);
        let tmp_path = path.with_extension("bin.tmp");

        // Atomic write: write to .tmp then rename.
        if let Err(e) = std::fs::write(&tmp_path, data) {
            tracing::error!(
                path = %tmp_path.display(),
                "crdt_file_backend: failed to write snapshot tmp: {e}"
            );
            return;
        }
        if let Err(e) = std::fs::rename(&tmp_path, &path) {
            tracing::error!(
                from = %tmp_path.display(),
                to = %path.display(),
                "crdt_file_backend: failed to rename snapshot tmp: {e}"
            );
            return;
        }

        // Clear updates directory — the snapshot subsumes them.
        let updates_dir = self.updates_dir(doc_id);
        if updates_dir.exists() {
            let _ = std::fs::remove_dir_all(&updates_dir);
        }
    }

    fn remove(&self, doc_id: &str) {
        let doc_dir = self.doc_dir(doc_id);
        let _ = std::fs::remove_dir_all(&doc_dir);
    }
}
