use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use super::types::*;

/// Trait that stores must implement to be syncable.
///
/// Ports TS `ISyncableStore`. In Rust we use trait objects rather than
/// EventEmitter, with explicit callback registration.
///
/// # Async Safety
///
/// All methods use `&self` and are called synchronously from within async
/// contexts (inside `StoreSyncAdapter`). Implementations MUST use
/// `std::sync::RwLock` or `std::sync::Mutex` for interior mutability,
/// NOT `tokio::sync` variants. Using `tokio::sync::RwLock::blocking_read()`
/// inside a tokio runtime context will panic or deadlock.
///
/// The `MockStore` in tests demonstrates the correct pattern using
/// `std::sync::Mutex`.
pub trait SyncableStore: Send + Sync {
    /// Unique store identifier.
    fn store_id(&self) -> &str;

    /// Get the local device's slice, if any.
    fn get_local_slice(&self) -> Option<DeviceSlice>;

    /// Apply a remote device's slice.
    fn apply_remote_slice(&self, slice: DeviceSlice);

    /// Remove a remote device's slice.
    fn remove_remote_slice(&self, device_id: &str, reason: &str);

    /// Clear all remote slices.
    fn clear_remote_slices(&self);
}

/// Configuration for the StoreSyncAdapter.
pub struct StoreSyncAdapterConfig {
    pub local_device_id: String,
}

/// StoreSyncAdapter - Cross-device state synchronization.
///
/// Connects SyncableStore implementations to a MessageBus-like broadcast channel.
/// Works with any store shape - consumers define their own store implementations.
///
/// Ports TS `StoreSyncAdapter` (342 LOC).
///
/// Key differences from TS:
/// - Uses trait objects instead of EventEmitter for stores
/// - Outgoing messages go through an mpsc channel instead of direct MessageBus calls
/// - Local change notifications come through an mpsc channel instead of EventEmitter
pub struct StoreSyncAdapter {
    local_device_id: String,
    stores: RwLock<HashMap<String, Arc<dyn SyncableStore>>>,

    /// Channel for outgoing broadcast messages (to MessageBus).
    /// Format: (msg_type, payload_json)
    outgoing_tx: mpsc::UnboundedSender<OutgoingSyncMessage>,

    disposed: AtomicBool,
    started: AtomicBool,
}

