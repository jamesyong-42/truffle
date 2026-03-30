//! Truffle Tauri v2 plugin.
//!
//! Provides a Tauri plugin that wraps `truffle-core`'s `Node<TailscaleProvider>`
//! API, exposing mesh networking, messaging, and file transfer to the Tauri
//! frontend via commands and events.
//!
//! # Usage
//!
//! ```text
//! fn main() {
//!     tauri::Builder::default()
//!         .plugin(truffle_tauri_plugin::init())
//!         .run(tauri::generate_context!())
//!         .expect("error while running tauri application");
//! }
//! ```
//!
//! # Commands
//!
//! - `start(config)` — Start the truffle node
//! - `stop()` — Stop the node
//! - `get_local_info()` — Get local node identity
//! - `get_peers()` — List all known peers
//! - `ping(peer_id)` — Ping a peer
//! - `health()` — Get network health info
//! - `send_message(peer_id, namespace, data)` — Send a message
//! - `broadcast(namespace, data)` — Broadcast to all peers
//! - `send_file(peer_id, local_path, remote_path)` — Send a file
//! - `pull_file(peer_id, remote_path, local_path)` — Download a file
//! - `auto_accept(output_dir)` — Auto-accept incoming file offers
//! - `accept_offer(token, save_path)` — Accept a pending file offer
//! - `reject_offer(token, reason)` — Reject a pending file offer
//!
//! # Events
//!
//! - `truffle://peer-event` — Peer state changes
//! - `truffle://file-transfer-event` — File transfer progress/completion
//! - `truffle://file-offer` — Incoming file offers
//! - `truffle://auth-required` — Authentication URL when login is needed

use std::collections::HashMap;
use std::sync::Arc;

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime};
use tokio::sync::RwLock;

use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::{Node, OfferResponder};

pub mod commands;
pub mod events;
pub mod types;

/// Shared state managed by the Tauri plugin.
///
/// Holds the running `Node<TailscaleProvider>` and any pending file offer
/// responders that the frontend has not yet accepted or rejected.
pub struct TruffleState {
    /// The running truffle node, or `None` if not yet started.
    pub node: RwLock<Option<Arc<Node<TailscaleProvider>>>>,
    /// Pending file offer responders, keyed by transfer token.
    /// Removed on accept or reject.
    pub pending_offers: Arc<RwLock<HashMap<String, OfferResponder>>>,
}

/// Initialize the Truffle Tauri v2 plugin.
///
/// Register this in your Tauri app builder:
///
/// ```text
/// tauri::Builder::default()
///     .plugin(truffle_tauri_plugin::init())
/// ```
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("truffle")
        .invoke_handler(tauri::generate_handler![
            commands::start,
            commands::stop,
            commands::get_local_info,
            commands::get_peers,
            commands::ping,
            commands::health,
            commands::send_message,
            commands::broadcast,
            commands::send_file,
            commands::pull_file,
            commands::auto_accept,
            commands::accept_offer,
            commands::reject_offer,
        ])
        .setup(|app, _| {
            app.manage(TruffleState {
                node: RwLock::new(None),
                pending_offers: Arc::new(RwLock::new(HashMap::new())),
            });
            Ok(())
        })
        .build()
}
