//! Tauri commands for the truffle plugin.
//!
//! Each command accesses `TruffleState` from Tauri's managed state, reads the
//! `Node<TailscaleProvider>`, and delegates to the core API. All commands are
//! async and return `Result<T, String>` as required by Tauri.

use std::sync::Arc;

use tauri::{command, AppHandle, Emitter, Runtime, State};

use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::{Node, NodeBuilder};

use crate::events::start_event_forwarding;
use crate::types::*;
use crate::TruffleState;

// ---------------------------------------------------------------------------
// Helper: get a clone of the Arc<Node> from state, or error
// ---------------------------------------------------------------------------

async fn get_node(
    state: &TruffleState,
) -> Result<Arc<Node<TailscaleProvider>>, String> {
    state
        .node
        .read()
        .await
        .clone()
        .ok_or_else(|| "Node not started. Call `start` first.".to_string())
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Start the truffle node with the given configuration.
///
/// Creates a `Node<TailscaleProvider>` via the builder, stores it in state,
/// and starts event forwarding to the Tauri frontend.
#[command]
pub async fn start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, TruffleState>,
    config: StartConfig,
) -> Result<NodeIdentityJs, String> {
    // Prevent double-start
    {
        let existing = state.node.read().await;
        if existing.is_some() {
            return Err("Node is already running. Call `stop` first.".to_string());
        }
    }

    let mut builder = NodeBuilder::default()
        .name(&config.name)
        .sidecar_path(&config.sidecar_path);

    if let Some(ref dir) = config.state_dir {
        builder = builder.state_dir(dir);
    }
    if let Some(ref key) = config.auth_key {
        builder = builder.auth_key(key);
    }
    if config.ephemeral {
        builder = builder.ephemeral(true);
    }
    if let Some(port) = config.ws_port {
        builder = builder.ws_port(port);
    }

    // Build with auth handler that forwards auth URLs to the frontend
    let app_clone = app.clone();
    let node = builder
        .build_with_auth_handler(move |url| {
            if let Err(e) = app_clone.emit("truffle://auth-required", &url) {
                tracing::warn!("Failed to emit auth-required event: {e}");
            }
        })
        .await
        .map_err(|e| e.to_string())?;

    let info = node.local_info();
    let arc = Arc::new(node);

    // Start event forwarding
    start_event_forwarding(&app, &arc, &state.pending_offers);

    // Store in state
    *state.node.write().await = Some(arc);

    Ok(info.into())
}

/// Stop the truffle node and clean up state.
#[command]
pub async fn stop(state: State<'_, TruffleState>) -> Result<(), String> {
    let node = state.node.write().await.take();
    match node {
        Some(n) => {
            n.stop().await;
            // Clear pending offers
            state.pending_offers.write().await.clear();
            Ok(())
        }
        None => Err("Node is not running.".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Identity & Discovery
// ---------------------------------------------------------------------------

/// Get the local node's identity.
#[command]
pub async fn get_local_info(
    state: State<'_, TruffleState>,
) -> Result<NodeIdentityJs, String> {
    let node = get_node(&state).await?;
    Ok(node.local_info().into())
}

/// Get all known peers.
#[command]
pub async fn get_peers(
    state: State<'_, TruffleState>,
) -> Result<Vec<PeerJs>, String> {
    let node = get_node(&state).await?;
    let peers = node.peers().await;
    Ok(peers.into_iter().map(PeerJs::from).collect())
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Ping a peer by ID or name.
#[command]
pub async fn ping(
    state: State<'_, TruffleState>,
    peer_id: String,
) -> Result<PingResultJs, String> {
    let node = get_node(&state).await?;
    let result = node.ping(&peer_id).await.map_err(|e| e.to_string())?;
    Ok(result.into())
}

/// Get health information from the network layer.
#[command]
pub async fn health(
    state: State<'_, TruffleState>,
) -> Result<HealthInfoJs, String> {
    let node = get_node(&state).await?;
    Ok(node.health().await.into())
}

// ---------------------------------------------------------------------------
// Messaging
// ---------------------------------------------------------------------------

/// Send a namespaced message to a specific peer.
#[command]
pub async fn send_message(
    state: State<'_, TruffleState>,
    peer_id: String,
    namespace: String,
    data: Vec<u8>,
) -> Result<(), String> {
    let node = get_node(&state).await?;
    node.send(&peer_id, &namespace, &data)
        .await
        .map_err(|e| e.to_string())
}

/// Broadcast a namespaced message to all connected peers.
#[command]
pub async fn broadcast(
    state: State<'_, TruffleState>,
    namespace: String,
    data: Vec<u8>,
) -> Result<(), String> {
    let node = get_node(&state).await?;
    node.broadcast(&namespace, &data).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// File Transfer
// ---------------------------------------------------------------------------

/// Send a file to a peer.
#[command]
pub async fn send_file(
    state: State<'_, TruffleState>,
    peer_id: String,
    local_path: String,
    remote_path: String,
) -> Result<TransferResultJs, String> {
    let node = get_node(&state).await?;
    let ft = node.file_transfer();
    let result = ft
        .send_file(&peer_id, &local_path, &remote_path)
        .await
        .map_err(|e| e.to_string())?;
    Ok(result.into())
}

/// Pull (download) a file from a peer.
#[command]
pub async fn pull_file(
    state: State<'_, TruffleState>,
    peer_id: String,
    remote_path: String,
    local_path: String,
) -> Result<TransferResultJs, String> {
    let node = get_node(&state).await?;
    let ft = node.file_transfer();
    let result = ft
        .pull_file(&peer_id, &remote_path, &local_path)
        .await
        .map_err(|e| e.to_string())?;
    Ok(result.into())
}

/// Enable auto-accept mode for incoming file offers.
///
/// All incoming offers will be automatically accepted and saved to `output_dir`.
#[command]
pub async fn auto_accept(
    state: State<'_, TruffleState>,
    output_dir: String,
) -> Result<(), String> {
    let node = get_node(&state).await?;
    let ft = node.file_transfer();
    ft.auto_accept(node.clone(), &output_dir).await;
    Ok(())
}

/// Accept an incoming file offer by its token.
///
/// The offer must have been received via the `truffle://file-offer` event.
/// The `save_path` is the local filesystem path where the file will be saved.
#[command]
pub async fn accept_offer(
    state: State<'_, TruffleState>,
    token: String,
    save_path: String,
) -> Result<(), String> {
    let responder = state
        .pending_offers
        .write()
        .await
        .remove(&token)
        .ok_or_else(|| format!("No pending offer with token: {token}"))?;

    responder.accept(&save_path);
    Ok(())
}

/// Reject an incoming file offer by its token.
///
/// The offer must have been received via the `truffle://file-offer` event.
#[command]
pub async fn reject_offer(
    state: State<'_, TruffleState>,
    token: String,
    reason: String,
) -> Result<(), String> {
    let responder = state
        .pending_offers
        .write()
        .await
        .remove(&token)
        .ok_or_else(|| format!("No pending offer with token: {token}"))?;

    responder.reject(&reason);
    Ok(())
}