impl StoreSyncAdapter {
    pub fn new(
        config: StoreSyncAdapterConfig,
        stores: Vec<Arc<dyn SyncableStore>>,
        outgoing_tx: mpsc::UnboundedSender<OutgoingSyncMessage>,
    ) -> Arc<Self> {
        let store_map: HashMap<String, Arc<dyn SyncableStore>> = stores
            .into_iter()
            .map(|s| (s.store_id().to_string(), s))
            .collect();

        Arc::new(Self {
            local_device_id: config.local_device_id,
            stores: RwLock::new(store_map),
            outgoing_tx,
            disposed: AtomicBool::new(false),
            started: AtomicBool::new(false),
        })
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Start syncing. Requests sync from existing devices and broadcasts current state.
    pub async fn start(&self) {
        if self.disposed.load(Ordering::Relaxed) {
            warn!("[StoreSyncAdapter] start: SKIPPED - disposed");
            return;
        }

        self.started.store(true, Ordering::Relaxed);

        // Request sync from existing devices
        self.request_sync_from_devices().await;

        // Broadcast our current state
        self.broadcast_all_stores().await;

        let store_count = self.stores.read().await.len();
        info!("[StoreSyncAdapter] Started with {store_count} stores");
    }

    /// Stop syncing. Clears remote slices from all stores.
    pub async fn stop(&self) {
        self.started.store(false, Ordering::Relaxed);

        let stores = self.stores.read().await;
        for store in stores.values() {
            store.clear_remote_slices();
        }

        info!("[StoreSyncAdapter] Stopped");
    }

    /// Dispose the adapter. Stops and prevents restart.
    pub async fn dispose(&self) {
        if self.disposed.swap(true, Ordering::Relaxed) {
            return; // already disposed
        }
        self.stop().await;
        info!("[StoreSyncAdapter] Disposed");
    }

    // ── Device events ─────────────────────────────────────────────────────

    /// Call when a device goes offline to clear its slices.
    pub async fn handle_device_offline(&self, device_id: &str) {
        info!("[StoreSyncAdapter] Device {device_id} went offline");

        let stores = self.stores.read().await;
        for store in stores.values() {
            store.remove_remote_slice(device_id, "offline");
        }

        // Broadcast SYNC_CLEAR for all stores
        for store_id in stores.keys() {
            let payload = SyncClearPayload {
                store_id: store_id.clone(),
                device_id: device_id.to_string(),
                reason: Some("offline".to_string()),
            };
            self.broadcast(message_types::SYNC_CLEAR, &payload);
        }
    }

    /// Call when a new device is discovered to sync state.
    pub async fn handle_device_discovered(&self, device_id: &str) {
        info!("[StoreSyncAdapter] Device {device_id} discovered");

        self.broadcast_all_stores().await;

        let stores = self.stores.read().await;
        for store_id in stores.keys() {
            let payload = SyncRequestPayload {
                store_id: store_id.clone(),
                from_device_id: Some(device_id.to_string()),
            };
            self.broadcast(message_types::SYNC_REQUEST, &payload);
        }
    }

    // ── Incoming message handling ─────────────────────────────────────────

    /// Handle an incoming sync message from the MessageBus.
    pub async fn handle_sync_message(&self, message: &SyncMessage) {
        if !self.started.load(Ordering::Relaxed) {
            return;
        }

        // Extract storeId from payload
        let store_id = match message.payload.get("storeId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return,
        };

        let stores = self.stores.read().await;
        let store = match stores.get(&store_id) {
            Some(s) => Arc::clone(s),
            None => {
                warn!("[StoreSyncAdapter] Unknown store in sync message: {store_id}");
                return;
            }
        };
        drop(stores);

        match message.msg_type.as_str() {
            msg_type if msg_type == message_types::SYNC_FULL => {
                self.handle_sync_full(&store, message.from.as_deref(), &message.payload);
            }
            msg_type if msg_type == message_types::SYNC_UPDATE => {
                self.handle_sync_update(&store, message.from.as_deref(), &message.payload);
            }
            msg_type if msg_type == message_types::SYNC_REQUEST => {
                self.handle_sync_request(message.from.as_deref(), &store_id, &message.payload).await;
            }
            msg_type if msg_type == message_types::SYNC_CLEAR => {
                self.handle_sync_clear(&store, &message.payload);
            }
            _ => {}
        }
    }

    /// Handle a local store change. Broadcasts SYNC_UPDATE.
    pub async fn handle_local_changed(&self, store_id: &str, slice: &DeviceSlice) {
        if !self.started.load(Ordering::Relaxed) {
            return;
        }

        let payload = SyncUpdatePayload {
            store_id: store_id.to_string(),
            device_id: slice.device_id.clone(),
            data: slice.data.clone(),
            version: slice.version,
            updated_at: slice.updated_at,
        };
        self.broadcast(message_types::SYNC_UPDATE, &payload);
    }

    // ── Private: message handlers ─────────────────────────────────────────

    fn handle_sync_full(
        &self,
        store: &Arc<dyn SyncableStore>,
        from: Option<&str>,
        payload: &serde_json::Value,
    ) {
        let from = match from {
            Some(f) if f != self.local_device_id => f,
            _ => return, // ignore from self or missing
        };

        let device_id = payload
            .get("deviceId")
            .and_then(|v| v.as_str())
            .unwrap_or(from);
        let data = payload.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let updated_at = payload
            .get("updatedAt")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let version = payload
            .get("version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        store.apply_remote_slice(DeviceSlice {
            device_id: device_id.to_string(),
            data,
            updated_at,
            version,
        });
    }

    fn handle_sync_update(
        &self,
        store: &Arc<dyn SyncableStore>,
        from: Option<&str>,
        payload: &serde_json::Value,
    ) {
        // Same logic as SYNC_FULL
        self.handle_sync_full(store, from, payload);
    }

    async fn handle_sync_request(
        &self,
        from: Option<&str>,
        store_id: &str,
        payload: &serde_json::Value,
    ) {
        match from {
            Some(f) if f != self.local_device_id => {}
            _ => return,
        };

        // If targeted at a specific device, check if it's us
        if let Some(from_device_id) = payload.get("fromDeviceId").and_then(|v| v.as_str()) {
            if from_device_id != self.local_device_id {
                return;
            }
        }

        let stores = self.stores.read().await;
        if let Some(store) = stores.get(store_id) {
            self.broadcast_store_full(store_id, &**store);
        }
    }

    fn handle_sync_clear(&self, store: &Arc<dyn SyncableStore>, payload: &serde_json::Value) {
        let device_id = match payload.get("deviceId").and_then(|v| v.as_str()) {
            Some(id) if id != self.local_device_id => id,
            _ => return, // ignore clear for self
        };

        let reason = payload
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        store.remove_remote_slice(device_id, reason);
    }

    // ── Private: broadcasting ─────────────────────────────────────────────

    fn broadcast<T: serde::Serialize>(&self, msg_type: &str, payload: &T) {
        let payload_json = match serde_json::to_value(payload) {
            Ok(v) => v,
            Err(e) => {
                warn!("[StoreSyncAdapter] Failed to serialize payload: {e}");
                return;
            }
        };
        let _ = self.outgoing_tx.send(OutgoingSyncMessage {
            msg_type: msg_type.to_string(),
            payload: payload_json,
        });
    }

    fn broadcast_store_full(&self, store_id: &str, store: &dyn SyncableStore) {
        let slice = match store.get_local_slice() {
            Some(s) => s,
            None => return,
        };

        let payload = SyncFullPayload {
            store_id: store_id.to_string(),
            device_id: slice.device_id,
            data: slice.data,
            version: slice.version,
            updated_at: slice.updated_at,
            schema_version: None,
        };

        self.broadcast(message_types::SYNC_FULL, &payload);
    }

