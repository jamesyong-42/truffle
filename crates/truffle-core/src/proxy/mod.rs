//! Reverse proxy subsystem.
//!
//! Exposes local HTTP/HTTPS ports to the Tailscale mesh network.
//! The Go sidecar handles the actual HTTP reverse proxying (TLS listener,
//! `httputil.ReverseProxy`, WebSocket hijack). This module provides the
//! Rust API surface and mesh discovery.

pub mod discovery;
pub mod types;

use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;

pub use types::*;

use crate::network::{NetworkProvider, ProxyAddParams};
use crate::node::{Node, NodeError};

/// Internal state for the proxy subsystem, stored in [`Node`].
///
/// **Limitation**: [`ProxyEvent::Error`] is currently only emitted during
/// [`Proxy::add()`] and [`Proxy::remove()`] calls (synchronous failures).
/// Post-start runtime errors from the sidecar (e.g. `SERVE_ERROR`,
/// `CONNECTION_REFUSED`) are logged but *not* forwarded to `event_tx`.
/// Addressing this requires a persistent background task that subscribes to
/// sidecar proxy error events — tracked as a future enhancement.
pub(crate) struct ProxyState {
    /// Broadcast channel for proxy lifecycle events.
    pub(crate) event_tx: broadcast::Sender<ProxyEvent>,
    /// Currently active proxies (local to this node).
    pub(crate) proxies: Mutex<HashMap<String, ProxyInfo>>,
}

impl ProxyState {
    pub(crate) fn new() -> Self {
        let (event_tx, _) = broadcast::channel(64);
        Self {
            event_tx,
            proxies: Mutex::new(HashMap::new()),
        }
    }

    /// Forward an error from the sidecar to the proxy event channel.
    ///
    /// Currently unused — see the limitation note on [`ProxyState`]. Provided
    /// so that a future background error-forwarding task can call it without
    /// reaching into `event_tx` directly.
    #[allow(dead_code)]
    pub(crate) fn emit_error(&self, id: String, code: String, message: String) {
        let _ = self.event_tx.send(ProxyEvent::Error { id, code, message });
    }
}

/// Handle for interacting with the reverse proxy subsystem.
///
/// Obtained via [`Node::proxy()`]. Follows the same pattern as
/// [`FileTransfer`](crate::file_transfer::FileTransfer).
pub struct Proxy<'a, N: NetworkProvider + 'static> {
    #[allow(dead_code)]
    node: &'a Node<N>,
}

impl<'a, N: NetworkProvider + 'static> Proxy<'a, N> {
    pub(crate) fn new(node: &'a Node<N>) -> Self {
        Self { node }
    }

    fn state(&self) -> &ProxyState {
        &self.node.proxy_state
    }

    /// Subscribe to proxy lifecycle events.
    pub fn subscribe(&self) -> broadcast::Receiver<ProxyEvent> {
        self.state().event_tx.subscribe()
    }

    /// Get a snapshot of all locally active proxies.
    pub fn list(&self) -> Vec<ProxyInfo> {
        self.state()
            .proxies
            .lock()
            .expect("proxy state poisoned")
            .values()
            .cloned()
            .collect()
    }

    /// Add a reverse proxy that forwards traffic from a TLS port on the mesh
    /// to a local target.
    ///
    /// Sends the `proxy:add` command to the sidecar and waits for confirmation.
    /// On success, stores the proxy in local state and emits a
    /// [`ProxyEvent::Started`] event.
    pub async fn add(&self, config: ProxyConfig) -> Result<ProxyInfo, NodeError> {
        let params = ProxyAddParams {
            id: config.id.clone(),
            name: config.name.clone(),
            listen_port: config.listen_port,
            target_host: config.target.host.clone(),
            target_port: config.target.port,
            target_scheme: config.target.scheme.clone(),
        };

        let result = self.node.network.proxy_add(params).await?;

        let info = ProxyInfo {
            id: result.id.clone(),
            name: config.name,
            listen_port: result.listen_port,
            target: config.target,
            url: result.url.clone(),
            status: ProxyStatus::Running,
        };

        // Store in local state
        self.state()
            .proxies
            .lock()
            .expect("proxy state poisoned")
            .insert(info.id.clone(), info.clone());

        // Emit event
        let _ = self.state().event_tx.send(ProxyEvent::Started {
            id: result.id,
            url: result.url,
            listen_port: result.listen_port,
        });

        Ok(info)
    }

    /// Remove a reverse proxy by ID.
    ///
    /// Sends the `proxy:remove` command to the sidecar and waits for
    /// confirmation. On success, removes the proxy from local state and
    /// emits a [`ProxyEvent::Stopped`] event.
    pub async fn remove(&self, id: &str) -> Result<(), NodeError> {
        self.node.network.proxy_remove(id).await?;

        // Remove from local state
        self.state()
            .proxies
            .lock()
            .expect("proxy state poisoned")
            .remove(id);

        // Emit event
        let _ = self
            .state()
            .event_tx
            .send(ProxyEvent::Stopped { id: id.to_string() });

        Ok(())
    }
}
