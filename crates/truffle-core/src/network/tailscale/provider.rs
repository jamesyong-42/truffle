//! TailscaleProvider — the public NetworkProvider implementation.
//!
//! Orchestrates the Go sidecar (Layer 1) and bridge (Layer 2) to provide
//! peer discovery, raw TCP connectivity, and diagnostics via the Tailscale
//! network.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;

use super::bridge::{Bridge, DIAL_TIMEOUT};
use super::sidecar::{GoSidecar, SidecarConfig, SidecarInternalEvent};
use crate::network::{
    HealthInfo, IncomingConnection, NetworkError, NetworkPeer, NetworkPeerEvent,
    NetworkTcpListener, NodeIdentity, PeerAddr, PingResult,
};

/// Configuration for creating a TailscaleProvider.
#[derive(Debug, Clone)]
pub struct TailscaleConfig {
    /// Path to the Go sidecar binary.
    pub binary_path: PathBuf,
    /// Hostname for the tsnet node (e.g., "truffle-cli-{uuid}").
    pub hostname: String,
    /// State directory for tsnet persistent state.
    pub state_dir: String,
    /// Optional Tailscale auth key for headless authentication.
    pub auth_key: Option<String>,
    /// Whether the node is ephemeral (removed when offline).
    pub ephemeral: Option<bool>,
    /// ACL tags to advertise (e.g., ["tag:truffle"]).
    pub tags: Option<Vec<String>>,
}

/// State of the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderState {
    Stopped,
    Starting,
    Running,
    Stopping,
}

/// Tailscale network provider implementing [`NetworkProvider`].
///
/// Wraps the Go sidecar (tsnet) and local TCP bridge to provide:
/// - Peer discovery via WatchIPNBus events
/// - Raw TCP dial/listen over encrypted Tailscale tunnels
/// - Network-level ping and health monitoring
///
/// All bridge internals (pending_dials, session tokens, binary headers) are
/// completely hidden. Callers interact only with plain `TcpStream`s and
/// high-level types.
pub struct TailscaleProvider {
    config: TailscaleConfig,
    state: Arc<RwLock<ProviderState>>,

    /// Local node identity (populated after start).
    ///
    /// Uses `std::sync::RwLock` (not tokio) so the sync trait methods
    /// `local_identity()` and `local_addr()` can read without `.await`.
    identity: Arc<std::sync::RwLock<NodeIdentity>>,
    /// Local node address (populated after start).
    ///
    /// Uses `std::sync::RwLock` (not tokio) so the sync trait method
    /// `local_addr()` can read without `.await`.
    local_addr: Arc<std::sync::RwLock<PeerAddr>>,

    /// Cached peer list.
    peers: Arc<RwLock<HashMap<String, NetworkPeer>>>,

    /// Broadcast channel for peer events.
    peer_event_tx: broadcast::Sender<NetworkPeerEvent>,

    /// Health info cache.
    health: Arc<RwLock<HealthInfo>>,

    /// Handle to the Go sidecar (set during start).
    sidecar: Arc<Mutex<Option<GoSidecar>>>,

    /// Handle to the bridge (set during start).
    bridge: Arc<Mutex<Option<Arc<Bridge>>>>,

    /// Bridge shutdown sender.
    bridge_shutdown_tx: Arc<Mutex<Option<tokio::sync::watch::Sender<bool>>>>,

    /// Session token (32 bytes, generated on start).
    session_token: Arc<RwLock<[u8; 32]>>,
}

