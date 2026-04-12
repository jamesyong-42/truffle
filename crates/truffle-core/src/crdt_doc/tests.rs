use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;

use crate::envelope::codec::JsonCodec;
use crate::envelope::EnvelopeCodec;
use crate::network::*;
use crate::session::PeerRegistry;
use crate::transport::websocket::WebSocketTransport;
use crate::transport::WsConfig;

use super::backend::{CrdtBackend, CrdtFileBackend, MemoryCrdtBackend};
use super::types::CrdtDocEvent;
use super::CrdtDoc;

// ── Mock network provider ───────────────────────────────────────────

struct MockNetworkProvider {
    identity: NodeIdentity,
    local_addr: PeerAddr,
    peer_event_tx: broadcast::Sender<NetworkPeerEvent>,
    mock_peers: Arc<tokio::sync::RwLock<Vec<NetworkPeer>>>,
}

impl MockNetworkProvider {
    fn new(id: &str) -> Self {
        let (peer_event_tx, _) = broadcast::channel(64);
        Self {
            identity: NodeIdentity {
                app_id: "test".to_string(),
                device_id: id.to_string(),
                device_name: format!("Test Node {id}"),
                tailscale_hostname: format!("truffle-test-{id}"),
                tailscale_id: id.to_string(),
                dns_name: None,
                ip: Some("127.0.0.1".parse().unwrap()),
            },
            local_addr: PeerAddr {
                ip: Some("127.0.0.1".parse().unwrap()),
                hostname: format!("truffle-test-{id}"),
                dns_name: None,
            },
            peer_event_tx,
            mock_peers: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        }
    }

    fn event_sender(&self) -> broadcast::Sender<NetworkPeerEvent> {
        self.peer_event_tx.clone()
    }
}

