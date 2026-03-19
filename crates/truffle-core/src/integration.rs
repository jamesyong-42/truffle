//! Integration helpers for wiring truffle-core components together.
//!
//! These functions handle the boilerplate of connecting `StoreSyncAdapter`
//! and `FileTransferAdapter` to a `MeshNode`'s event broadcast and message
//! routing. They spawn background tasks that run until the channel closes.

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};

use crate::services::file_transfer::adapter::FileTransferAdapter;
use crate::mesh::node::{MeshNode, MeshNodeEvent};
use crate::services::store_sync::adapter::StoreSyncAdapter;
use crate::services::store_sync::types::{OutgoingSyncMessage, SyncMessage, STORE_SYNC_NAMESPACE};
use crate::services::file_transfer::adapter::FILE_TRANSFER_NAMESPACE;
use crate::protocol::envelope::MeshEnvelope;

/// Wire a `StoreSyncAdapter` to a `MeshNode`.
///
/// Spawns two background tasks:
/// 1. **Incoming pump**: Subscribes to MeshNode events, routes `sync` namespace
///    messages to the adapter, and handles device discovery/offline events.
/// 2. **Outgoing pump**: Reads from the adapter's outgoing channel and
///    broadcasts via MeshNode.
///
/// Returns `JoinHandle`s for both tasks so the caller can abort them on shutdown.
pub fn wire_store_sync(
    mesh_node: &Arc<MeshNode>,
    adapter: &Arc<StoreSyncAdapter>,
    mut outgoing_rx: mpsc::UnboundedReceiver<OutgoingSyncMessage>,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let adapt = Arc::clone(adapter);
    let mut event_rx = mesh_node.subscribe_events();

    let incoming_handle = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(MeshNodeEvent::Message(msg)) if msg.namespace == STORE_SYNC_NAMESPACE => {
                    let sync_msg = SyncMessage {
                        from: msg.from,
                        msg_type: msg.msg_type,
                        payload: msg.payload,
                    };
                    adapt.handle_sync_message(&sync_msg).await;
                }
                Ok(MeshNodeEvent::DeviceDiscovered(device)) => {
                    adapt.handle_device_discovered(&device.id).await;
                }
                Ok(MeshNodeEvent::DeviceOffline(id)) => {
                    adapt.handle_device_offline(&id).await;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Store sync event receiver lagged by {n}");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let node = Arc::clone(mesh_node);
    let outgoing_handle = tokio::spawn(async move {
        while let Some(msg) = outgoing_rx.recv().await {
            let envelope = MeshEnvelope::new(
                STORE_SYNC_NAMESPACE,
                &msg.msg_type,
                msg.payload,
            );
            node.broadcast_envelope(&envelope).await;
        }
    });

    (incoming_handle, outgoing_handle)
}

/// Wire a `FileTransferAdapter` to a `MeshNode`.
///
/// Spawns two background tasks:
/// 1. **Incoming pump**: Subscribes to MeshNode events, routes `file-transfer`
///    namespace messages to the adapter.
/// 2. **Outgoing pump**: Reads from the adapter's outgoing channel and sends
///    targeted messages via MeshNode.
///
/// Returns `JoinHandle`s for both tasks so the caller can abort them on shutdown.
pub fn wire_file_transfer(
    mesh_node: &Arc<MeshNode>,
    adapter: &Arc<FileTransferAdapter>,
    mut outgoing_rx: mpsc::UnboundedReceiver<(String, String, String)>,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let adapt = Arc::clone(adapter);
    let mut event_rx = mesh_node.subscribe_events();

    let incoming_handle = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(MeshNodeEvent::Message(msg)) if msg.namespace == FILE_TRANSFER_NAMESPACE => {
                    adapt.handle_bus_message(&msg.msg_type, &msg.payload).await;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("File transfer event receiver lagged by {n}");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let node = Arc::clone(mesh_node);
    let outgoing_handle = tokio::spawn(async move {
        while let Some((target_id, msg_type, payload_json)) = outgoing_rx.recv().await {
            let payload: serde_json::Value = serde_json::from_str(&payload_json)
                .unwrap_or(serde_json::Value::Null);
            let envelope = MeshEnvelope::new(
                FILE_TRANSFER_NAMESPACE,
                &msg_type,
                payload,
            );
            node.send_envelope(&target_id, &envelope).await;
        }
    });

    (incoming_handle, outgoing_handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::node::MeshNodeConfig;
    use crate::mesh::node::MeshTimingConfig;
    use crate::transport::connection::{ConnectionManager, TransportConfig};

    fn test_node() -> (Arc<MeshNode>, broadcast::Receiver<MeshNodeEvent>) {
        let transport_config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(transport_config);
        let (node, event_rx) = MeshNode::new(
            MeshNodeConfig {
                device_id: "test-dev".to_string(),
                device_name: "Test".to_string(),
                device_type: "desktop".to_string(),
                hostname_prefix: "app".to_string(),
                prefer_primary: false,
                capabilities: vec![],
                metadata: None,
                timing: MeshTimingConfig::default(),
            },
            Arc::new(conn_mgr),
        );
        (Arc::new(node), event_rx)
    }

    #[tokio::test]
    async fn wire_store_sync_receives_events() {
        let (node, _event_rx) = test_node();
        let (outgoing_tx, outgoing_rx) = mpsc::unbounded_channel();

        let adapter = StoreSyncAdapter::new(
            crate::services::store_sync::adapter::StoreSyncAdapterConfig {
                local_device_id: "test-dev".to_string(),
            },
            vec![],
            outgoing_tx,
        );

        let (h1, h2) = wire_store_sync(&node, &adapter, outgoing_rx);

        // Handles are spawned and running
        assert!(!h1.is_finished());
        assert!(!h2.is_finished());

        h1.abort();
        h2.abort();
    }

    #[tokio::test]
    async fn subscribe_events_multiple_receivers() {
        let (node, mut rx1) = test_node();
        let mut rx2 = node.subscribe_events();

        node.start().await;

        // Both receivers get the Started event
        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert!(matches!(e1, MeshNodeEvent::Started));
        assert!(matches!(e2, MeshNodeEvent::Started));

        node.stop().await;
    }
}