impl TailscaleProvider {
    /// Create a new TailscaleProvider with the given configuration.
    ///
    /// Does not start the provider — call [`start()`](NetworkProvider::start) to begin.
    pub fn new(config: TailscaleConfig) -> Self {
        let (peer_event_tx, _) = broadcast::channel(256);

        Self {
            config,
            state: Arc::new(RwLock::new(ProviderState::Stopped)),
            identity: Arc::new(std::sync::RwLock::new(NodeIdentity::default())),
            local_addr: Arc::new(std::sync::RwLock::new(PeerAddr::default())),
            peers: Arc::new(RwLock::new(HashMap::new())),
            peer_event_tx,
            health: Arc::new(RwLock::new(HealthInfo {
                state: "stopped".to_string(),
                healthy: false,
                ..Default::default()
            })),
            sidecar: Arc::new(Mutex::new(None)),
            bridge: Arc::new(Mutex::new(None)),
            bridge_shutdown_tx: Arc::new(Mutex::new(None)),
            session_token: Arc::new(RwLock::new([0u8; 32])),
        }
    }

    /// Generate a random 32-byte session token.
    fn generate_session_token() -> Result<[u8; 32], NetworkError> {
        let mut token = [0u8; 32];
        getrandom::getrandom(&mut token)
            .map_err(|e| NetworkError::Internal(format!("failed to generate session token: {e}")))?;
        Ok(token)
    }

