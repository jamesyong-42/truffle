//! Persistence backends for SyncedStore.

/// Backend for persisting store data across restarts.
///
/// Methods are synchronous — file I/O for small JSON payloads is fast enough
/// that async overhead isn't warranted. Apps needing async persistence (e.g.,
/// database-backed) can spawn a blocking task internally.
pub trait StoreBackend: Send + Sync + 'static {
    /// Load a device's slice. Returns `(serialized_data, version)` or `None`.
    fn load(&self, store_id: &str, device_id: &str) -> Option<(Vec<u8>, u64)>;

    /// Save a device's slice.
    fn save(&self, store_id: &str, device_id: &str, data: &[u8], version: u64);

    /// Remove a device's slice.
    fn remove(&self, store_id: &str, device_id: &str);
}

/// In-memory backend (no persistence). Default.
///
/// Data lives only in the `SyncedStore`'s in-memory state and is lost on
/// process exit. Suitable for ephemeral stores like presence or typing
/// indicators.
#[derive(Debug, Default)]
pub struct MemoryBackend;

impl StoreBackend for MemoryBackend {
    fn load(&self, _store_id: &str, _device_id: &str) -> Option<(Vec<u8>, u64)> {
        None
    }

    fn save(&self, _store_id: &str, _device_id: &str, _data: &[u8], _version: u64) {}

    fn remove(&self, _store_id: &str, _device_id: &str) {}
}
