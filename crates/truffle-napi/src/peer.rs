//! NAPI `Peer` class — RFC 022 user-facing peer handle.
//!
//! Each live registry entry maps to at most one JS `Peer` instance per
//! `NapiNode` (keyed by `peer_ref`). Returning the same instance from
//! `getPeers` / events / `peer()` gives `===` identity for `Map<Peer, T>`.

use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::*;
use napi_derive::napi;
use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::Node;

/// Snapshot fields mirrored onto the JS `Peer` class.
///
/// Getters read from this snapshot; call [`Peer::refresh_from`] when the
/// registry updates the peer so live fields stay current without reallocating
/// the JS object (preserving `===`).
#[napi]
pub struct Peer {
    /// Process-local ref `{tailscaleId}:{generation}` — table key.
    peer_ref: String,
    /// Owning mesh node for `send` / `ping`.
    node: Arc<Node<TailscaleProvider>>,
    /// Latest projected fields from core.
    snapshot: Mutex<truffle_core::node::Peer>,
}

// Note: instances are not yet constructed from Rust (JS package interns
// handles). The class remains so napi-rs emits typings/exports for future
// Reference-table interning; getters/methods are ready for that wiring.

impl Peer {
    /// Routing string for core `send` / `ping` (prefer published ULID).
    fn route_id(&self) -> String {
        let snap = self.snapshot.lock().expect("peer snapshot lock");
        snap.device_id
            .clone()
            .unwrap_or_else(|| snap.tailscale_id.clone())
    }
}

#[napi]
impl Peer {
    /// Process-local opaque ref — safe Map key when object identity is
    /// inconvenient; do not persist across restarts.
    #[napi(getter)]
    pub fn peer_ref(&self) -> String {
        self.peer_ref.clone()
    }

    /// Best label for UI.
    #[napi(getter)]
    pub fn display_name(&self) -> String {
        self.snapshot
            .lock()
            .expect("peer snapshot lock")
            .display_name
            .clone()
    }

    /// Durable ULID once known; `null` until identity is learned.
    #[napi(getter)]
    pub fn device_id(&self) -> Option<String> {
        self.snapshot
            .lock()
            .expect("peer snapshot lock")
            .device_id
            .clone()
    }

    /// Human-readable name from hello identity, if known.
    #[napi(getter)]
    pub fn device_name(&self) -> Option<String> {
        self.snapshot
            .lock()
            .expect("peer snapshot lock")
            .device_name
            .clone()
    }

    /// Layer 3 Tailscale hostname.
    #[napi(getter)]
    pub fn hostname(&self) -> String {
        self.snapshot
            .lock()
            .expect("peer snapshot lock")
            .hostname
            .clone()
    }

    /// Tailscale stable node id (advanced / routing diagnostics).
    #[napi(getter)]
    pub fn tailscale_id(&self) -> String {
        self.snapshot
            .lock()
            .expect("peer snapshot lock")
            .tailscale_id
            .clone()
    }

    /// Registry generation (bumped on re-join).
    #[napi(getter)]
    pub fn generation(&self) -> u32 {
        self.snapshot.lock().expect("peer snapshot lock").generation as u32
    }

    #[napi(getter)]
    pub fn ip(&self) -> String {
        self.snapshot
            .lock()
            .expect("peer snapshot lock")
            .ip
            .to_string()
    }

    #[napi(getter)]
    pub fn online(&self) -> bool {
        self.snapshot.lock().expect("peer snapshot lock").online
    }

    /// Envelope-bus WebSocket up. Advanced — do not gate `send` on this.
    #[napi(getter)]
    pub fn ws_connected(&self) -> bool {
        self.snapshot
            .lock()
            .expect("peer snapshot lock")
            .ws_connected
    }

    #[napi(getter)]
    pub fn connection_type(&self) -> String {
        self.snapshot
            .lock()
            .expect("peer snapshot lock")
            .connection_type
            .clone()
    }

    #[napi(getter)]
    pub fn os(&self) -> Option<String> {
        self.snapshot.lock().expect("peer snapshot lock").os.clone()
    }

    #[napi(getter)]
    pub fn last_seen(&self) -> Option<String> {
        self.snapshot
            .lock()
            .expect("peer snapshot lock")
            .last_seen
            .clone()
    }

    /// Same registry entry generation (RFC 022 §7.7).
    #[napi]
    pub fn equals(&self, other: &Peer) -> bool {
        self.peer_ref == other.peer_ref
    }

    /// Send a namespaced message to this peer.
    #[napi]
    pub async fn send(&self, namespace: String, data: Buffer) -> Result<()> {
        let route = self.route_id();
        self.node
            .send(&route, &namespace, data.as_ref())
            .await
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Ping this peer.
    #[napi]
    pub async fn ping(&self) -> Result<crate::types::NapiPingResult> {
        let route = self.route_id();
        let result = self
            .node
            .ping(&route)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(crate::types::NapiPingResult {
            latency_ms: result.latency.as_secs_f64() * 1000.0,
            connection: result.connection,
            peer_addr: result.peer_addr,
        })
    }
}

// Instance interning for hard `===` is implemented in packages/core
// (`PeerRegistry` on `createMeshNode`). Native `Peer` class instances are
// still useful as method receivers when constructed explicitly; the JS
// package is the stable public surface for handle identity.