    /// Convert a SidecarPeer to a NetworkPeer.
    fn sidecar_peer_to_network_peer(
        peer: &super::protocol::SidecarPeer,
    ) -> NetworkPeer {
        let ip = peer
            .tailscale_ips
            .first()
            .and_then(|s| s.parse::<IpAddr>().ok())
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));

        NetworkPeer {
            id: peer.id.clone(),
            hostname: peer.hostname.clone(),
            ip,
            online: peer.online,
            cur_addr: if peer.cur_addr.is_empty() {
                None
            } else {
                Some(peer.cur_addr.clone())
            },
            relay: if peer.relay.is_empty() {
                None
            } else {
                Some(peer.relay.clone())
            },
            os: if peer.os.is_empty() {
                None
            } else {
                Some(peer.os.clone())
            },
            last_seen: peer.last_seen.clone(),
            key_expiry: peer.key_expiry.clone(),
            dns_name: Some(peer.dns_name.clone()),
        }
    }

    /// Spawn the background event processing loop that maps sidecar events
    /// to peer events and updates cached state.
    fn spawn_event_processor(
        mut sidecar_rx: broadcast::Receiver<SidecarInternalEvent>,
        peers: Arc<RwLock<HashMap<String, NetworkPeer>>>,
        peer_event_tx: broadcast::Sender<NetworkPeerEvent>,
        health: Arc<RwLock<HealthInfo>>,
        identity: Arc<std::sync::RwLock<NodeIdentity>>,
        local_addr: Arc<std::sync::RwLock<PeerAddr>>,
        state: Arc<RwLock<ProviderState>>,
        started_tx: Option<oneshot::Sender<Result<(), NetworkError>>>,
    ) {
        tokio::spawn(async move {
            let mut started_tx = started_tx;

            loop {
                match sidecar_rx.recv().await {
                    Ok(event) => {
                        match event {
                            SidecarInternalEvent::Started {
                                hostname,
                                dns_name,
                                tailscale_ip,
                                node_id,
                            } => {
                                let ip: Option<IpAddr> = tailscale_ip.parse().ok();

                                {
                                    let mut id = identity.write().unwrap();
                                    id.hostname = hostname.clone();
                                    id.dns_name = Some(dns_name.clone());
                                    id.name = hostname.clone();
                                    id.ip = ip;
                                    if !node_id.is_empty() {
                                        id.id = node_id;
                                    }
                                }

                                {
                                    let mut addr = local_addr.write().unwrap();
                                    addr.hostname = hostname;
                                    addr.dns_name = Some(dns_name);
                                    addr.ip = ip;
                                }

                                {
                                    let mut h = health.write().await;
                                    h.state = "running".to_string();
                                    h.healthy = true;
                                }

                                *state.write().await = ProviderState::Running;

                                // Signal start() that we're ready
                                if let Some(tx) = started_tx.take() {
                                    let _ = tx.send(Ok(()));
                                }
                            }
                            SidecarInternalEvent::AuthRequired { auth_url } => {
                                tracing::info!("tailscale auth required: {auth_url}");
                                // Emit auth URL via peer events so callers can display it.
                                // Do NOT consume started_tx — keep waiting for Running state.
                                let _ = peer_event_tx.send(NetworkPeerEvent::AuthRequired {
                                    url: auth_url,
                                });
                            }
                            SidecarInternalEvent::Stopped => {
                                *state.write().await = ProviderState::Stopped;
                                let mut h = health.write().await;
                                h.state = "stopped".to_string();
                                h.healthy = false;
                                tracing::info!("tailscale provider stopped");
                                return;
                            }
                            SidecarInternalEvent::StateChange { state: new_state } => {
                                let mut h = health.write().await;
                                h.state = new_state;
                            }
                            SidecarInternalEvent::KeyExpiring { expires_at } => {
                                let mut h = health.write().await;
                                h.key_expiry = Some(expires_at);
                            }
                            SidecarInternalEvent::HealthWarning { warnings } => {
                                let mut h = health.write().await;
                                h.warnings = warnings;
                                h.healthy = h.warnings.is_empty();
                            }
                            SidecarInternalEvent::PeersReceived(sidecar_peers) => {
                                let mut peer_map = peers.write().await;
                                // Filter to truffle peers only
                                let new_peers: HashMap<String, NetworkPeer> = sidecar_peers
                                    .iter()
                                    .filter(|p| is_truffle_peer(&p.hostname))
                                    .map(|p| {
                                        let np = Self::sidecar_peer_to_network_peer(p);
                                        (np.id.clone(), np)
                                    })
                                    .collect();

                                // Detect joins, leaves, and updates
                                for (id, new_peer) in &new_peers {
                                    if let Some(_existing) = peer_map.get(id) {
                                        let _ = peer_event_tx
                                            .send(NetworkPeerEvent::Updated(new_peer.clone()));
                                    } else {
                                        let _ = peer_event_tx
                                            .send(NetworkPeerEvent::Joined(new_peer.clone()));
                                    }
                                }
                                for id in peer_map.keys() {
                                    if !new_peers.contains_key(id) {
                                        let _ = peer_event_tx
                                            .send(NetworkPeerEvent::Left(id.clone()));
                                    }
                                }

                                *peer_map = new_peers;
                            }
                            SidecarInternalEvent::PeerChanged(change) => {
                                let mut peer_map = peers.write().await;
                                match change.change_type.as_str() {
                                    "joined" => {
                                        if let Some(p) = change.peer {
                                            if is_truffle_peer(&p.hostname) {
                                                let np = Self::sidecar_peer_to_network_peer(&p);
                                                peer_map.insert(np.id.clone(), np.clone());
                                                let _ = peer_event_tx
                                                    .send(NetworkPeerEvent::Joined(np));
                                            }
                                        }
                                    }
                                    "left" => {
                                        if peer_map.remove(&change.peer_id).is_some() {
                                            let _ = peer_event_tx
                                                .send(NetworkPeerEvent::Left(change.peer_id));
                                        }
                                    }
                                    "updated" => {
                                        if let Some(p) = change.peer {
                                            if is_truffle_peer(&p.hostname) {
                                                let np = Self::sidecar_peer_to_network_peer(&p);
                                                peer_map.insert(np.id.clone(), np.clone());
                                                let _ = peer_event_tx
                                                    .send(NetworkPeerEvent::Updated(np));
                                            }
                                        }
                                    }
                                    other => {
                                        tracing::warn!("unknown peer change type: {other}");
                                    }
                                }
                            }
                            SidecarInternalEvent::Error { code, message } => {
                                tracing::error!("sidecar error [{code}]: {message}");
                                // If start() is still waiting and this is a fatal error
                                if let Some(tx) = started_tx.take() {
                                    let _ = tx.send(Err(NetworkError::SidecarError(
                                        format!("[{code}] {message}"),
                                    )));
                                }
                            }
                            SidecarInternalEvent::ProcessExited { exit_code } => {
                                tracing::error!("sidecar process exited: {exit_code:?}");
                                *state.write().await = ProviderState::Stopped;
                                let mut h = health.write().await;
                                h.state = "crashed".to_string();
                                h.healthy = false;
                                if let Some(tx) = started_tx.take() {
                                    let _ = tx.send(Err(NetworkError::SidecarError(
                                        format!("process exited with code {exit_code:?}"),
                                    )));
                                }
                                return;
                            }
                            // Dial/Listen/Ping results are handled by the caller,
                            // not the background event processor
                            _ => {}
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("event processor lagged by {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!("sidecar event channel closed, stopping event processor");
                        return;
                    }
                }
            }
        });
    }
}

/// Check if a hostname belongs to a truffle node.
/// Truffle nodes use hostnames like "truffle-cli-{uuid}" or "truffle-{name}".
pub(crate) fn is_truffle_peer(hostname: &str) -> bool {
    hostname.starts_with("truffle-")
}

impl super::super::NetworkProvider for TailscaleProvider {
    async fn start(&mut self) -> Result<(), NetworkError> {
        {
            let current_state = *self.state.read().await;
            if current_state != ProviderState::Stopped {
                return Err(NetworkError::AlreadyRunning);
            }
        }
        *self.state.write().await = ProviderState::Starting;

        // Generate session token
        let token = Self::generate_session_token()?;
        let token_hex = hex::encode(token);
        *self.session_token.write().await = token;

        // Start the bridge
        let bridge = Bridge::bind(token).await?;
        let bridge_port = bridge.local_port()?;
        let bridge = Arc::new(bridge);

        // Create bridge shutdown channel
        let (bridge_shutdown_tx, bridge_shutdown_rx) = tokio::sync::watch::channel(false);

        // Run bridge accept loop
        {
            let bridge_clone = bridge.clone();
            tokio::spawn(async move {
                bridge_clone.run(bridge_shutdown_rx).await;
            });
        }

        *self.bridge.lock().await = Some(bridge.clone());
        *self.bridge_shutdown_tx.lock().await = Some(bridge_shutdown_tx);

        // Build sidecar config
        let sidecar_config = SidecarConfig {
            binary_path: self.config.binary_path.clone(),
            hostname: self.config.hostname.clone(),
            state_dir: self.config.state_dir.clone(),
            auth_key: self.config.auth_key.clone(),
            bridge_port,
            session_token_hex: token_hex,
            ephemeral: self.config.ephemeral,
            tags: self.config.tags.clone(),
        };

        // Spawn the sidecar
        let (sidecar, sidecar_rx) = GoSidecar::spawn(sidecar_config.clone()).await?;

        // Create a channel for the event processor to signal when we're running
        let (started_tx, started_rx) = oneshot::channel();

        // Start event processor
        Self::spawn_event_processor(
            sidecar_rx,
            self.peers.clone(),
            self.peer_event_tx.clone(),
            self.health.clone(),
            self.identity.clone(),
            self.local_addr.clone(),
            self.state.clone(),
            Some(started_tx),
        );

        // Send start command to sidecar
        sidecar.send_start(&sidecar_config).await?;

        *self.sidecar.lock().await = Some(sidecar);

        // Wait for the sidecar to reach "running" state.
        // Use a generous timeout (5 min) because browser auth may take a while.
        // Auth URLs are emitted via peer_events() so the caller can display them.
        let auth_timeout = Duration::from_secs(300);
        let result = tokio::time::timeout(auth_timeout, started_rx)
            .await
            .map_err(|_| NetworkError::StartFailed(
                "timed out waiting for authentication (5 min). \
                 Subscribe to peer_events() to display auth URLs.".into()
            ))?
            .map_err(|_| NetworkError::StartFailed("start signal channel dropped".into()))?;

        match result {
            Ok(()) => {
                // Fetch initial peer list
                if let Some(ref sidecar) = *self.sidecar.lock().await {
                    let _ = sidecar.send_get_peers().await;
                    // Also start WatchIPNBus for real-time peer events
                    let _ = sidecar.send_watch_peers().await;
                }
                tracing::info!("tailscale provider started successfully");
                Ok(())
            }
            Err(e) => {
                *self.state.write().await = ProviderState::Stopped;
                Err(e)
            }
        }
    }

    async fn stop(&mut self) -> Result<(), NetworkError> {
        *self.state.write().await = ProviderState::Stopping;

        // Shut down sidecar
        if let Some(sidecar) = self.sidecar.lock().await.take() {
            sidecar.shutdown().await;
        }

        // Shut down bridge
        if let Some(tx) = self.bridge_shutdown_tx.lock().await.take() {
            let _ = tx.send(true);
        }
        *self.bridge.lock().await = None;

        // Clear state
        self.peers.write().await.clear();
        *self.state.write().await = ProviderState::Stopped;
        let mut h = self.health.write().await;
        h.state = "stopped".to_string();
        h.healthy = false;

        tracing::info!("tailscale provider stopped");
        Ok(())
    }

    fn local_identity(&self) -> NodeIdentity {
        self.identity.read().unwrap().clone()
    }

    fn local_addr(&self) -> PeerAddr {
        self.local_addr.read().unwrap().clone()
    }

    fn peer_events(&self) -> broadcast::Receiver<NetworkPeerEvent> {
        self.peer_event_tx.subscribe()
    }

    async fn peers(&self) -> Vec<NetworkPeer> {
        self.peers.read().await.values().cloned().collect()
    }

    async fn dial_tcp(&self, addr: &str, port: u16) -> Result<TcpStream, NetworkError> {
        if *self.state.read().await != ProviderState::Running {
            return Err(NetworkError::NotRunning);
        }

        let bridge = self
            .bridge
            .lock()
            .await
            .clone()
            .ok_or(NetworkError::NotRunning)?;

        // Generate a unique request ID
        let request_id = uuid::Uuid::new_v4().to_string();

        // Register the pending dial before sending the command
        let dial_rx = bridge.register_dial(request_id.clone()).await;

        // Scope the sidecar lock: subscribe + send, then release
        let mut event_rx = {
            let sidecar_guard = self.sidecar.lock().await;
            let sidecar = sidecar_guard
                .as_ref()
                .ok_or(NetworkError::NotRunning)?;

            let event_rx = sidecar.subscribe();

            sidecar
                .send_dial(request_id.clone(), addr.to_string(), port)
                .await?;

            event_rx
        };

        // Wait for either:
        // 1. Bridge delivers the TcpStream (success path)
        // 2. Sidecar reports dial failure via event (error path)
        // 3. Timeout
        //
        // The bridge delivers the TcpStream once the Go sidecar bridges the
        // connection back. If the sidecar reports a failure, we get that via
        // the event channel and abort early.
        let result = tokio::time::timeout(DIAL_TIMEOUT, async {
            // Spawn a task to watch for dial failure events.
            // We keep the JoinHandle so we can abort it once the select resolves,
            // preventing an orphaned task that would loop forever.
            let fail_request_id = request_id.clone();
            let (fail_tx, fail_rx) = oneshot::channel::<String>();
            let fail_watcher: JoinHandle<()> = tokio::spawn(async move {
                loop {
                    match event_rx.recv().await {
                        Ok(SidecarInternalEvent::DialFailed { request_id: rid, error })
                            if rid == fail_request_id =>
                        {
                            let _ = fail_tx.send(error);
                            return;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            let _ = fail_tx.send("event channel closed".to_string());
                            return;
                        }
                        _ => continue,
                    }
                }
            });

            let result = tokio::select! {
                stream_result = dial_rx => {
                    stream_result.map_err(|_| NetworkError::DialFailed("dial cancelled".into()))
                }
                fail_result = fail_rx => {
                    let error = fail_result.unwrap_or_else(|_| "dial watcher dropped".to_string());
                    Err(NetworkError::DialFailed(error))
                }
            };

            // Cancel the fail-watcher task so it doesn't leak
            fail_watcher.abort();

            result
        })
        .await
        .map_err(|_| NetworkError::DialTimeout(DIAL_TIMEOUT))?;

        // Clean up pending dial on any error
        if result.is_err() {
            bridge.remove_dial(&request_id).await;
        }

        result
    }

    async fn listen_tcp(
        &self,
        port: u16,
    ) -> Result<NetworkTcpListener, NetworkError> {
        if *self.state.read().await != ProviderState::Running {
            return Err(NetworkError::NotRunning);
        }

        let bridge = self
            .bridge
            .lock()
            .await
            .clone()
            .ok_or(NetworkError::NotRunning)?;

        // Create channel for incoming connections
        let (tx, rx) = mpsc::channel::<IncomingConnection>(64);

        // Scope the sidecar lock: subscribe + send listen, then release
        let mut event_rx = {
            let sidecar_guard = self.sidecar.lock().await;
            let sidecar = sidecar_guard
                .as_ref()
                .ok_or(NetworkError::NotRunning)?;

            let event_rx = sidecar.subscribe();
            sidecar.send_listen(port, None).await?;
            event_rx
        };

        // Wait for confirmation or error.
        // When port is 0, the sidecar assigns an ephemeral port and reports
        // the actual port in the Listening event.
        let actual_port = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match event_rx.recv().await {
                    Ok(SidecarInternalEvent::Listening { port: p })
                        if port == 0 || p == port =>
                    {
                        return Ok(p);
                    }
                    Ok(SidecarInternalEvent::Error { code, message }) => {
                        return Err(NetworkError::ListenFailed(format!("[{code}] {message}")));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(NetworkError::SidecarError("event channel closed".into()));
                    }
                    _ => continue,
                }
            }
        })
        .await
        .map_err(|_| NetworkError::ListenFailed("listen confirmation timed out".into()))??;

        // Register the channel with the bridge using the actual port
        bridge.register_listener(actual_port, tx).await;

        Ok(NetworkTcpListener { port: actual_port, incoming: rx })
    }

    async fn unlisten_tcp(&self, port: u16) -> Result<(), NetworkError> {
        if *self.state.read().await != ProviderState::Running {
            return Err(NetworkError::NotRunning);
        }

        let bridge = self
            .bridge
            .lock()
            .await
            .clone()
            .ok_or(NetworkError::NotRunning)?;

        // Remove bridge listener
        bridge.remove_listener(port).await;

        // Tell sidecar to stop listening
        {
            let sidecar_guard = self.sidecar.lock().await;
            let sidecar = sidecar_guard
                .as_ref()
                .ok_or(NetworkError::NotRunning)?;
            sidecar.send_unlisten(port).await?;
        }

        Ok(())
    }

    async fn ping(&self, addr: &str) -> Result<PingResult, NetworkError> {
        if *self.state.read().await != ProviderState::Running {
            return Err(NetworkError::NotRunning);
        }

        let target = addr.to_string();

        // Scope the sidecar lock: subscribe + send ping, then release
        let mut event_rx = {
            let sidecar_guard = self.sidecar.lock().await;
            let sidecar = sidecar_guard
                .as_ref()
                .ok_or(NetworkError::NotRunning)?;

            let event_rx = sidecar.subscribe();
            sidecar.send_ping(target.clone(), None).await?;
            event_rx
        };

        // Wait for result
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match event_rx.recv().await {
                    Ok(SidecarInternalEvent::PingResult(data)) if data.target == target => {
                        if !data.error.is_empty() {
                            return Err(NetworkError::PingFailed(data.error));
                        }
                        let connection = if data.direct {
                            "direct".to_string()
                        } else if !data.relay.is_empty() {
                            format!("relay:{}", data.relay)
                        } else {
                            "unknown".to_string()
                        };
                        return Ok(PingResult {
                            latency: Duration::from_secs_f64(data.latency_ms / 1000.0),
                            connection,
                            peer_addr: if data.peer_addr.is_empty() {
                                None
                            } else {
                                Some(data.peer_addr)
                            },
                        });
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(NetworkError::SidecarError("event channel closed".into()));
                    }
                    _ => continue,
                }
            }
        })
        .await
        .map_err(|_| NetworkError::PingFailed("ping timed out".into()))?;

        result
    }

    async fn bind_udp(&self, port: u16) -> Result<super::super::NetworkUdpSocket, NetworkError> {
        if *self.state.read().await != ProviderState::Running {
            return Err(NetworkError::NotRunning);
        }

        // Scope the sidecar lock: subscribe + send listenPacket, then release
        let mut event_rx = {
            let sidecar_guard = self.sidecar.lock().await;
            let sidecar = sidecar_guard
                .as_ref()
                .ok_or(NetworkError::NotRunning)?;

            let event_rx = sidecar.subscribe();
            sidecar.send_listen_packet(port).await?;
            event_rx
        };

        // Wait for the sidecar to report the local relay port
        let local_port = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match event_rx.recv().await {
                    Ok(SidecarInternalEvent::ListeningPacket {
                        port: p,
                        local_port,
                    }) if p == port => {
                        return Ok(local_port);
                    }
                    Ok(SidecarInternalEvent::Error { code, message }) => {
                        return Err(NetworkError::ListenFailed(format!(
                            "UDP bind failed [{code}] {message}"
                        )));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(NetworkError::SidecarError(
                            "event channel closed".into(),
                        ));
                    }
                    _ => continue,
                }
            }
        })
        .await
        .map_err(|_| {
            NetworkError::ListenFailed("UDP listenPacket confirmation timed out".into())
        })??;

        // Bind a local UDP socket and connect it to the relay
        let local_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .map_err(|e| {
                NetworkError::Internal(format!("failed to bind local UDP socket: {e}"))
            })?;

        local_socket
            .connect(format!("127.0.0.1:{local_port}"))
            .await
            .map_err(|e| {
                NetworkError::Internal(format!(
                    "failed to connect local UDP socket to relay: {e}"
                ))
            })?;

        let rust_local_addr = local_socket.local_addr().map_err(|e| {
            NetworkError::Internal(format!("failed to get local UDP addr: {e}"))
        })?;

        // Send a registration packet so the relay learns our address.
        // Without this, the relay drops inbound packets because it doesn't
        // know where to forward them (it learns the Rust peer address from
        // the first outbound packet).
        local_socket
            .send(b"TRUFFLE_UDP_REGISTER")
            .await
            .map_err(|e| {
                NetworkError::Internal(format!("failed to send UDP registration: {e}"))
            })?;

        tracing::info!(
            tsnet_port = port,
            relay_port = local_port,
            rust_local_addr = %rust_local_addr,
            "UDP socket bound via tsnet relay (registered)"
        );

        Ok(super::super::NetworkUdpSocket::new(local_socket, port))
    }

    async fn health(&self) -> HealthInfo {
        self.health.read().await.clone()
    }
}

impl TailscaleProvider {
    /// Get the local identity (convenience alias — same as the trait method).
    ///
    /// Retained for backwards compatibility with existing callers that used
    /// the old async version.
    pub async fn local_identity_async(&self) -> NodeIdentity {
        self.identity.read().unwrap().clone()
    }

    /// Get the local address (convenience alias — same as the trait method).
    ///
    /// Retained for backwards compatibility with existing callers that used
    /// the old async version.
    pub async fn local_addr_async(&self) -> PeerAddr {
        self.local_addr.read().unwrap().clone()
    }
}