impl NetworkProvider for MockNetworkProvider {
    fn local_identity(&self) -> NodeIdentity {
        self.identity.clone()
    }
    fn local_addr(&self) -> PeerAddr {
        self.local_addr.clone()
    }
    fn peer_events(&self) -> broadcast::Receiver<NetworkPeerEvent> {
        self.peer_event_tx.subscribe()
    }
    async fn start(&mut self) -> Result<(), NetworkError> {
        Ok(())
    }
    async fn stop(&mut self) -> Result<(), NetworkError> {
        Ok(())
    }
    async fn peers(&self) -> Vec<NetworkPeer> {
        self.mock_peers.read().await.clone()
    }
    async fn dial_tcp(
        &self,
        _addr: &str,
        _port: u16,
    ) -> Result<tokio::net::TcpStream, NetworkError> {
        Err(NetworkError::DialFailed("mock".into()))
    }
    async fn listen_tcp(&self, _port: u16) -> Result<NetworkTcpListener, NetworkError> {
        Err(NetworkError::ListenFailed("mock".into()))
    }
    async fn unlisten_tcp(&self, _port: u16) -> Result<(), NetworkError> {
        Ok(())
    }
    async fn bind_udp(&self, _port: u16) -> Result<NetworkUdpSocket, NetworkError> {
        Err(NetworkError::NotRunning)
    }
    async fn ping(&self, _addr: &str) -> Result<PingResult, NetworkError> {
        Ok(PingResult {
            latency: Duration::from_millis(1),
            connection: "direct".to_string(),
            peer_addr: None,
        })
    }
    async fn health(&self) -> HealthInfo {
        HealthInfo {
            state: "running".to_string(),
            healthy: true,
            ..Default::default()
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn ws_config(port: u16) -> WsConfig {
    WsConfig {
        port,
        ping_interval: Duration::from_secs(300),
        pong_timeout: Duration::from_secs(300),
        ..Default::default()
    }
}

async fn random_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}

async fn make_test_node(
    id: &str,
    ws_port: u16,
) -> (
    Arc<crate::node::Node<MockNetworkProvider>>,
    broadcast::Sender<NetworkPeerEvent>,
) {
    let provider = MockNetworkProvider::new(id);
    let event_tx = provider.event_sender();
    let network = Arc::new(provider);
    let ws_transport = Arc::new(WebSocketTransport::new(network.clone(), ws_config(ws_port)));
    let session = Arc::new(PeerRegistry::new(network.clone(), ws_transport));
    session.start().await;

    let codec: Arc<dyn EnvelopeCodec> = Arc::new(JsonCodec);
    let node = Arc::new(crate::node::Node::from_parts(network, session, codec));
    (node, event_tx)
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_crdt_doc_new() {
    let ws_port = random_port().await;
    let (node, _event_tx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new(node, "test-doc").await.unwrap();

    assert_eq!(doc.doc_id(), "test-doc");

    // A fresh document should have an empty deep value.
    let value = doc.get_deep_value();
    // LoroValue for an empty doc is a Map with no entries.
    assert!(
        matches!(&value, loro::LoroValue::Map(m) if m.is_empty()),
        "expected empty map, got: {value:?}"
    );

    doc.stop().await;
}

#[tokio::test]
async fn test_local_change_event() {
    let ws_port = random_port().await;
    let (node, _event_tx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new(node, "test-doc").await.unwrap();
    let mut events = doc.subscribe();

    // Make a local change.
    doc.with_doc(|d| {
        let map = d.get_map("root");
        map.insert("key", "value").unwrap();
        d.commit();
    });

    // Should receive a LocalChange event.
    let event = tokio::time::timeout(Duration::from_millis(500), events.recv())
        .await
        .expect("should receive event within timeout")
        .expect("channel should not be closed");

    assert!(
        matches!(event, CrdtDocEvent::LocalChange),
        "expected LocalChange, got: {event:?}"
    );

    doc.stop().await;
}

#[tokio::test]
async fn test_stop_cancels_task() {
    let ws_port = random_port().await;
    let (node, _event_tx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new(node, "test-doc").await.unwrap();

    // Stop should not panic.
    doc.stop().await;

    // Calling stop again should be a no-op.
    doc.stop().await;
}

#[test]
fn test_memory_backend_noop() {
    let backend = MemoryCrdtBackend;

    // All operations are no-ops.
    assert!(backend.load_snapshot("doc").is_none());
    assert!(backend.load_updates("doc").is_empty());
    backend.save_update("doc", b"data");
    backend.save_snapshot("doc", b"snap");
    backend.remove("doc");

    // Still empty after "saving".
    assert!(backend.load_snapshot("doc").is_none());
    assert!(backend.load_updates("doc").is_empty());
}

#[test]
fn test_file_backend_snapshot_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let backend = CrdtFileBackend::new(dir.path());

    let snapshot_data = b"snapshot-binary-data-here";

    // Initially nothing.
    assert!(backend.load_snapshot("my-doc").is_none());

    // Save and load back.
    backend.save_snapshot("my-doc", snapshot_data);
    let loaded = backend.load_snapshot("my-doc").unwrap();
    assert_eq!(loaded, snapshot_data);

    // Overwrite with new snapshot.
    let new_snapshot = b"new-snapshot-data";
    backend.save_snapshot("my-doc", new_snapshot);
    let loaded2 = backend.load_snapshot("my-doc").unwrap();
    assert_eq!(loaded2, new_snapshot);

    // Non-existent doc returns None.
    assert!(backend.load_snapshot("no-doc").is_none());
}

#[test]
fn test_file_backend_updates_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let backend = CrdtFileBackend::new(dir.path());

    // Initially no updates.
    assert!(backend.load_updates("my-doc").is_empty());

    // Save some updates.
    backend.save_update("my-doc", b"update-1");
    backend.save_update("my-doc", b"update-2");
    backend.save_update("my-doc", b"update-3");

    let updates = backend.load_updates("my-doc");
    assert_eq!(updates.len(), 3);
    assert_eq!(updates[0], b"update-1");
    assert_eq!(updates[1], b"update-2");
    assert_eq!(updates[2], b"update-3");
}

#[test]
fn test_file_backend_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let backend = CrdtFileBackend::new(dir.path());

    // Save some updates.
    backend.save_update("my-doc", b"update-1");
    backend.save_update("my-doc", b"update-2");
    assert_eq!(backend.load_updates("my-doc").len(), 2);

    // Save a snapshot — should clear updates.
    backend.save_snapshot("my-doc", b"snapshot");
    assert_eq!(backend.load_updates("my-doc").len(), 0);
    assert_eq!(backend.load_snapshot("my-doc").unwrap(), b"snapshot");

    // New updates after snapshot.
    backend.save_update("my-doc", b"update-3");
    assert_eq!(backend.load_updates("my-doc").len(), 1);

    // Remove clears everything.
    backend.remove("my-doc");
    assert!(backend.load_snapshot("my-doc").is_none());
    assert!(backend.load_updates("my-doc").is_empty());
}

#[tokio::test]
async fn test_doc_id_accessor() {
    let ws_port = random_port().await;
    let (node, _event_tx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new(node, "my-special-doc").await.unwrap();
    assert_eq!(doc.doc_id(), "my-special-doc");

    doc.stop().await;
}

#[tokio::test]
async fn test_version_vector() {
    let ws_port = random_port().await;
    let (node, _event_tx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new(node, "test-doc").await.unwrap();

    // Initial version vector.
    let vv1 = doc.version_vector();

    // Make a change.
    doc.with_doc(|d| {
        let map = d.get_map("root");
        map.insert("key", "value").unwrap();
        d.commit();
    });

    // Version vector should have changed.
    let vv2 = doc.version_vector();
    assert_ne!(
        vv1.encode(),
        vv2.encode(),
        "version vector should change after edit"
    );

    doc.stop().await;
}

#[tokio::test]
async fn test_export_import() {
    let ws_port = random_port().await;
    let (node, _event_tx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new(node.clone(), "test-doc").await.unwrap();

    // Make a change.
    doc.with_doc(|d| {
        let map = d.get_map("root");
        map.insert("key", 42).unwrap();
        d.commit();
    });

    // Export snapshot.
    let snapshot = doc.export_snapshot().unwrap();
    assert!(!snapshot.is_empty());

    doc.stop().await;

    // Create a second doc and import the snapshot.
    let ws_port2 = random_port().await;
    let (node2, _event_tx2) = make_test_node("device-b", ws_port2).await;
    let doc2 = CrdtDoc::new(node2, "test-doc-2").await.unwrap();

    doc2.import(&snapshot).unwrap();

    // Verify the data came through.
    let value = doc2.get_deep_value();
    if let loro::LoroValue::Map(map) = &value {
        if let Some(loro::LoroValue::Map(root)) = map.get("root") {
            if let Some(loro::LoroValue::I64(v)) = root.get("key") {
                assert_eq!(*v, 42);
            } else {
                panic!("expected i64 value for 'key', got: {root:?}");
            }
        } else {
            panic!("expected 'root' map, got: {map:?}");
        }
    } else {
        panic!("expected Map, got: {value:?}");
    }

    doc2.stop().await;
}

// ── Persistence integration tests ──────────────────────────────────

/// Helper: wait for a LocalChange event (the sync task has processed the
/// commit and persisted the update to the backend).
async fn wait_for_local_change(events: &mut broadcast::Receiver<CrdtDocEvent>) {
    let ev = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("timed out waiting for LocalChange")
        .expect("event channel closed");
    assert!(
        matches!(ev, CrdtDocEvent::LocalChange),
        "expected LocalChange, got: {ev:?}"
    );
}

/// Helper: extract a string value from a nested map path in LoroValue.
/// E.g. `extract_str(&value, "root", "key")` reads `value["root"]["key"]`.
fn extract_str(value: &loro::LoroValue, container: &str, key: &str) -> Option<String> {
    if let loro::LoroValue::Map(top) = value {
        if let Some(loro::LoroValue::Map(inner)) = top.get(container) {
            if let Some(loro::LoroValue::String(s)) = inner.get(key) {
                return Some(s.to_string());
            }
        }
    }
    None
}

#[tokio::test]
async fn test_persistence_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(CrdtFileBackend::new(dir.path()));

    // ── First session: create doc, insert data, stop ──
    let ws_port = random_port().await;
    let (node, _etx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new_with_backend(node, "persist-doc", backend.clone())
        .await
        .unwrap();
    let mut events = doc.subscribe();

    doc.with_doc(|d| {
        let map = d.get_map("root");
        map.insert("greeting", "hello").unwrap();
        d.commit();
    });
    wait_for_local_change(&mut events).await;
    doc.stop().await;

    // ── Second session: recreate doc from the same backend ──
    let ws_port2 = random_port().await;
    let (node2, _etx2) = make_test_node("device-b", ws_port2).await;

    let doc2 = CrdtDoc::new_with_backend(node2, "persist-doc", backend.clone())
        .await
        .unwrap();

    let value = doc2.get_deep_value();
    let greeting = extract_str(&value, "root", "greeting");
    assert_eq!(
        greeting.as_deref(),
        Some("hello"),
        "data should survive restart; got: {value:?}"
    );

    doc2.stop().await;
}

#[tokio::test]
async fn test_persistence_with_multiple_edits() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(CrdtFileBackend::new(dir.path()));

    // ── First session: 5 separate edits ──
    let ws_port = random_port().await;
    let (node, _etx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new_with_backend(node, "multi-doc", backend.clone())
        .await
        .unwrap();
    let mut events = doc.subscribe();

    for i in 0..5 {
        let key = format!("key_{i}");
        let val = format!("val_{i}");
        doc.with_doc(|d| {
            let map = d.get_map("data");
            map.insert(&key, val.as_str()).unwrap();
            d.commit();
        });
        wait_for_local_change(&mut events).await;
    }
    doc.stop().await;

    // ── Second session: verify all 5 keys ──
    let ws_port2 = random_port().await;
    let (node2, _etx2) = make_test_node("device-b", ws_port2).await;

    let doc2 = CrdtDoc::new_with_backend(node2, "multi-doc", backend.clone())
        .await
        .unwrap();

    let value = doc2.get_deep_value();
    for i in 0..5 {
        let key = format!("key_{i}");
        let expected = format!("val_{i}");
        let actual = extract_str(&value, "data", &key);
        assert_eq!(
            actual.as_deref(),
            Some(expected.as_str()),
            "missing key {key} after restart; got: {value:?}"
        );
    }

    doc2.stop().await;
}

#[tokio::test]
async fn test_compaction_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(CrdtFileBackend::new(dir.path()));

    // ── First session: insert data, then compact ──
    let ws_port = random_port().await;
    let (node, _etx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new_with_backend(node, "compact-doc", backend.clone())
        .await
        .unwrap();
    let mut events = doc.subscribe();

    // Insert several keys.
    for i in 0..3 {
        let key = format!("item_{i}");
        let val = format!("value_{i}");
        doc.with_doc(|d| {
            let map = d.get_map("store");
            map.insert(&key, val.as_str()).unwrap();
            d.commit();
        });
        wait_for_local_change(&mut events).await;
    }

    // Explicitly compact — this saves a snapshot and clears update files.
    doc.compact().unwrap();

    // Verify that the backend now has a snapshot and no updates.
    assert!(
        backend.load_snapshot("compact-doc").is_some(),
        "snapshot should exist after compact"
    );
    assert!(
        backend.load_updates("compact-doc").is_empty(),
        "updates should be cleared after compact"
    );

    doc.stop().await;

    // ── Second session: recreate from compacted snapshot ──
    let ws_port2 = random_port().await;
    let (node2, _etx2) = make_test_node("device-b", ws_port2).await;

    let doc2 = CrdtDoc::new_with_backend(node2, "compact-doc", backend.clone())
        .await
        .unwrap();

    let value = doc2.get_deep_value();
    for i in 0..3 {
        let key = format!("item_{i}");
        let expected = format!("value_{i}");
        let actual = extract_str(&value, "store", &key);
        assert_eq!(
            actual.as_deref(),
            Some(expected.as_str()),
            "compacted data missing {key}; got: {value:?}"
        );
    }

    doc2.stop().await;
}

#[tokio::test]
async fn test_file_backend_remove() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(CrdtFileBackend::new(dir.path()));

    // ── Save some data ──
    let ws_port = random_port().await;
    let (node, _etx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new_with_backend(node, "remove-doc", backend.clone())
        .await
        .unwrap();
    let mut events = doc.subscribe();

    doc.with_doc(|d| {
        let map = d.get_map("root");
        map.insert("x", "y").unwrap();
        d.commit();
    });
    wait_for_local_change(&mut events).await;

    // Compact so we have both a snapshot and (empty) updates dir cleared.
    doc.compact().unwrap();
    assert!(backend.load_snapshot("remove-doc").is_some());

    doc.stop().await;

    // ── Remove all persisted data for this doc ──
    backend.remove("remove-doc");

    assert!(
        backend.load_snapshot("remove-doc").is_none(),
        "snapshot should be gone after remove"
    );
    assert!(
        backend.load_updates("remove-doc").is_empty(),
        "updates should be gone after remove"
    );

    // ── Recreating from the same backend should yield an empty doc ──
    let ws_port2 = random_port().await;
    let (node2, _etx2) = make_test_node("device-b", ws_port2).await;

    let doc2 = CrdtDoc::new_with_backend(node2, "remove-doc", backend.clone())
        .await
        .unwrap();

    let value = doc2.get_deep_value();
    assert!(
        matches!(&value, loro::LoroValue::Map(m) if m.is_empty()),
        "expected empty map after remove + recreate, got: {value:?}"
    );

    doc2.stop().await;
}

#[tokio::test]
async fn test_restore_sets_version_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(CrdtFileBackend::new(dir.path()));

    // ── First session: create doc, insert data ──
    let ws_port = random_port().await;
    let (node, _etx) = make_test_node("device-a", ws_port).await;

    let doc = CrdtDoc::new_with_backend(node, "version-doc", backend.clone())
        .await
        .unwrap();
    let mut events = doc.subscribe();

    doc.with_doc(|d| {
        let map = d.get_map("root");
        map.insert("a", "1").unwrap();
        d.commit();
    });
    wait_for_local_change(&mut events).await;

    let vv_before = doc.version_vector();
    doc.stop().await;

    // ── Second session: restore, then make a NEW edit ──
    let ws_port2 = random_port().await;
    let (node2, _etx2) = make_test_node("device-b", ws_port2).await;

    let doc2 = CrdtDoc::new_with_backend(node2, "version-doc", backend.clone())
        .await
        .unwrap();

    // Version vector after restore should match the one before stop.
    let vv_restored = doc2.version_vector();
    assert_eq!(
        vv_before.encode(),
        vv_restored.encode(),
        "version vector should be restored correctly"
    );

    // Make a new edit — should succeed (version counter must be higher).
    doc2.with_doc(|d| {
        let map = d.get_map("root");
        map.insert("b", "2").unwrap();
        d.commit();
    });

    let vv_after = doc2.version_vector();
    assert_ne!(
        vv_restored.encode(),
        vv_after.encode(),
        "version vector should advance after new edit on restored doc"
    );

    // Verify both old and new data are present.
    let value = doc2.get_deep_value();
    assert_eq!(extract_str(&value, "root", "a").as_deref(), Some("1"));
    assert_eq!(extract_str(&value, "root", "b").as_deref(), Some("2"));

    doc2.stop().await;
}
