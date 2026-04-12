//! Background sync task for CrdtDoc.
//!
//! Handles three event sources via `tokio::select!`:
//! 1. Local updates from Loro's `subscribe_local_update` callback
//! 2. Incoming sync messages from peers (namespace `"crdt:{doc_id}"`)
//! 3. Peer join/leave events (for sync-on-join and cleanup-on-leave)

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};

use crate::network::NetworkProvider;
use crate::node::Node;
use crate::session::PeerEvent;

use super::backend::CrdtBackend;
use super::types::{CrdtDocEvent, CrdtSyncMessage};

/// Compaction threshold in bytes. When accumulated local updates exceed this
/// size, the document is compacted into a snapshot on disk.
const COMPACTION_THRESHOLD: usize = 256 * 1024; // 256 KB

/// Spawn the background sync task.
///
/// Returns a `JoinHandle` that can be aborted to stop the task.
pub(super) fn spawn_sync_task<N: NetworkProvider + 'static>(
    node: Arc<Node<N>>,
    doc: Arc<std::sync::Mutex<loro::LoroDoc>>,
    doc_id: String,
    backend: Arc<dyn CrdtBackend>,
    event_tx: broadcast::Sender<CrdtDocEvent>,
    mut local_update_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut compact_rx: mpsc::UnboundedReceiver<()>,
) -> tokio::task::JoinHandle<()> {
    let namespace = format!("crdt:{doc_id}");

    tokio::spawn(async move {
        let mut msg_rx = node.subscribe(&namespace);
        let mut peer_rx = node.on_peer_change();
        let mut accumulated_update_bytes: usize = 0;

        tracing::info!(doc_id = doc_id.as_str(), "crdt_doc: sync task started");

        loop {
            tokio::select! {
                // ── Arm 1: local updates from Loro subscribe_local_update ──
                Some(bytes) = local_update_rx.recv() => {
                    // Persist the update.
                    backend.save_update(&doc_id, &bytes);
                    accumulated_update_bytes += bytes.len();

                    // Broadcast to peers.
                    let sync_msg = CrdtSyncMessage::Update { data: bytes };
                    if let Ok(payload) = serde_json::to_value(&sync_msg) {
                        node.broadcast_typed(&namespace, "update", &payload).await;
                    }

                    // Compact if accumulated updates exceed threshold.
                    if accumulated_update_bytes > COMPACTION_THRESHOLD {
                        let snapshot = {
                            let doc = doc.lock().unwrap();
                            doc.export(loro::ExportMode::Snapshot)
                        };
                        match snapshot {
                            Ok(data) => {
                                backend.save_snapshot(&doc_id, &data);
                                accumulated_update_bytes = 0;
                                tracing::debug!(
                                    doc_id = doc_id.as_str(),
                                    "crdt_doc: compacted to snapshot"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    doc_id = doc_id.as_str(),
                                    "crdt_doc: compaction failed: {e}"
                                );
                            }
                        }
                    }

                    let _ = event_tx.send(CrdtDocEvent::LocalChange);
                }

                // ── Arm 2: incoming sync messages from peers ──
                result = msg_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let sync_msg: CrdtSyncMessage = match serde_json::from_value(msg.payload) {
                                Ok(m) => m,
                                Err(e) => {
                                    tracing::warn!(
                                        doc_id = doc_id.as_str(),
                                        from = msg.from.as_str(),
                                        "crdt_doc: failed to parse sync message: {e}"
                                    );
                                    continue;
                                }
                            };

                            match sync_msg {
                                CrdtSyncMessage::Update { data } => {
                                    let import_result = {
                                        let doc = doc.lock().unwrap();
                                        doc.import(&data)
                                    };
                                    match import_result {
                                        Ok(_) => {
                                            let _ = event_tx.send(CrdtDocEvent::RemoteChange {
                                                from: msg.from.clone(),
                                            });
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                doc_id = doc_id.as_str(),
                                                from = msg.from.as_str(),
                                                "crdt_doc: failed to import update: {e}"
                                            );
                                        }
                                    }
                                }

                                CrdtSyncMessage::SyncRequest { version_vector } => {
                                    // Compute delta from their version vector.
                                    let their_vv = match loro::VersionVector::decode(&version_vector) {
                                        Ok(vv) => vv,
                                        Err(e) => {
                                            tracing::warn!(
                                                doc_id = doc_id.as_str(),
                                                from = msg.from.as_str(),
                                                "crdt_doc: failed to decode version vector: {e}"
                                            );
                                            continue;
                                        }
                                    };

                                    let delta = {
                                        let doc = doc.lock().unwrap();
                                        doc.export(loro::ExportMode::updates(&their_vv))
                                    };

                                    match delta {
                                        Ok(data) => {
                                            let response = CrdtSyncMessage::SyncResponse { data };
                                            if let Ok(payload) = serde_json::to_value(&response) {
                                                if let Err(e) = node.send_typed(
                                                    &msg.from,
                                                    &namespace,
                                                    "sync_response",
                                                    &payload,
                                                ).await {
                                                    tracing::warn!(
                                                        doc_id = doc_id.as_str(),
                                                        peer = msg.from.as_str(),
                                                        "crdt_doc: failed to send SyncResponse: {e}"
                                                    );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                doc_id = doc_id.as_str(),
                                                "crdt_doc: failed to export delta: {e}"
                                            );
                                            // Fall back to sending a full snapshot.
                                            let snapshot = {
                                                let doc = doc.lock().unwrap();
                                                doc.export(loro::ExportMode::Snapshot)
                                            };
                                            if let Ok(data) = snapshot {
                                                let snap_msg = CrdtSyncMessage::Snapshot { data };
                                                if let Ok(payload) = serde_json::to_value(&snap_msg) {
                                                    let _ = node.send_typed(
                                                        &msg.from,
                                                        &namespace,
                                                        "snapshot",
                                                        &payload,
                                                    ).await;
                                                }
                                            }
                                        }
                                    }
                                }

                                CrdtSyncMessage::SyncResponse { data } | CrdtSyncMessage::Snapshot { data } => {
                                    let import_result = {
                                        let doc = doc.lock().unwrap();
                                        doc.import(&data)
                                    };
                                    match import_result {
                                        Ok(_) => {
                                            let _ = event_tx.send(CrdtDocEvent::PeerSynced {
                                                peer_id: msg.from.clone(),
                                            });
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                doc_id = doc_id.as_str(),
                                                from = msg.from.as_str(),
                                                "crdt_doc: failed to import sync response: {e}"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                doc_id = doc_id.as_str(),
                                missed = n,
                                "crdt_doc: message receiver lagged"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::debug!(doc_id = doc_id.as_str(), "crdt_doc: message channel closed");
                            break;
                        }
                    }
                }

                // ── Arm 3: peer join/leave events ──
                result = peer_rx.recv() => {
                    match result {
                        Ok(PeerEvent::Joined(state)) => {
                            // Send a SyncRequest with our version vector.
                            let vv_bytes = {
                                let doc = doc.lock().unwrap();
                                doc.state_vv().encode()
                            };
                            let request = CrdtSyncMessage::SyncRequest {
                                version_vector: vv_bytes,
                            };
                            if let Ok(payload) = serde_json::to_value(&request) {
                                if let Err(e) = node.send_typed(
                                    &state.id,
                                    &namespace,
                                    "sync_request",
                                    &payload,
                                ).await {
                                    tracing::warn!(
                                        doc_id = doc_id.as_str(),
                                        peer = state.id.as_str(),
                                        "crdt_doc: failed to send SyncRequest to new peer: {e}"
                                    );
                                }
                            }
                        }
                        Ok(PeerEvent::Left(peer_id)) => {
                            let _ = event_tx.send(CrdtDocEvent::PeerLeft {
                                peer_id: peer_id.clone(),
                            });
                            tracing::debug!(
                                doc_id = doc_id.as_str(),
                                peer = peer_id.as_str(),
                                "crdt_doc: peer left"
                            );
                        }
                        Ok(_) => {} // Updated, WsConnected, WsDisconnected, AuthRequired — ignore
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                doc_id = doc_id.as_str(),
                                missed = n,
                                "crdt_doc: peer event receiver lagged"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::debug!(doc_id = doc_id.as_str(), "crdt_doc: peer event channel closed");
                            break;
                        }
                    }
                }

                // ── Arm 4: manual compaction notification ──
                Some(()) = compact_rx.recv() => {
                    accumulated_update_bytes = 0;
                    tracing::debug!(
                        doc_id = doc_id.as_str(),
                        "crdt_doc: sync task reset accumulated_update_bytes after manual compact"
                    );
                }
            }
        }

        tracing::info!(doc_id = doc_id.as_str(), "crdt_doc: sync task stopped");
    })
}
