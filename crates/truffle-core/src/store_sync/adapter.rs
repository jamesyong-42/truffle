use std::collections::HashMap;
use std::sync::Arc;

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

    disposed: RwLock<bool>,
    started: RwLock<bool>,
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
            disposed: RwLock::new(false),
            started: RwLock::new(false),
        })
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Start syncing. Requests sync from existing devices and broadcasts current state.
    pub async fn start(&self) {
        if *self.disposed.read().await {
            warn!("[StoreSyncAdapter] start: SKIPPED - disposed");
            return;
        }

        *self.started.write().await = true;

        // Request sync from existing devices
        self.request_sync_from_devices().await;

        // Broadcast our current state
        self.broadcast_all_stores().await;

        let store_count = self.stores.read().await.len();
        info!("[StoreSyncAdapter] Started with {store_count} stores");
    }

    /// Stop syncing. Clears remote slices from all stores.
    pub async fn stop(&self) {
        *self.started.write().await = false;

        let stores = self.stores.read().await;
        for store in stores.values() {
            store.clear_remote_slices();
        }

        info!("[StoreSyncAdapter] Stopped");
    }

    /// Dispose the adapter. Stops and prevents restart.
    pub async fn dispose(&self) {
        {
            let is_disposed = *self.disposed.read().await;
            if is_disposed {
                return;
            }
        }
        *self.disposed.write().await = true;
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
        if !*self.started.read().await {
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
        if !*self.started.read().await {
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
}
