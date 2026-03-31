//! Background sync task for SyncedStore.
//!
//! Handles three event sources via `tokio::select!`:
//! 1. Incoming sync messages from peers (namespace `"ss:{store_id}"`)
//! 2. Peer join/leave events (for sync-on-join and cleanup-on-leave)
//! 3. Outbound broadcast requests from `SyncedStore::set()`

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};

use crate::network::NetworkProvider;
use crate::node::Node;
use crate::session::PeerEvent;

use super::types::{Slice, StoreEvent, SyncMessage};
use super::StoreInner;

/// Spawn the background sync task.
///
/// Returns a `JoinHandle` that can be aborted to stop the task.
pub(super) fn spawn_sync_task<N, T>(
    node: Arc<Node<N>>,
    inner: Arc<StoreInner<T>>,
    mut broadcast_rx: mpsc::UnboundedReceiver<SyncMessage>,
) -> tokio::task::JoinHandle<()>
where
    N: NetworkProvider + 'static,
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let namespace = format!("ss:{}", inner.store_id);

    tokio::spawn(async move {
        let mut msg_rx = node.subscribe(&namespace);
        let mut peer_rx = node.on_peer_change();

        tracing::info!(store = inner.store_id.as_str(), "synced_store: sync task started");

        loop {
            tokio::select! {
                // ── Source 1: incoming sync messages from peers ──
                result = msg_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            handle_incoming_message(&node, &inner, &namespace, &msg.from, msg.payload).await;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                store = inner.store_id.as_str(),
                                missed = n,
                                "synced_store: message receiver lagged"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::debug!(store = inner.store_id.as_str(), "synced_store: message channel closed");
                            break;
                        }
                    }
                }

                // ── Source 2: peer join/leave events ──
                result = peer_rx.recv() => {
                    match result {
                        Ok(PeerEvent::Joined(state)) => {
                            handle_peer_joined(&node, &inner, &namespace, &state.id).await;
                        }
                        Ok(PeerEvent::Left(peer_id)) => {
                            handle_peer_left(&inner, &peer_id).await;
                        }
                        Ok(_) => {} // Updated, WsConnected, WsDisconnected, AuthRequired — ignore
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                store = inner.store_id.as_str(),
                                missed = n,
                                "synced_store: peer event receiver lagged"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::debug!(store = inner.store_id.as_str(), "synced_store: peer event channel closed");
                            break;
                        }
                    }
                }

                // ── Source 3: outbound broadcast requests from set() ──
                Some(sync_msg) = broadcast_rx.recv() => {
                    let msg_type = match &sync_msg {
                        SyncMessage::Update { .. } => "update",
                        SyncMessage::Full { .. } => "full",
                        SyncMessage::Request { .. } => "request",
                        SyncMessage::Clear { .. } => "clear",
                    };
                    if let Ok(payload) = serde_json::to_value(&sync_msg) {
                        node.broadcast_typed(&namespace, msg_type, &payload).await;
                    }
                }
            }
        }

        tracing::info!(store = inner.store_id.as_str(), "synced_store: sync task stopped");
    })
}

/// Handle an incoming sync message from a peer.
async fn handle_incoming_message<N, T>(
    node: &Node<N>,
    inner: &StoreInner<T>,
    namespace: &str,
    from: &str,
    payload: serde_json::Value,
) where
    N: NetworkProvider + 'static,
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let sync_msg: SyncMessage = match serde_json::from_value(payload) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                store = inner.store_id.as_str(),
                from = from,
                "synced_store: failed to parse sync message: {e}"
            );
            return;
        }
    };

    match sync_msg {
        SyncMessage::Update {
            device_id,
            data,
            version,
            updated_at,
        }
        | SyncMessage::Full {
            device_id,
            data,
            version,
            updated_at,
        } => {
            apply_remote_slice(inner, &device_id, data, version, updated_at).await;
        }

        SyncMessage::Request {} => {
            // Peer is requesting our current slice.
            let local = inner.local.read().await;
            if let Some(slice) = local.as_ref() {
                let full = SyncMessage::Full {
                    device_id: inner.device_id.clone(),
                    data: match serde_json::to_value(&slice.data) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::error!(
                                store = inner.store_id.as_str(),
                                "synced_store: failed to serialize local data: {e}"
                            );
                            return;
                        }
                    },
                    version: slice.version,
                    updated_at: slice.updated_at,
                };
                if let Ok(payload) = serde_json::to_value(&full) {
                    if let Err(e) = node.send_typed(from, namespace, "full", &payload).await {
                        tracing::warn!(
                            store = inner.store_id.as_str(),
                            peer = from,
                            "synced_store: failed to send Full response: {e}"
                        );
                    }
                }
            }
        }

        SyncMessage::Clear { device_id } => {
            let mut remotes = inner.remotes.write().await;
            if remotes.remove(&device_id).is_some() {
                let _ = inner.event_tx.send(StoreEvent::PeerRemoved {
                    device_id: device_id.clone(),
                });
                tracing::debug!(
                    store = inner.store_id.as_str(),
                    device = device_id.as_str(),
                    "synced_store: cleared remote slice (Clear message)"
                );
            }
        }
    }
}