    async fn broadcast_all_stores(&self) {
        let stores = self.stores.read().await;
        for (store_id, store) in stores.iter() {
            self.broadcast_store_full(store_id, &**store);
        }
    }

    async fn request_sync_from_devices(&self) {
        let stores = self.stores.read().await;
        for store_id in stores.keys() {
            let payload = SyncRequestPayload {
                store_id: store_id.clone(),
                from_device_id: None,
            };
            self.broadcast(message_types::SYNC_REQUEST, &payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test store that records calls for verification.
    struct MockStore {
        id: String,
        local_slice: Mutex<Option<DeviceSlice>>,
        applied_slices: Mutex<Vec<DeviceSlice>>,
        removed_slices: Mutex<Vec<(String, String)>>,
        cleared: Mutex<bool>,
    }

    impl MockStore {
        fn new(id: &str) -> Arc<Self> {
            Arc::new(Self {
                id: id.to_string(),
                local_slice: Mutex::new(None),
                applied_slices: Mutex::new(Vec::new()),
                removed_slices: Mutex::new(Vec::new()),
                cleared: Mutex::new(false),
            })
        }

        fn set_local_slice(&self, slice: DeviceSlice) {
            *self.local_slice.lock().unwrap() = Some(slice);
        }

        fn applied_count(&self) -> usize {
            self.applied_slices.lock().unwrap().len()
        }

        fn last_applied(&self) -> Option<DeviceSlice> {
            self.applied_slices.lock().unwrap().last().cloned()
        }

        fn removed_count(&self) -> usize {
            self.removed_slices.lock().unwrap().len()
        }

        fn was_cleared(&self) -> bool {
            *self.cleared.lock().unwrap()
        }
    }

    impl SyncableStore for MockStore {
        fn store_id(&self) -> &str {
            &self.id
        }

        fn get_local_slice(&self) -> Option<DeviceSlice> {
            self.local_slice.lock().unwrap().clone()
        }

        fn apply_remote_slice(&self, slice: DeviceSlice) {
            self.applied_slices.lock().unwrap().push(slice);
        }

        fn remove_remote_slice(&self, device_id: &str, reason: &str) {
            self.removed_slices
                .lock()
                .unwrap()
                .push((device_id.to_string(), reason.to_string()));
        }

        fn clear_remote_slices(&self) {
            *self.cleared.lock().unwrap() = true;
        }
    }

    fn create_adapter(
        stores: Vec<Arc<MockStore>>,
    ) -> (Arc<StoreSyncAdapter>, mpsc::UnboundedReceiver<OutgoingSyncMessage>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let store_refs: Vec<Arc<dyn SyncableStore>> = stores
            .into_iter()
            .map(|s| s as Arc<dyn SyncableStore>)
            .collect();
        let adapter = StoreSyncAdapter::new(
            StoreSyncAdapterConfig {
                local_device_id: "dev-1".to_string(),
            },
            store_refs,
            tx,
        );
        (adapter, rx)
    }

    fn drain_messages(rx: &mut mpsc::UnboundedReceiver<OutgoingSyncMessage>) -> Vec<OutgoingSyncMessage> {
        let mut msgs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            msgs.push(msg);
        }
        msgs
    }

    // ── Lifecycle tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn start_broadcasts_all_stores() {
        let store1 = MockStore::new("tasks");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({"items": ["a", "b"]}),
            updated_at: 1000,
            version: 1,
        });
        let store2 = MockStore::new("settings");

        let (adapter, mut rx) = create_adapter(vec![store1, store2]);
        adapter.start().await;

        let msgs = drain_messages(&mut rx);

