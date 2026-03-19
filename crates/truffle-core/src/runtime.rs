//! TruffleRuntime -- owns the full lifecycle of a Truffle mesh node.
//!
//! Wires together BridgeManager, GoShim, ConnectionManager, and MeshNode.
//! NAPI and Tauri consume this as a thin wrapper.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, oneshot, Mutex};

use crate::bridge::header::Direction;
use crate::bridge::manager::{BridgeConnection, BridgeManager, ChannelHandler};
use crate::bridge::shim::{GoShim, ShimConfig, ShimLifecycleEvent};
use crate::mesh::node::{MeshNode, MeshNodeConfig, MeshNodeEvent};
use crate::transport::connection::{ConnectionManager, TransportConfig};
use crate::types::TailnetPeer;

/// Configuration for the Truffle runtime.
pub struct RuntimeConfig {
    pub mesh: MeshNodeConfig,
    pub transport: TransportConfig,
    pub sidecar_path: Option<PathBuf>,
    pub state_dir: Option<String>,
    pub auth_key: Option<String>,
}

/// The full Truffle runtime -- owns sidecar, bridge, and mesh node.
pub struct TruffleRuntime {
    mesh_node: Arc<MeshNode>,
    connection_manager: Arc<ConnectionManager>,
    shim: Arc<Mutex<Option<GoShim>>>,
    bridge_pending_dials: Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>>,
}

impl TruffleRuntime {
    /// Create a new runtime (does not start anything).
    pub fn new(config: &RuntimeConfig) -> (Self, broadcast::Receiver<MeshNodeEvent>) {
        let (connection_manager, _transport_rx) = ConnectionManager::new(config.transport.clone());
        let connection_manager = Arc::new(connection_manager);
        let (mesh_node, event_rx) = MeshNode::new(config.mesh.clone(), connection_manager.clone());

        let runtime = Self {
            mesh_node: Arc::new(mesh_node),
            connection_manager,
            shim: Arc::new(Mutex::new(None)),
            bridge_pending_dials: Arc::new(Mutex::new(HashMap::new())),
        };

        (runtime, event_rx)
    }

    /// Start the runtime: spawn sidecar (if configured), start mesh node.
    pub async fn start(&self, config: &RuntimeConfig) -> Result<(), RuntimeError> {
        if let Some(ref sidecar_path) = config.sidecar_path {
            self.bootstrap_sidecar(sidecar_path, config).await?;
        }
        self.mesh_node.start().await;
        Ok(())
    }

    /// Stop the runtime: stop mesh node and sidecar.
    pub async fn stop(&self) {
        let shim = {
            let mut guard = self.shim.lock().await;
            guard.take()
        };
        if let Some(shim) = shim {
            let _ = shim.stop().await;
        }
        self.mesh_node.stop().await;
    }

    /// Access the mesh node.
    pub fn mesh_node(&self) -> &Arc<MeshNode> {
        &self.mesh_node
    }

    /// Access the connection manager.
    pub fn connection_manager(&self) -> &Arc<ConnectionManager> {
        &self.connection_manager
    }

    /// Dial a peer through the full bridge pipeline.
    pub async fn dial_peer(&self, target_dns: &str, port: u16) -> Result<(), String> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        // Step 1: Insert BEFORE dial command (prevents race)
        {
            let mut dials = self.bridge_pending_dials.lock().await;
            dials.insert(request_id.clone(), tx);
        }

        // Step 2: Tell Go to dial
        {
            let shim_guard = self.shim.lock().await;
            let shim = shim_guard.as_ref().ok_or("no sidecar running")?;
            if let Err(e) = shim.dial_raw(target_dns.to_string(), port, request_id.clone()).await {
                let mut dials = self.bridge_pending_dials.lock().await;
                dials.remove(&request_id);
                return Err(format!("dial command failed: {e}"));
            }
        } // Release shim lock during network wait