/// Apply a remote slice if the version is newer.
async fn apply_remote_slice<T>(
    inner: &StoreInner<T>,
    device_id: &str,
    data: serde_json::Value,
    version: u64,
    updated_at: u64,
) where
    T: DeserializeOwned + Clone + Send + Sync + 'static,
{
    // Don't apply our own data back.
    if device_id == inner.device_id {
        return;
    }

    let typed_data: T = match serde_json::from_value(data) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                store = inner.store_id.as_str(),
                device = device_id,
                "synced_store: failed to deserialize remote data: {e}"
            );
            return;
        }
    };

    let mut remotes = inner.remotes.write().await;

    // Check version — only accept if newer.
    if let Some(existing) = remotes.get(device_id) {
        if version <= existing.version {
            tracing::debug!(
                store = inner.store_id.as_str(),
                device = device_id,
                existing_v = existing.version,
                incoming_v = version,
                "synced_store: stale update rejected"
            );
            return;
        }
    }

    let slice = Slice {
        device_id: device_id.to_string(),
        data: typed_data.clone(),
        version,
        updated_at,
    };

    remotes.insert(device_id.to_string(), slice);

    let _ = inner.event_tx.send(StoreEvent::PeerUpdated {
        device_id: device_id.to_string(),
        data: typed_data,
        version,
    });

    tracing::debug!(
        store = inner.store_id.as_str(),
        device = device_id,
        version = version,
        "synced_store: applied remote slice"
    );
}

/// Handle a peer joining: send them a Request for their data.
async fn handle_peer_joined<N, T>(
    node: &Node<N>,
    inner: &StoreInner<T>,
    namespace: &str,
    peer_id: &str,
) where
    N: NetworkProvider + 'static,
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    // Send Request to the new peer.
    let request = SyncMessage::Request {};
    if let Ok(payload) = serde_json::to_value(&request) {
        if let Err(e) = node.send_typed(peer_id, namespace, "request", &payload).await {
            tracing::warn!(
                store = inner.store_id.as_str(),
                peer = peer_id,
                "synced_store: failed to send Request to new peer: {e}"
            );
        }
    }

    // Also send our current data to the new peer (proactive full sync).
    let local = inner.local.read().await;
    if let Some(slice) = local.as_ref() {
        let full = SyncMessage::Full {
            device_id: inner.device_id.clone(),
            data: match serde_json::to_value(&slice.data) {
                Ok(v) => v,
                Err(_) => return,
            },
            version: slice.version,
            updated_at: slice.updated_at,
        };
        if let Ok(payload) = serde_json::to_value(&full) {
            let _ = node.send_typed(peer_id, namespace, "full", &payload).await;
        }
    }
}

/// Handle a peer leaving: remove their slice and emit PeerRemoved.
async fn handle_peer_left<T>(inner: &StoreInner<T>, peer_id: &str)
where
    T: Clone + Send + Sync + 'static,
{
    let mut remotes = inner.remotes.write().await;
    if remotes.remove(peer_id).is_some() {
        let _ = inner.event_tx.send(StoreEvent::PeerRemoved {
            device_id: peer_id.to_string(),
        });
        tracing::debug!(
            store = inner.store_id.as_str(),
            device = peer_id,
            "synced_store: removed peer slice (peer left)"
        );
    }
}