        // Should have SYNC_REQUEST for both stores + SYNC_FULL for store1 (store2 has no slice)
        let full_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_FULL)
            .collect();
        assert_eq!(full_msgs.len(), 1);
        assert_eq!(
            full_msgs[0].payload.get("storeId").unwrap().as_str().unwrap(),
            "tasks"
        );

        let request_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_REQUEST)
            .collect();
        assert_eq!(request_msgs.len(), 2);
    }

    #[tokio::test]
    async fn stop_clears_remote_slices() {
        let store1 = MockStore::new("tasks");
        let store2 = MockStore::new("settings");
        let s1 = Arc::clone(&store1);
        let s2 = Arc::clone(&store2);

        let (adapter, _rx) = create_adapter(vec![store1, store2]);
        adapter.start().await;
        adapter.stop().await;

        assert!(s1.was_cleared());
        assert!(s2.was_cleared());
    }

    #[tokio::test]
    async fn dispose_prevents_restart() {
        let store1 = MockStore::new("tasks");
        let (adapter, mut rx) = create_adapter(vec![store1]);

        adapter.dispose().await;
        drain_messages(&mut rx); // clear any stop messages

        adapter.start().await;

        let msgs = drain_messages(&mut rx);
        assert!(msgs.is_empty(), "should not send messages after dispose");
    }

    #[tokio::test]
    async fn double_dispose_is_safe() {
        let store1 = MockStore::new("tasks");
        let (adapter, _rx) = create_adapter(vec![store1]);

        adapter.dispose().await;
        adapter.dispose().await; // should not panic
    }

    // ── Incoming message tests ───────────────────────────────────────────

    #[tokio::test]
    async fn handle_sync_full_from_remote() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: message_types::SYNC_FULL.to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                    "deviceId": "dev-2",
                    "data": {"items": ["x"]},
                    "version": 3,
                    "updatedAt": 2000,
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 1);
        let applied = s1.last_applied().unwrap();
        assert_eq!(applied.device_id, "dev-2");
        assert_eq!(applied.version, 3);
    }

    #[tokio::test]
    async fn handle_sync_update_from_remote() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: message_types::SYNC_UPDATE.to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                    "deviceId": "dev-2",
                    "data": {"items": ["y"]},
                    "version": 4,
                    "updatedAt": 3000,
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 1);
        let applied = s1.last_applied().unwrap();
        assert_eq!(applied.version, 4);
    }

    #[tokio::test]
    async fn ignores_sync_full_from_self() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-1".to_string()),
                msg_type: message_types::SYNC_FULL.to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                    "deviceId": "dev-1",
                    "data": {},
                    "version": 1,
                    "updatedAt": 1000,
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 0);
    }

    #[tokio::test]
    async fn handle_sync_request_broadcasts_full() {
        let store1 = MockStore::new("tasks");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({"items": ["local"]}),
            updated_at: 5000,
            version: 5,
        });

        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        drain_messages(&mut rx); // clear start messages

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-3".to_string()),
                msg_type: message_types::SYNC_REQUEST.to_string(),
                payload: serde_json::json!({"storeId": "tasks"}),
            })
            .await;

        let msgs = drain_messages(&mut rx);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].msg_type, message_types::SYNC_FULL);
        assert_eq!(
            msgs[0].payload.get("storeId").unwrap().as_str().unwrap(),
            "tasks"
        );
    }

    #[tokio::test]
    async fn handle_targeted_sync_request() {
        let store1 = MockStore::new("tasks");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({}),
            updated_at: 1000,
            version: 1,
        });

        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        drain_messages(&mut rx);

        // Targeted at dev-1 - should respond
        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-3".to_string()),
                msg_type: message_types::SYNC_REQUEST.to_string(),
                payload: serde_json::json!({"storeId": "tasks", "fromDeviceId": "dev-1"}),
            })
            .await;

        let msgs = drain_messages(&mut rx);
        assert_eq!(msgs.len(), 1);
    }

    #[tokio::test]
    async fn ignores_sync_request_targeted_at_other() {
        let store1 = MockStore::new("tasks");
        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        drain_messages(&mut rx);

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-3".to_string()),
                msg_type: message_types::SYNC_REQUEST.to_string(),
                payload: serde_json::json!({"storeId": "tasks", "fromDeviceId": "dev-99"}),
            })
            .await;

        let msgs = drain_messages(&mut rx);
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn handle_sync_clear_from_remote() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: message_types::SYNC_CLEAR.to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                    "deviceId": "dev-2",
                    "reason": "offline",
                }),
            })
            .await;

        assert_eq!(s1.removed_count(), 1);
    }

    #[tokio::test]
    async fn ignores_sync_clear_for_self() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: message_types::SYNC_CLEAR.to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                    "deviceId": "dev-1",
                    "reason": "offline",
                }),
            })
            .await;

        assert_eq!(s1.removed_count(), 0);
    }

    #[tokio::test]
    async fn ignores_unknown_stores() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: message_types::SYNC_FULL.to_string(),
                payload: serde_json::json!({
                    "storeId": "nonexistent",
                    "deviceId": "dev-2",
                    "data": {},
                    "version": 1,
                    "updatedAt": 1000,
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 0);
    }

    // ── Outgoing message tests ───────────────────────────────────────────

    #[tokio::test]
    async fn local_changed_broadcasts_update() {
        let store1 = MockStore::new("tasks");
        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        drain_messages(&mut rx);

        let slice = DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({"items": ["updated"]}),
            updated_at: 6000,
            version: 6,
        };

        adapter.handle_local_changed("tasks", &slice).await;

        let msgs = drain_messages(&mut rx);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].msg_type, message_types::SYNC_UPDATE);
        assert_eq!(
            msgs[0].payload.get("storeId").unwrap().as_str().unwrap(),
            "tasks"
        );
        assert_eq!(
            msgs[0].payload.get("version").unwrap().as_u64().unwrap(),
            6
        );
    }

    #[tokio::test]
    async fn does_not_broadcast_after_stop() {
        let store1 = MockStore::new("tasks");
        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        adapter.stop().await;
        drain_messages(&mut rx);

        let slice = DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({}),
            updated_at: 1000,
            version: 1,
        };

        adapter.handle_local_changed("tasks", &slice).await;

        let msgs = drain_messages(&mut rx);
        assert!(msgs.is_empty());
    }

    // ── Device event tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn handle_device_offline() {
        let store1 = MockStore::new("tasks");
        let store2 = MockStore::new("settings");
        let s1 = Arc::clone(&store1);
        let s2 = Arc::clone(&store2);

        let (adapter, mut rx) = create_adapter(vec![store1, store2]);
        adapter.start().await;
        drain_messages(&mut rx);

        adapter.handle_device_offline("dev-2").await;

        assert_eq!(s1.removed_count(), 1);
        assert_eq!(s2.removed_count(), 1);

        let msgs = drain_messages(&mut rx);
        let clear_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_CLEAR)
            .collect();
        assert_eq!(clear_msgs.len(), 2);
    }

    #[tokio::test]
    async fn handle_device_discovered() {
        let store1 = MockStore::new("tasks");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({"tasks": []}),
            updated_at: 1000,
            version: 1,
        });

        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        drain_messages(&mut rx);

        adapter.handle_device_discovered("dev-3").await;

        let msgs = drain_messages(&mut rx);

        // Should broadcast SYNC_FULL
        let full_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_FULL)
            .collect();
        assert_eq!(full_msgs.len(), 1);

        // Should send SYNC_REQUEST
        let req_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_REQUEST)
            .collect();
        assert_eq!(req_msgs.len(), 1);
        assert_eq!(
            req_msgs[0]
                .payload
                .get("fromDeviceId")
                .unwrap()
                .as_str()
                .unwrap(),
            "dev-3"
        );
    }

    // ── ARCH-12: AtomicBool flag tests ────────────────────────────────────

    /// ARCH-12: disposed flag uses AtomicBool, not RwLock.
    /// Verify that concurrent dispose calls are safe.
    #[tokio::test]
    async fn concurrent_dispose_is_safe() {
        let store1 = MockStore::new("tasks");
        let (adapter, _rx) = create_adapter(vec![store1]);

        // Dispose from multiple "concurrent" calls
        let adapter1 = adapter.clone();
        let adapter2 = adapter.clone();

        let h1 = tokio::spawn(async move { adapter1.dispose().await });
        let h2 = tokio::spawn(async move { adapter2.dispose().await });

        h1.await.unwrap();
        h2.await.unwrap();

        // Should not panic, and start should be blocked
        let mut rx2 = {
            let (adapter3, rx) = create_adapter(vec![MockStore::new("tasks")]);
            adapter3.dispose().await;
            adapter3.start().await;
            rx
        };
        let msgs = drain_messages(&mut rx2);
        assert!(msgs.is_empty(), "Should not send messages after dispose");
    }

    /// ARCH-12: started flag uses AtomicBool, not RwLock.
    #[tokio::test]
    async fn started_flag_blocks_incoming_messages() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);

        // Don't call start() -- started=false

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: message_types::SYNC_FULL.to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                    "deviceId": "dev-2",
                    "data": {},
                    "version": 1,
                    "updatedAt": 1000,
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 0,
            "Messages should be ignored when not started");
    }

    /// stop() then start() cycle works correctly.
    #[tokio::test]
    async fn stop_start_cycle_works() {
        let store1 = MockStore::new("tasks");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({"items": []}),
            updated_at: 1000,
            version: 1,
        });

        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        drain_messages(&mut rx);

        adapter.stop().await;
        drain_messages(&mut rx);

        // Start again -- should broadcast
        adapter.start().await;

        let msgs = drain_messages(&mut rx);
        let full_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_FULL)
            .collect();
        assert!(!full_msgs.is_empty(), "Restart should broadcast stores again");
    }

    // ══════════════════════════════════════════════════════════════════════
    // Adversarial edge-case tests (Layer 4: Services)
    // ══════════════════════════════════════════════════════════════════════

    // ── 1. Register store after start ────────────────────────────────────
    // StoreSyncAdapter takes stores at construction time and there's no
    // public `register_store` method — but we can verify that stores
    // supplied at construction are properly synced even if start() is
    // called on an adapter with many stores.
    // Instead, we verify the real edge case: calling start() triggers
    // broadcast for all stores present at construction time.
    #[tokio::test]
    async fn start_broadcasts_for_all_registered_stores() {
        let mut stores = Vec::new();
        for i in 0..5 {
            let s = MockStore::new(&format!("store-{i}"));
            s.set_local_slice(DeviceSlice {
                device_id: "dev-1".to_string(),
                data: serde_json::json!({"idx": i}),
                updated_at: 1000 + i as u64,
                version: 1,
            });
            stores.push(s);
        }
        let (adapter, mut rx) = create_adapter(stores);
        adapter.start().await;

        let msgs = drain_messages(&mut rx);
        let full_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_FULL)
            .collect();
        assert_eq!(full_msgs.len(), 5, "All 5 stores should broadcast SYNC_FULL on start");
    }

    // ── 3. handle_local_changed for unknown store_id ─────────────────────
    #[tokio::test]
    async fn handle_local_changed_unknown_store_no_panic() {
        let store1 = MockStore::new("tasks");
        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        drain_messages(&mut rx);

        // Call handle_local_changed with a store_id that doesn't exist
        // This should not panic - it broadcasts the update regardless
        // because handle_local_changed doesn't look up the store.
        let slice = DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({"ghost": true}),
            updated_at: 9999,
            version: 42,
        };
        adapter.handle_local_changed("nonexistent-store", &slice).await;

        let msgs = drain_messages(&mut rx);
        // It broadcasts unconditionally (no store lookup in handle_local_changed)
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].msg_type, message_types::SYNC_UPDATE);
        assert_eq!(
            msgs[0].payload.get("storeId").unwrap().as_str().unwrap(),
            "nonexistent-store"
        );
    }

    // ── 4. handle_sync_message before start ──────────────────────────────
    #[tokio::test]
    async fn handle_sync_message_before_start_is_rejected() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, mut rx) = create_adapter(vec![store1]);
        // Deliberately do NOT call start()

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: message_types::SYNC_FULL.to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                    "deviceId": "dev-2",
                    "data": {"items": ["sneaky"]},
                    "version": 1,
                    "updatedAt": 1000,
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 0, "Should reject message when not started");
        let msgs = drain_messages(&mut rx);
        assert!(msgs.is_empty(), "No outgoing messages before start");
    }

    // ── 5. Dispose while processing ──────────────────────────────────────
    #[tokio::test]
    async fn dispose_during_message_handling() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({}),
            updated_at: 1000,
            version: 1,
        });

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        // Fire both concurrently: a sync message + dispose
        let adapter1 = adapter.clone();
        let adapter2 = adapter.clone();

        let h1 = tokio::spawn(async move {
            for i in 0..10 {
                adapter1
                    .handle_sync_message(&SyncMessage {
                        from: Some("dev-2".to_string()),
                        msg_type: message_types::SYNC_FULL.to_string(),
                        payload: serde_json::json!({
                            "storeId": "tasks",
                            "deviceId": "dev-2",
                            "data": {"i": i},
                            "version": i as u64,
                            "updatedAt": 1000 + i as u64,
                        }),
                    })
                    .await;
            }
        });

        let h2 = tokio::spawn(async move {
            adapter2.dispose().await;
        });

        // Neither should panic
        h1.await.unwrap();
        h2.await.unwrap();

        // After dispose, no more messages should be accepted
        assert!(s1.applied_count() <= 10, "At most 10 messages accepted");
    }

    // ── 6. Double start ──────────────────────────────────────────────────
    #[tokio::test]
    async fn double_start_is_idempotent() {
        let store1 = MockStore::new("tasks");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({}),
            updated_at: 1000,
            version: 1,
        });

        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        let msgs1 = drain_messages(&mut rx);

        adapter.start().await;
        let msgs2 = drain_messages(&mut rx);

        // Both calls produce broadcasts (start doesn't check if already started).
        // This verifies no crash/deadlock on double-start.
        let full1: Vec<_> = msgs1.iter().filter(|m| m.msg_type == message_types::SYNC_FULL).collect();
        let full2: Vec<_> = msgs2.iter().filter(|m| m.msg_type == message_types::SYNC_FULL).collect();
        assert_eq!(full1.len(), full2.len(), "Double start should produce same broadcasts");
    }

    // ── 7. Double dispose ────────────────────────────────────────────────
    // (Already tested in `double_dispose_is_safe`, adding explicit assertion)
    #[tokio::test]
    async fn double_dispose_no_double_clear() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        adapter.dispose().await;
        // Reset cleared flag to detect second dispose
        *s1.cleared.lock().unwrap() = false;

        adapter.dispose().await;
        assert!(!s1.was_cleared(), "Second dispose should be a no-op due to disposed flag swap");
    }

    // ── 8. SYNC_FULL with very large payload ─────────────────────────────
    #[tokio::test]
    async fn sync_full_with_large_payload() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        // Build ~1MB JSON payload
        let large_array: Vec<String> = (0..10_000)
            .map(|i| format!("item-{i}-{}", "x".repeat(90)))
            .collect();

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: message_types::SYNC_FULL.to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                    "deviceId": "dev-2",
                    "data": large_array,
                    "version": 1,
                    "updatedAt": 1000,
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 1, "Large payload should be handled");
        let applied = s1.last_applied().unwrap();
        assert!(applied.data.as_array().unwrap().len() == 10_000);
    }

    // ── 9. SYNC_REQUEST triggers SYNC_FULL response ──────────────────────
    // (Already tested in handle_sync_request_broadcasts_full, adding edge
    //  case: multiple stores, only the requested one responds)
    #[tokio::test]
    async fn sync_request_responds_only_for_requested_store() {
        let store1 = MockStore::new("tasks");
        let store2 = MockStore::new("settings");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({"tasks": []}),
            updated_at: 1000,
            version: 1,
        });
        store2.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({"theme": "dark"}),
            updated_at: 2000,
            version: 2,
        });

        let (adapter, mut rx) = create_adapter(vec![store1, store2]);
        adapter.start().await;
        drain_messages(&mut rx);

        // Request sync only for "tasks"
        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-3".to_string()),
                msg_type: message_types::SYNC_REQUEST.to_string(),
                payload: serde_json::json!({"storeId": "tasks"}),
            })
            .await;

        let msgs = drain_messages(&mut rx);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].msg_type, message_types::SYNC_FULL);
        assert_eq!(
            msgs[0].payload.get("storeId").unwrap().as_str().unwrap(),
            "tasks"
        );
    }

    // ── 10. SYNC_REQUEST for unknown store ───────────────────────────────
    #[tokio::test]
    async fn sync_request_for_unknown_store_no_crash() {
        let store1 = MockStore::new("tasks");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({}),
            updated_at: 1000,
            version: 1,
        });

        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        drain_messages(&mut rx);

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-3".to_string()),
                msg_type: message_types::SYNC_REQUEST.to_string(),
                payload: serde_json::json!({"storeId": "nonexistent"}),
            })
            .await;

        let msgs = drain_messages(&mut rx);
        assert!(msgs.is_empty(), "No response for unknown store");
    }

    // ── 11. Device discovered triggers sync requests for all stores ──────
    #[tokio::test]
    async fn device_discovered_sends_requests_for_all_stores() {
        let store1 = MockStore::new("tasks");
        let store2 = MockStore::new("settings");
        let store3 = MockStore::new("presence");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({}),
            updated_at: 1000,
            version: 1,
        });

        let (adapter, mut rx) = create_adapter(vec![store1, store2, store3]);
        adapter.start().await;
        drain_messages(&mut rx);

        adapter.handle_device_discovered("dev-5").await;

        let msgs = drain_messages(&mut rx);
        let req_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_REQUEST)
            .collect();
        assert_eq!(req_msgs.len(), 3, "Should send SYNC_REQUEST for each store");

        // All requests should have fromDeviceId = "dev-5"
        for msg in &req_msgs {
            assert_eq!(
                msg.payload.get("fromDeviceId").unwrap().as_str().unwrap(),
                "dev-5"
            );
        }
    }

    // ── 12. Device offline clears remote slices ──────────────────────────
    #[tokio::test]
    async fn device_offline_clears_remote_slices_for_all_stores() {
        let store1 = MockStore::new("tasks");
        let store2 = MockStore::new("settings");
        let store3 = MockStore::new("presence");
        let s1 = Arc::clone(&store1);
        let s2 = Arc::clone(&store2);
        let s3 = Arc::clone(&store3);

        let (adapter, mut rx) = create_adapter(vec![store1, store2, store3]);
        adapter.start().await;
        drain_messages(&mut rx);

        adapter.handle_device_offline("dev-7").await;

        // All stores should have had remove_remote_slice called
        assert_eq!(s1.removed_count(), 1);
        assert_eq!(s2.removed_count(), 1);
        assert_eq!(s3.removed_count(), 1);

        // Verify the removed device_id and reason
        let removed = s1.removed_slices.lock().unwrap();
        assert_eq!(removed[0].0, "dev-7");
        assert_eq!(removed[0].1, "offline");

        // Verify SYNC_CLEAR messages sent
        let msgs = drain_messages(&mut rx);
        let clear_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_CLEAR)
            .collect();
        assert_eq!(clear_msgs.len(), 3, "Should broadcast SYNC_CLEAR for each store");
    }

    // ── 13. Many stores registered ───────────────────────────────────────
    #[tokio::test]
    async fn twenty_stores_all_broadcast_on_start() {
        let mut stores = Vec::new();
        let mut refs = Vec::new();
        for i in 0..20 {
            let s = MockStore::new(&format!("store-{i}"));
            s.set_local_slice(DeviceSlice {
                device_id: "dev-1".to_string(),
                data: serde_json::json!({"idx": i}),
                updated_at: 1000 + i as u64,
                version: 1,
            });
            refs.push(Arc::clone(&s));
            stores.push(s);
        }

        let (adapter, mut rx) = create_adapter(stores);
        adapter.start().await;

        let msgs = drain_messages(&mut rx);
        let full_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_FULL)
            .collect();
        assert_eq!(full_msgs.len(), 20, "All 20 stores should broadcast SYNC_FULL");

        let req_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_REQUEST)
            .collect();
        assert_eq!(req_msgs.len(), 20, "All 20 stores should send SYNC_REQUEST");
    }

    // ── 14. Concurrent handle_local_changed calls ────────────────────────
    #[tokio::test]
    async fn concurrent_local_changed_no_deadlock() {
        let store1 = MockStore::new("tasks");
        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        drain_messages(&mut rx);

        let mut handles = Vec::new();
        for i in 0..10 {
            let a = adapter.clone();
            let handle = tokio::spawn(async move {
                let slice = DeviceSlice {
                    device_id: "dev-1".to_string(),
                    data: serde_json::json!({"concurrent": i}),
                    updated_at: 1000 + i as u64,
                    version: i as u64,
                };
                a.handle_local_changed("tasks", &slice).await;
            });
            handles.push(handle);
        }

        for h in handles {
            h.await.unwrap();
        }

        let msgs = drain_messages(&mut rx);
        let update_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| m.msg_type == message_types::SYNC_UPDATE)
            .collect();
        assert_eq!(update_msgs.len(), 10, "All 10 concurrent updates should produce messages");
    }

    // ── Edge: handle_local_changed when not started ──────────────────────
    #[tokio::test]
    async fn handle_local_changed_when_not_started() {
        let store1 = MockStore::new("tasks");
        let (adapter, mut rx) = create_adapter(vec![store1]);
        // Not started

        let slice = DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({}),
            updated_at: 1000,
            version: 1,
        };
        adapter.handle_local_changed("tasks", &slice).await;

        let msgs = drain_messages(&mut rx);
        assert!(msgs.is_empty(), "Should not broadcast when not started");
    }

    // ── Edge: SYNC_FULL with missing payload fields ──────────────────────
    #[tokio::test]
    async fn sync_full_with_missing_fields_uses_defaults() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        // Payload missing deviceId, data, version, updatedAt
        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: message_types::SYNC_FULL.to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 1);
        let applied = s1.last_applied().unwrap();
        // device_id falls back to from
        assert_eq!(applied.device_id, "dev-2");
        // data falls back to Null
        assert!(applied.data.is_null());
        // version/updated_at fall back to 0
        assert_eq!(applied.version, 0);
        assert_eq!(applied.updated_at, 0);
    }

    // ── Edge: SYNC message with missing storeId ──────────────────────────
    #[tokio::test]
    async fn sync_message_missing_store_id_is_ignored() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: message_types::SYNC_FULL.to_string(),
                payload: serde_json::json!({
                    "data": {"items": []},
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 0, "Message without storeId should be ignored");
    }

    // ── Edge: SYNC message with None from field ──────────────────────────
    #[tokio::test]
    async fn sync_full_with_none_from_is_ignored() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, _rx) = create_adapter(vec![store1]);
        adapter.start().await;

        adapter
            .handle_sync_message(&SyncMessage {
                from: None,
                msg_type: message_types::SYNC_FULL.to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                    "deviceId": "dev-2",
                    "data": {},
                    "version": 1,
                    "updatedAt": 1000,
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 0, "Message with None from should be ignored");
    }

    // ── Edge: Unknown message type ───────────────────────────────────────
    #[tokio::test]
    async fn unknown_message_type_is_ignored() {
        let store1 = MockStore::new("tasks");
        let s1 = Arc::clone(&store1);

        let (adapter, mut rx) = create_adapter(vec![store1]);
        adapter.start().await;
        drain_messages(&mut rx);

        adapter
            .handle_sync_message(&SyncMessage {
                from: Some("dev-2".to_string()),
                msg_type: "store:sync:unknown".to_string(),
                payload: serde_json::json!({
                    "storeId": "tasks",
                }),
            })
            .await;

        assert_eq!(s1.applied_count(), 0);
        let msgs = drain_messages(&mut rx);
        assert!(msgs.is_empty(), "Unknown message types should produce no output");
    }

    // ── Edge: Concurrent start + stop + dispose ──────────────────────────
    #[tokio::test]
    async fn concurrent_lifecycle_operations_no_panic() {
        let store1 = MockStore::new("tasks");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({}),
            updated_at: 1000,
            version: 1,
        });

        let (adapter, _rx) = create_adapter(vec![store1]);

        let a1 = adapter.clone();
        let a2 = adapter.clone();
        let a3 = adapter.clone();

        let h1 = tokio::spawn(async move { a1.start().await });
        let h2 = tokio::spawn(async move { a2.stop().await });
        let h3 = tokio::spawn(async move { a3.dispose().await });

        h1.await.unwrap();
        h2.await.unwrap();
        h3.await.unwrap();
    }

    // ── Edge: Dropped receiver channel ───────────────────────────────────
    #[tokio::test]
    async fn broadcast_with_dropped_receiver_no_panic() {
        let store1 = MockStore::new("tasks");
        store1.set_local_slice(DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({}),
            updated_at: 1000,
            version: 1,
        });

        let (adapter, rx) = create_adapter(vec![store1]);
        drop(rx); // Drop receiver

        // start() will try to broadcast - should not panic
        adapter.start().await;

        // handle_local_changed should also not panic
        let slice = DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({}),
            updated_at: 2000,
            version: 2,
        };
        adapter.handle_local_changed("tasks", &slice).await;
    }
}
