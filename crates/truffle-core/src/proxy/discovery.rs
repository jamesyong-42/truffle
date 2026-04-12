//! Mesh discovery helpers for the reverse proxy subsystem.
//!
//! Uses [`SyncedStore`] to announce local proxies and discover remote proxies
//! across the mesh. The store is created at the binding layer (NAPI/Tauri)
//! because [`SyncedStore::new`] requires `Arc<Node>`.

use crate::proxy::types::{ProxyAnnouncement, ProxyInfo, RemoteProxy};
use crate::synced_store::SyncedStore;

/// Build a list of announcements from locally active proxies.
pub fn build_announcements(proxies: &[ProxyInfo]) -> Vec<ProxyAnnouncement> {
    proxies
        .iter()
        .map(|p| ProxyAnnouncement {
            id: p.id.clone(),
            name: p.name.clone(),
            listen_port: p.listen_port,
            url: p.url.clone(),
        })
        .collect()
}

/// Extract remote proxies from a SyncedStore, excluding the local device's own entries.
pub async fn extract_remote_proxies(
    store: &SyncedStore<Vec<ProxyAnnouncement>>,
    local_device_id: &str,
) -> Vec<RemoteProxy> {
    store
        .all()
        .await
        .into_iter()
        .filter(|(device_id, _)| device_id != local_device_id)
        .flat_map(|(device_id, slice)| {
            slice.data.into_iter().map(move |a| RemoteProxy {
                peer_id: device_id.clone(),
                peer_name: String::new(), // Resolved at the binding layer
                id: a.id,
                name: a.name,
                url: a.url,
                listen_port: a.listen_port,
            })
        })
        .collect()
}