        // Step 3: Await bridge connection with timeout
        let timeout = Duration::from_secs(10);
        let bridge_conn = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(_)) => {
                return Err("dial cancelled (sender dropped)".into());
            }
            Err(_) => {
                let mut dials = self.bridge_pending_dials.lock().await;
                dials.remove(&request_id);
                return Err(format!("dial timed out after {timeout:?}"));
            }
        };

        // Step 4: Upgrade to WebSocket
        self.connection_manager.handle_outgoing(bridge_conn).await;
        Ok(())
    }

    async fn bootstrap_sidecar(
        &self,
        sidecar_path: &PathBuf,
        config: &RuntimeConfig,
    ) -> Result<(), RuntimeError> {
        // Generate session token
        let mut session_token = [0u8; 32];
        getrandom::getrandom(&mut session_token)
            .map_err(|e| RuntimeError::Bootstrap(format!("token generation failed: {e}")))?;
        let session_token_hex = hex::encode(session_token);

        // Bind bridge
        let mut bridge_manager = BridgeManager::bind(session_token)
            .await
            .map_err(|e| RuntimeError::Bootstrap(format!("bridge bind failed: {e}")))?;
        let bridge_port = bridge_manager.local_port();

        // Register WS handlers
        let (ws_in_tx, mut ws_in_rx) = mpsc::channel(64);
        let (ws_out_tx, mut ws_out_rx) = mpsc::channel(64);
        bridge_manager.add_handler(443, Direction::Incoming, Arc::new(ChannelHandler::new(ws_in_tx)));
        bridge_manager.add_handler(443, Direction::Outgoing, Arc::new(ChannelHandler::new(ws_out_tx)));

        // Use the bridge manager's pending_dials Arc directly
        let bridge_pending = bridge_manager.pending_dials().clone();

        // Spawn bridge
        tokio::spawn(async move {
            bridge_manager.run(tokio_util::sync::CancellationToken::new()).await;
        });

        // Spawn WS connection handlers
        let conn_mgr = self.connection_manager.clone();
        tokio::spawn(async move {
            while let Some(bc) = ws_in_rx.recv().await {
                conn_mgr.handle_incoming(bc).await;
            }
        });
        let conn_mgr2 = self.connection_manager.clone();
        tokio::spawn(async move {
            while let Some(bc) = ws_out_rx.recv().await {
                conn_mgr2.handle_outgoing(bc).await;
            }
        });

        // Build hostname
        let hostname = format!(
            "{}-{}-{}",
            config.mesh.hostname_prefix,
            config.mesh.device_type,
            config.mesh.device_id
        );

        let state_dir = config.state_dir.clone()
            .unwrap_or_else(|| format!(".truffle-state/{}", config.mesh.device_id));
        std::fs::create_dir_all(&state_dir)
            .map_err(|e| RuntimeError::Bootstrap(format!("state dir: {e}")))?;

        // Spawn sidecar
        let shim_config = ShimConfig {
            binary_path: sidecar_path.clone(),
            hostname,
            state_dir,
            auth_key: config.auth_key.clone(),
            bridge_port,
            session_token: session_token_hex,
            auto_restart: true,
            ephemeral: None,
            tags: None,
        };

        let (shim, lifecycle_rx) = GoShim::spawn(shim_config)
            .await
            .map_err(|e| RuntimeError::Bootstrap(format!("sidecar spawn: {e}")))?;

        *self.shim.lock().await = Some(shim);

        // Spawn lifecycle handler
        self.spawn_lifecycle_handler(lifecycle_rx, bridge_pending);

        Ok(())
    }

    fn spawn_lifecycle_handler(
        &self,
        mut rx: broadcast::Receiver<ShimLifecycleEvent>,
        bridge_pending: Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>>,
    ) {
        let node = self.mesh_node.clone();
        let shim = self.shim.clone();
        let conn_mgr = self.connection_manager.clone();
        let hostname_prefix = node.config_ref().hostname_prefix.clone();

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ShimLifecycleEvent::Status(status)) => {
                        if !status.tailscale_ip.is_empty() {
                            let dns = if status.dns_name.is_empty() { None } else { Some(status.dns_name.as_str()) };
                            node.set_local_online(&status.tailscale_ip, dns).await;
                            node.set_auth_authenticated().await;
                            // Request peers
                            let sg = shim.lock().await;
                            if let Some(ref s) = *sg {
                                let _ = s.get_peers().await;
                            }
                        }
                    }
                    Ok(ShimLifecycleEvent::AuthRequired { auth_url }) => {
                        node.set_auth_required(&auth_url).await;
                    }
                    Ok(ShimLifecycleEvent::Peers(peers_data)) => {
                        let core_peers: Vec<TailnetPeer> = peers_data.peers.iter()
                            .map(|p| p.to_canonical())
                            .collect();
                        node.handle_tailnet_peers(&core_peers).await;

                        // Dial online truffle peers
                        let local_dns = node.local_device().await
                            .tailscale_dns_name.unwrap_or_default();
                        for peer in &peers_data.peers {
                            if !peer.online || peer.dns_name.is_empty() { continue; }
                            if peer.dns_name == local_dns { continue; }
                            if !peer.hostname.contains(&hostname_prefix) { continue; }

                            let dns = peer.dns_name.clone();
                            let bp = bridge_pending.clone();
                            let sh = shim.clone();
                            let cm = conn_mgr.clone();
                            tokio::spawn(async move {
                                let sg = sh.lock().await;
                                if let Some(ref s) = *sg {
                                    let rid = uuid::Uuid::new_v4().to_string();
                                    let (tx, rx) = oneshot::channel();
                                    bp.lock().await.insert(rid.clone(), tx);
                                    if s.dial_raw(dns.clone(), 443, rid.clone()).await.is_ok() {
                                        drop(sg);
                                        match tokio::time::timeout(Duration::from_secs(10), rx).await {
                                            Ok(Ok(bc)) => { cm.handle_outgoing(bc).await; }
                                            _ => { bp.lock().await.remove(&rid); }
                                        }
                                    } else {
                                        bp.lock().await.remove(&rid);
                                    }
                                }
                            });
                        }
                    }
                    Ok(ShimLifecycleEvent::DialFailed { request_id, error }) => {
                        tracing::warn!("Dial failed: {request_id}: {error}");
                        bridge_pending.lock().await.remove(&request_id);
                    }
                    Ok(ShimLifecycleEvent::Crashed { exit_code, stderr_tail }) => {
                        node.emit_event(MeshNodeEvent::Error(format!(
                            "Sidecar crashed (exit={exit_code:?}): {}",
                            stderr_tail.chars().take(200).collect::<String>()
                        )));
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Lifecycle receiver lagged by {n}");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("bootstrap failed: {0}")]
    Bootstrap(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_runtime_config() -> RuntimeConfig {
        RuntimeConfig {
            mesh: MeshNodeConfig {
                device_id: "test-dev-1".to_string(),
                device_name: "Test Device".to_string(),
                device_type: "desktop".to_string(),
                hostname_prefix: "app".to_string(),
                prefer_primary: false,
                capabilities: vec![],
                metadata: None,
                timing: crate::mesh::node::MeshTimingConfig::default(),
            },
            transport: TransportConfig::default(),
            sidecar_path: None,
            state_dir: None,
            auth_key: None,
        }
    }

    #[tokio::test]
    async fn runtime_creates_and_starts() {
        let config = test_runtime_config();
        let (runtime, mut rx) = TruffleRuntime::new(&config);
        runtime.start(&config).await.unwrap();

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, MeshNodeEvent::Started));

        runtime.stop().await;
    }

    #[tokio::test]
    async fn runtime_stop_without_start() {
        let config = test_runtime_config();
        let (runtime, _rx) = TruffleRuntime::new(&config);
        runtime.stop().await; // Should not panic
    }

    #[tokio::test]
    async fn runtime_mesh_node_accessible() {
        let config = test_runtime_config();
        let (runtime, _rx) = TruffleRuntime::new(&config);
        let device_id = runtime.mesh_node().device_id().await;
        assert_eq!(device_id, "test-dev-1");
    }
}
