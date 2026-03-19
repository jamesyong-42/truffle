use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::WebSocketStream;

use crate::bridge::header::Direction;
use crate::bridge::manager::BridgeConnection;

use crate::protocol::frame::ControlMessage;

use super::heartbeat::{self, HeartbeatConfig, HeartbeatTimeout};
use super::reconnect::ReconnectTarget;
use super::websocket::{self, DecodedFrame, WsError, WireMessage};

/// Protocol version 2: legacy format (pre-RFC 009). No frame type byte.
pub const PROTOCOL_V2: u8 = 2;

/// Protocol version 3: RFC 009 format. 2-byte header, typed frames.
pub const PROTOCOL_V3: u8 = 3;

/// Connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
}

/// A WebSocket connection record.
#[derive(Debug, Clone)]
pub struct WSConnection {
    pub id: String,
    pub device_id: Option<String>,
    pub direction: Direction,
    pub remote_addr: String,
    pub remote_dns_name: String,
    pub status: ConnectionStatus,
    /// Negotiated protocol version: 2 = legacy (v2), 3 = RFC 009 (v3).
    /// Starts at v2 (legacy) and upgrades to v3 after a successful handshake.
    pub protocol_version: u8,
}

/// Events emitted by the ConnectionManager.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// New connection established.
    Connected(WSConnection),
    /// Connection lost.
    Disconnected {
        connection_id: String,
        reason: String,
    },
    /// Reconnecting to a peer.
    Reconnecting {
        device_id: String,
        attempt: u32,
        delay: Duration,
    },
    /// Device identity established for a connection.
    DeviceIdentified {
        connection_id: String,
        device_id: String,
    },
    /// Application-level message received.
    Message {
        connection_id: String,
        message: WireMessage,
    },
}

/// Configuration for the transport layer.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub heartbeat: HeartbeatConfig,
    pub auto_reconnect: bool,
    pub max_reconnect_delay: Duration,
    /// Whether to use JSON mode for outgoing messages (debug).
    pub debug_json_mode: bool,
    /// Local device ID, used for v3 handshake messages.
    /// If `None`, handshake will not be sent (v2-only mode).
    pub local_device_id: Option<String>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            heartbeat: HeartbeatConfig::default(),
            auto_reconnect: true,
            max_reconnect_delay: super::reconnect::MAX_RECONNECT_DELAY,
            debug_json_mode: false,
            local_device_id: None,
        }
    }
}

/// Internal state for an active connection.
#[allow(dead_code)]
struct ActiveConnection {
    info: WSConnection,
    /// Send encoded WS binary frames to the write pump.
    write_tx: mpsc::Sender<Vec<u8>>,
    /// Signal activity to the heartbeat tracker.
    activity_tx: mpsc::Sender<()>,
    /// Negotiated protocol version, shared with message loop tasks.
    /// Starts at PROTOCOL_V2, upgraded to PROTOCOL_V3 after handshake.
    protocol_version: Arc<AtomicU8>,
}

/// Manages WebSocket connections over bridged TCP streams.
///
/// Handles incoming connections from the BridgeManager and outgoing
/// connections via the GoShim dial mechanism.
pub struct ConnectionManager {
    config: TransportConfig,
    connections: Arc<RwLock<HashMap<String, ActiveConnection>>>,
    device_to_connection: Arc<RwLock<HashMap<String, String>>>,
    #[allow(dead_code)]
    reconnect_targets: Arc<Mutex<HashMap<String, ReconnectTarget>>>,
    event_tx: broadcast::Sender<TransportEvent>,
}

impl ConnectionManager {
    pub fn new(config: TransportConfig) -> (Self, broadcast::Receiver<TransportEvent>) {
        let (event_tx, event_rx) = broadcast::channel(256);
        let manager = Self {
            config,
            connections: Arc::new(RwLock::new(HashMap::new())),
            device_to_connection: Arc::new(RwLock::new(HashMap::new())),
            reconnect_targets: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
        };
        (manager, event_rx)
    }

    /// Subscribe to transport events.
    pub fn subscribe(&self) -> broadcast::Receiver<TransportEvent> {
        self.event_tx.subscribe()
    }

    /// Handle an incoming bridge connection on port 443.
    ///
    /// Performs the WebSocket upgrade handshake (checking for GET /ws),
    /// then starts read/write pumps and heartbeat.
    pub async fn handle_incoming(&self, conn: BridgeConnection) {
        let connection_id = format!("incoming:{}", conn.header.remote_addr);
        let remote_addr = conn.header.remote_addr.clone();
        let remote_dns = conn.header.remote_dns_name.clone();

        tracing::info!("incoming WS connection from {remote_addr} ({remote_dns})");

        // WebSocket upgrade — accept only GET /ws with Upgrade: websocket
        let ws_config = websocket::ws_config();
        let ws_stream = match tokio_tungstenite::accept_hdr_async_with_config(
            conn.stream,
            |req: &Request, resp: Response| {
                if req.uri().path() != "/ws" {
                    let resp = Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(None)
                        .unwrap();
                    return Err(resp);
                }
                Ok(resp)
            },
            Some(ws_config),
        )
        .await
        {
            Ok(ws) => ws,
            Err(e) => {
                tracing::warn!("WS upgrade failed for {remote_addr}: {e}");
                return;
            }
        };

        let ws_conn = WSConnection {
            id: connection_id.clone(),
            device_id: None,
            direction: Direction::Incoming,
            remote_addr,
            remote_dns_name: remote_dns,
            status: ConnectionStatus::Connected,
            protocol_version: PROTOCOL_V2,
        };

        self.setup_connection(connection_id, ws_stream, ws_conn)
            .await;
    }

    /// Handle an outgoing bridge connection (from a completed dial).
    ///
    /// The TcpStream is already connected — perform WS client handshake.
    pub async fn handle_outgoing(&self, conn: BridgeConnection) {
        let connection_id = format!("dial:{}", conn.header.request_id);
        let remote_addr = conn.header.remote_addr.clone();
        let remote_dns = conn.header.remote_dns_name.clone();

        tracing::info!("outgoing WS connection to {remote_addr} ({remote_dns})");

        // Client-side WebSocket handshake
        let ws_config = websocket::ws_config();
        // Use ws:// since TLS is already terminated by Go
        let uri = format!("ws://{}/ws", remote_addr);
        let (ws_stream, _response) = match tokio_tungstenite::client_async_with_config(
            uri,
            conn.stream,
            Some(ws_config),
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!("WS client handshake failed for {remote_addr}: {e}");
                return;
            }
        };

        let ws_conn = WSConnection {
            id: connection_id.clone(),
            device_id: None,
            direction: Direction::Outgoing,
            remote_addr,
            remote_dns_name: remote_dns,
            status: ConnectionStatus::Connected,
            protocol_version: PROTOCOL_V2,
        };

        self.setup_connection(connection_id, ws_stream, ws_conn)
            .await;
    }

    /// Handle an already-upgraded WebSocket stream (e.g., from the HTTP router).
    ///
    /// Takes a `WebSocketStream` over any `S: AsyncRead + AsyncWrite + Unpin + Send`,
    /// so it works with `TokioIo<hyper::upgrade::Upgraded>` as well as `TcpStream`.
    pub async fn handle_ws_stream<S>(
        &self,
        ws_stream: WebSocketStream<S>,
        remote_addr: String,
        remote_dns_name: String,
        direction: Direction,
    ) where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let connection_id = format!(
            "{}:{}",
            match direction {
                Direction::Incoming => "incoming",
                Direction::Outgoing => "dial",
            },
            &remote_addr
        );

        tracing::info!(
            "{} WS connection (upgraded) from {remote_addr} ({remote_dns_name})",
            match direction {
                Direction::Incoming => "incoming",
                Direction::Outgoing => "outgoing",
            }
        );

        let ws_conn = WSConnection {
            id: connection_id.clone(),
            device_id: None,
            direction,
            remote_addr,
            remote_dns_name,
            status: ConnectionStatus::Connected,
            protocol_version: PROTOCOL_V2,
        };

        self.setup_connection(connection_id, ws_stream, ws_conn)
            .await;
    }

    /// Set up read/write pumps and heartbeat for a connected WebSocket.
    ///
    /// Generic over the underlying stream type `S` so this works with both
    /// `TcpStream` (direct bridge) and `TokioIo<Upgraded>` (HTTP router).
    async fn setup_connection<S>(
        &self,
        connection_id: String,
        ws_stream: WebSocketStream<S>,
        ws_conn: WSConnection,
    ) where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (write_half, read_half) = ws_stream.split();

        // Channels
        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(256);
        let (msg_tx, mut msg_rx) = mpsc::channel::<DecodedFrame>(256);
        let (activity_tx, activity_rx) = mpsc::channel::<()>(64);

        // Shared protocol version — starts at v2, upgraded to v3 after handshake.
        let protocol_version = Arc::new(AtomicU8::new(PROTOCOL_V2));

        let active_conn = ActiveConnection {
            info: ws_conn.clone(),
            write_tx: write_tx.clone(),
            activity_tx: activity_tx.clone(),
            protocol_version: protocol_version.clone(),
        };

        // Store connection
        {
            let mut conns = self.connections.write().await;
            conns.insert(connection_id.clone(), active_conn);
        }

        // Emit connected event
        let _ = self.event_tx.send(TransportEvent::Connected(ws_conn.clone()));

        // Spawn write pump
        let conn_id_w = connection_id.clone();
        tokio::spawn(async move {
            if let Err(e) = websocket::write_pump(write_half, write_rx).await {
                tracing::debug!("write pump ended for {conn_id_w}: {e}");
            }
        });

        // --- RFC 009 Phase 4: Send v3 handshake (fire-and-forget) ---
        // If we have a local_device_id, send a handshake control frame.
        // Don't wait for the ack — start normal operation immediately.
        // The message loop will upgrade protocol_version when the ack arrives.
        if let Some(ref local_device_id) = self.config.local_device_id {
            let handshake = ControlMessage::Handshake {
                protocol_version: PROTOCOL_V3 as u32,
                device_id: local_device_id.clone(),
                capabilities: vec![],
            };
            let use_json = self.config.debug_json_mode;
            match websocket::encode_control_frame(&handshake, use_json) {
                Ok(encoded) => {
                    let conns = self.connections.read().await;
                    if let Some(ac) = conns.get(&connection_id) {
                        let _ = ac.write_tx.send(encoded).await;
                        tracing::debug!(
                            connection_id = %connection_id,
                            "sent v3 handshake to peer"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        connection_id = %connection_id,
                        "failed to encode v3 handshake: {e}"
                    );
                }
            }
        }

        // Spawn the connection lifecycle task
        let conn_id_r = connection_id.clone();
        let event_tx = self.event_tx.clone();
        let connections_for_pong = self.connections.clone();
        let connections_for_cleanup = self.connections.clone();
        let device_to_conn = self.device_to_connection.clone();
        let config = self.config.clone();
        let proto_ver = protocol_version;

        tokio::spawn(async move {
            // Read pump — forwards messages
            let read_handle = tokio::spawn(async move {
                websocket::read_pump(read_half, msg_tx).await
            });

            // Heartbeat
            let heartbeat_handle = tokio::spawn(async move {
                heartbeat::heartbeat_loop(config.heartbeat, write_tx, activity_rx).await
            });
            let heartbeat_abort = heartbeat_handle.abort_handle();

            // Message forwarding loop — handles both v2 and v3 frames
            let conn_id_msg = conn_id_r.clone();
            let event_tx_msg = event_tx.clone();
            let local_device_id = config.local_device_id.clone();
            let msg_forward = tokio::spawn(async move {
                while let Some(frame) = msg_rx.recv().await {
                    // Record activity for heartbeat on every received frame
                    let _ = activity_tx.send(()).await;

                    match frame {
                        // --- v3 control frames (RFC 009 Phase 2 + Phase 4) ---
                        DecodedFrame::V3Control(ControlMessage::Ping { timestamp }) => {
                            // Respond with a v3 pong, echoing the sender's encoding
                            let pong = heartbeat::create_v3_pong(timestamp);
                            let conns = connections_for_pong.read().await;
                            if let Some(ac) = conns.get(&conn_id_msg) {
                                if let Ok(encoded) =
                                    websocket::encode_control_frame(&pong, false)
                                {
                                    let _ = ac.write_tx.send(encoded).await;
                                }
                            }
                        }
                        DecodedFrame::V3Control(ControlMessage::Pong { .. }) => {
                            // Activity already recorded above; nothing else to do.
                        }

                        // --- RFC 009 Phase 4: Handshake handling ---
                        DecodedFrame::V3Control(ControlMessage::Handshake {
                            protocol_version: peer_version,
                            device_id: peer_device_id,
                            ..
                        }) => {
                            // Peer is initiating a v3 handshake. Respond with ack.
                            let negotiated = std::cmp::min(PROTOCOL_V3 as u32, peer_version);
                            tracing::info!(
                                connection_id = %conn_id_msg,
                                peer_device_id = %peer_device_id,
                                peer_version = peer_version,
                                negotiated_version = negotiated,
                                "received v3 handshake from peer"
                            );

                            // Upgrade our protocol version
                            if negotiated >= PROTOCOL_V3 as u32 {
                                proto_ver.store(PROTOCOL_V3, Ordering::Release);
                            }

                            // Send HandshakeAck
                            let ack_device_id = local_device_id
                                .as_deref()
                                .unwrap_or("unknown")
                                .to_string();
                            let ack = ControlMessage::HandshakeAck {
                                protocol_version: PROTOCOL_V3 as u32,
                                device_id: ack_device_id,
                                negotiated_version: negotiated,
                            };
                            let conns = connections_for_pong.read().await;
                            if let Some(ac) = conns.get(&conn_id_msg) {
                                if let Ok(encoded) =
                                    websocket::encode_control_frame(&ack, false)
                                {
                                    let _ = ac.write_tx.send(encoded).await;
                                }
                            }
                        }
                        DecodedFrame::V3Control(ControlMessage::HandshakeAck {
                            negotiated_version,
                            device_id: peer_device_id,
                            ..
                        }) => {
                            tracing::info!(
                                connection_id = %conn_id_msg,
                                peer_device_id = %peer_device_id,
                                negotiated_version = negotiated_version,
                                "received v3 handshake ack — upgrading connection"
                            );

                            // Upgrade protocol version to negotiated
                            if negotiated_version >= PROTOCOL_V3 as u32 {
                                proto_ver.store(PROTOCOL_V3, Ordering::Release);
                            }
                        }

                        // --- v3 data frames ---
                        DecodedFrame::V3Data { payload, was_json } => {
                            let msg = WireMessage { payload, was_json };
                            let _ = event_tx_msg.send(TransportEvent::Message {
                                connection_id: conn_id_msg.clone(),
                                message: msg,
                            });
                        }

                        // --- v3 error frames ---
                        DecodedFrame::V3Error(error) => {
                            tracing::warn!(
                                connection_id = %conn_id_msg,
                                code = %error.code,
                                fatal = error.fatal,
                                "received protocol error from peer: {}",
                                error.message
                            );
                            // If fatal, the peer will close. We just log it.
                        }

                    }
                }
            });

            // Wait for read pump or heartbeat to finish
            let disconnect_reason = tokio::select! {
                exit = read_handle => {
                    msg_forward.abort();
                    heartbeat_abort.abort();
                    match exit {
                        Ok(websocket::ReadPumpExit::ClosedByRemote(_)) => "remote_closed".to_string(),
                        Ok(websocket::ReadPumpExit::ClosedByLocal) => "local_closed".to_string(),
                        Ok(websocket::ReadPumpExit::Error(e)) => format!("error: {e}"),
                        Err(e) => format!("read task panicked: {e}"),
                    }
                }
                exit = heartbeat_handle => {
                    msg_forward.abort();
                    match exit {
                        Ok(Err(HeartbeatTimeout)) => "heartbeat_timeout".to_string(),
                        _ => "heartbeat_ended".to_string(),
                    }
                }
            };

            tracing::info!("connection {conn_id_r} ended: {disconnect_reason}");

            let _ = event_tx.send(TransportEvent::Disconnected {
                connection_id: conn_id_r.clone(),
                reason: disconnect_reason,
            });

            // Clean up connection state
            {
                let mut conns = connections_for_cleanup.write().await;
                if let Some(ac) = conns.remove(&conn_id_r) {
                    if let Some(device_id) = &ac.info.device_id {
                        let mut d2c = device_to_conn.write().await;
                        d2c.remove(device_id);
                    }
                }
            }
        });
    }

    /// Send a message to a specific connection.
    ///
    /// Always uses v3 data frame format (2-byte header + payload).
    pub async fn send(
        &self,
        connection_id: &str,
        value: &serde_json::Value,
    ) -> Result<(), WsError> {
        let conns = self.connections.read().await;
        let conn = conns.get(connection_id).ok_or(WsError::Closed)?;

        let use_json = self.config.debug_json_mode;
        let encoded = websocket::encode_data_frame(value, use_json)?;

        conn.write_tx
            .send(encoded)
            .await
            .map_err(|_| WsError::Closed)?;

        Ok(())
    }

    /// Get the negotiated protocol version for a connection.
    ///
    /// Returns `PROTOCOL_V2` (2) for legacy connections, `PROTOCOL_V3` (3)
    /// for connections that completed a v3 handshake, or `None` if the
    /// connection is not found.
    pub async fn get_protocol_version(&self, connection_id: &str) -> Option<u8> {
        let conns = self.connections.read().await;
        conns
            .get(connection_id)
            .map(|ac| ac.protocol_version.load(Ordering::Acquire))
    }

    /// Set the device ID for a connection (after mesh announce).
    pub async fn set_device_id(&self, connection_id: &str, device_id: String) {
        let mut conns = self.connections.write().await;
        if let Some(conn) = conns.get_mut(connection_id) {
            if let Some(old_id) = conn.info.device_id.take() {
                let mut d2c = self.device_to_connection.write().await;
                d2c.remove(&old_id);
            }
            conn.info.device_id = Some(device_id.clone());
            let mut d2c = self.device_to_connection.write().await;
            d2c.insert(device_id.clone(), connection_id.to_string());

            let _ = self.event_tx.send(TransportEvent::DeviceIdentified {
                connection_id: connection_id.to_string(),
                device_id,
            });
        }
    }

    /// Get a snapshot of all connections.
    pub async fn get_connections(&self) -> Vec<WSConnection> {
        let conns = self.connections.read().await;
        conns.values().map(|ac| ac.info.clone()).collect()
    }

    /// Get a connection by its ID.
    pub async fn get_connection(&self, connection_id: &str) -> Option<WSConnection> {
        let conns = self.connections.read().await;
        conns.get(connection_id).map(|ac| ac.info.clone())
    }

    /// Get a connection by device ID.
    pub async fn get_connection_by_device(&self, device_id: &str) -> Option<WSConnection> {
        let d2c = self.device_to_connection.read().await;
        let conn_id = d2c.get(device_id)?;
        let conns = self.connections.read().await;
        conns.get(conn_id).map(|ac| ac.info.clone())
    }

    /// Close a specific connection.
    pub async fn close_connection(&self, connection_id: &str) {
        let mut conns = self.connections.write().await;
        if let Some(ac) = conns.remove(connection_id) {
            if let Some(device_id) = &ac.info.device_id {
                let mut d2c = self.device_to_connection.write().await;
                d2c.remove(device_id);
            }
            // Dropping write_tx closes the write pump, which closes the WS
        }

        let _ = self.event_tx.send(TransportEvent::Disconnected {
            connection_id: connection_id.to_string(),
            reason: "closed_by_local".to_string(),
        });
    }

    /// Close all connections.
    pub async fn close_all(&self) {
        let conn_ids: Vec<String> = {
            let conns = self.connections.read().await;
            conns.keys().cloned().collect()
        };

        for id in conn_ids {
            self.close_connection(&id).await;
        }
    }
}

/// BridgeHandler implementation for the ConnectionManager.
///
/// This allows the BridgeManager to route port 443 connections to the transport layer.
pub struct WsIncomingHandler {
    manager: Arc<ConnectionManager>,
}

impl WsIncomingHandler {
    pub fn new(manager: Arc<ConnectionManager>) -> Self {
        Self { manager }
    }
}

impl crate::bridge::manager::BridgeHandler for WsIncomingHandler {
    fn handle(
        &self,
        conn: BridgeConnection,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.manager.handle_incoming(conn).await;
        })
    }
}

pub struct WsOutgoingHandler {
    manager: Arc<ConnectionManager>,
}

impl WsOutgoingHandler {
    pub fn new(manager: Arc<ConnectionManager>) -> Self {
        Self { manager }
    }
}

impl crate::bridge::manager::BridgeHandler for WsOutgoingHandler {
    fn handle(
        &self,
        conn: BridgeConnection,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.manager.handle_outgoing(conn).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_config_default() {
        let config = TransportConfig::default();
        assert!(config.auto_reconnect);
        assert!(!config.debug_json_mode);
        assert_eq!(config.heartbeat.ping_interval, heartbeat::DEFAULT_PING_INTERVAL);
        assert_eq!(config.heartbeat.timeout, heartbeat::DEFAULT_HEARTBEAT_TIMEOUT);
    }

    #[tokio::test]
    async fn connection_manager_creation() {
        let config = TransportConfig::default();
        let (manager, _rx) = ConnectionManager::new(config);

        let conns = manager.get_connections().await;
        assert!(conns.is_empty());
    }

    #[tokio::test]
    async fn ws_echo_over_tcp() {
        use futures_util::{SinkExt, StreamExt};
        use tokio::net::TcpStream;
        use tokio_tungstenite::tungstenite::Message;

        // Create a TCP pair to simulate a bridge connection
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn a WS echo server
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws.split();
            while let Some(Ok(msg)) = read.next().await {
                if msg.is_binary() || msg.is_text() {
                    let _ = write.send(msg).await;
                }
            }
        });

        // Connect as client
        let client_stream = TcpStream::connect(addr).await.unwrap();
        let ws_config = websocket::ws_config();
        let (ws_stream, _) = tokio_tungstenite::client_async_with_config(
            format!("ws://{}", addr),
            client_stream,
            Some(ws_config),
        )
        .await
        .unwrap();

        let (mut write, mut read) = ws_stream.split();

        // Send a test message as v3 data frame
        let test_msg = websocket::encode_data_frame(
            &serde_json::json!({"namespace": "test", "type": "hello"}),
            false,
        )
        .unwrap();
        write
            .send(Message::Binary(bytes::Bytes::from(test_msg)))
            .await
            .unwrap();

        // Read the echo
        if let Some(Ok(Message::Binary(data))) = read.next().await {
            let decoded = websocket::decode_frame(&data).unwrap();
            match decoded {
                websocket::DecodedFrame::V3Data { payload, was_json } => {
                    assert_eq!(payload.get("type").unwrap().as_str().unwrap(), "hello");
                    assert!(!was_json);
                }
                other => panic!("expected V3Data, got {other:?}"),
            }
        } else {
            panic!("expected binary echo");
        }

        // Send a JSON-mode v3 data frame
        let json_msg = websocket::encode_data_frame(
            &serde_json::json!({"namespace": "mesh", "type": "announce"}),
            true,
        )
        .unwrap();
        write
            .send(Message::Binary(bytes::Bytes::from(json_msg)))
            .await
            .unwrap();

        if let Some(Ok(Message::Binary(data))) = read.next().await {
            let decoded = websocket::decode_frame(&data).unwrap();
            match decoded {
                websocket::DecodedFrame::V3Data { payload, was_json } => {
                    assert_eq!(payload.get("type").unwrap().as_str().unwrap(), "announce");
                    assert!(was_json);
                }
                other => panic!("expected V3Data, got {other:?}"),
            }
        } else {
            panic!("expected binary echo for JSON mode");
        }

        server_handle.abort();
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Helpers for WebSocket-over-duplex testing
    // ═══════════════════════════════════════════════════════════════════════

    use tokio_tungstenite::tungstenite::protocol::Role;

    /// Build a TransportConfig with a very long heartbeat timeout so heartbeat
    /// does not interfere with short-lived test connections.
    fn test_config() -> TransportConfig {
        TransportConfig {
            heartbeat: HeartbeatConfig {
                ping_interval: Duration::from_secs(600),
                timeout: Duration::from_secs(600),
            },
            auto_reconnect: false,
            max_reconnect_delay: Duration::from_secs(1),
            debug_json_mode: false,
            local_device_id: None,
        }
    }

    /// Create a duplex-backed WebSocket pair (server, client).
    ///
    /// Returns `(server_ws, client_ws)` where both sides are `WebSocketStream`
    /// over `DuplexStream`. The server side can be fed to `handle_ws_stream`,
    /// and the client side can send/receive messages for assertions.
    async fn duplex_ws_pair() -> (
        WebSocketStream<tokio::io::DuplexStream>,
        WebSocketStream<tokio::io::DuplexStream>,
    ) {
        let (client_io, server_io) = tokio::io::duplex(16384);
        let server_fut = WebSocketStream::from_raw_socket(server_io, Role::Server, None);
        let client_fut = WebSocketStream::from_raw_socket(client_io, Role::Client, None);
        // Both must be driven concurrently (from_raw_socket may exchange frames)
        let (server_ws, client_ws) = tokio::join!(server_fut, client_fut);
        (server_ws, client_ws)
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Connection lifecycle tests
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_handle_ws_stream_emits_connected_event_incoming() {
        let (manager, mut rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:9999".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // The Connected event should have been emitted synchronously in
        // setup_connection before the spawned tasks, so it's already in the
        // channel.
        match rx.try_recv() {
            Ok(TransportEvent::Connected(conn)) => {
                assert_eq!(conn.remote_addr, "100.64.0.2:9999");
                assert_eq!(conn.remote_dns_name, "peer.ts.net");
                assert_eq!(conn.direction, Direction::Incoming);
                assert_eq!(conn.status, ConnectionStatus::Connected);
                assert!(conn.device_id.is_none());
                assert!(conn.id.starts_with("incoming:"));
            }
            other => panic!("expected Connected event, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_handle_ws_stream_emits_connected_event_outgoing() {
        let (manager, mut rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.3:8080".into(),
                "other.ts.net".into(),
                Direction::Outgoing,
            )
            .await;

        match rx.try_recv() {
            Ok(TransportEvent::Connected(conn)) => {
                assert_eq!(conn.remote_addr, "100.64.0.3:8080");
                assert_eq!(conn.direction, Direction::Outgoing);
                assert!(conn.id.starts_with("dial:"));
            }
            other => panic!("expected Connected event, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_device_id_emits_identified_event() {
        let (manager, mut rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:9999".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // Consume the Connected event
        let _ = rx.recv().await.unwrap();

        let conn_id = "incoming:100.64.0.2:9999".to_string();
        manager.set_device_id(&conn_id, "device-abc".into()).await;

        match rx.try_recv() {
            Ok(TransportEvent::DeviceIdentified {
                connection_id,
                device_id,
            }) => {
                assert_eq!(connection_id, conn_id);
                assert_eq!(device_id, "device-abc");
            }
            other => panic!("expected DeviceIdentified event, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_device_id_updates_index() {
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:9999".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.2:9999";
        manager
            .set_device_id(conn_id, "device-xyz".into())
            .await;

        // Lookup by device_id should return the connection
        let found = manager.get_connection_by_device("device-xyz").await;
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.id, conn_id);
        assert_eq!(found.device_id.as_deref(), Some("device-xyz"));

        // Lookup by a non-existent device_id returns None
        let missing = manager.get_connection_by_device("no-such-device").await;
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_set_device_id_replaces_old_device_id() {
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:9999".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.2:9999";

        // Set initial device_id
        manager.set_device_id(conn_id, "old-device".into()).await;
        assert!(manager.get_connection_by_device("old-device").await.is_some());

        // Replace with new device_id
        manager.set_device_id(conn_id, "new-device".into()).await;

        // Old device_id should no longer resolve
        assert!(manager.get_connection_by_device("old-device").await.is_none());
        // New device_id should resolve
        let found = manager.get_connection_by_device("new-device").await.unwrap();
        assert_eq!(found.id, conn_id);
        assert_eq!(found.device_id.as_deref(), Some("new-device"));
    }

    #[tokio::test]
    async fn test_disconnect_emits_disconnected_event() {
        let (manager, mut rx) = ConnectionManager::new(test_config());
        let (server_ws, client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:9999".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // Consume the Connected event
        let _ = rx.recv().await.unwrap();

        // Drop the client side to trigger a disconnect in the read pump
        drop(client_ws);

        // The spawned lifecycle task should detect the closed stream and
        // emit a Disconnected event.
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for Disconnected event")
            .expect("channel closed unexpectedly");

        match event {
            TransportEvent::Disconnected {
                connection_id,
                reason,
            } => {
                assert_eq!(connection_id, "incoming:100.64.0.2:9999");
                // The reason should indicate the remote closed
                assert!(
                    reason.contains("remote_closed") || reason.contains("error"),
                    "unexpected reason: {}",
                    reason,
                );
            }
            other => panic!("expected Disconnected event, got {:?}", other),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Generic handle_ws_stream tests
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_handle_ws_stream_accepts_duplex_io() {
        // Verifies the generic method works with DuplexStream (not TcpStream).
        // This is the RFC 007 generification: any AsyncRead+AsyncWrite works.
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        // This call must compile and succeed — that is the test.
        manager
            .handle_ws_stream(
                server_ws,
                "127.0.0.1:1234".into(),
                "mock.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conns = manager.get_connections().await;
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].remote_addr, "127.0.0.1:1234");
    }

    #[tokio::test]
    async fn test_handle_ws_stream_connection_registered() {
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        // Before: no connections
        assert!(manager.get_connections().await.is_empty());

        manager
            .handle_ws_stream(
                server_ws,
                "10.0.0.1:5555".into(),
                "node.ts.net".into(),
                Direction::Outgoing,
            )
            .await;

        // After: one connection present
        let conns = manager.get_connections().await;
        assert_eq!(conns.len(), 1);
        let conn = &conns[0];
        assert_eq!(conn.id, "dial:10.0.0.1:5555");
        assert_eq!(conn.remote_addr, "10.0.0.1:5555");
        assert_eq!(conn.remote_dns_name, "node.ts.net");
        assert_eq!(conn.direction, Direction::Outgoing);
        assert_eq!(conn.status, ConnectionStatus::Connected);
        assert!(conn.device_id.is_none());
    }

    #[tokio::test]
    async fn test_handle_ws_stream_message_forwarded() {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

        let (manager, mut rx) = ConnectionManager::new(test_config());
        let (server_ws, mut client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.5:7777".into(),
                "sender.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // Consume the Connected event
        let _ = rx.recv().await.unwrap();

        // Send a message from the client side
        let payload = serde_json::json!({"namespace": "mesh", "type": "announce", "data": "hello"});
        let encoded = websocket::encode_data_frame(&payload, false).unwrap();
        client_ws
            .send(Message::Binary(bytes::Bytes::from(encoded)))
            .await
            .unwrap();

        // Should receive a TransportEvent::Message
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for Message event")
            .expect("channel closed");

        match event {
            TransportEvent::Message {
                connection_id,
                message,
            } => {
                assert_eq!(connection_id, "incoming:100.64.0.5:7777");
                assert_eq!(
                    message.payload.get("type").unwrap().as_str().unwrap(),
                    "announce"
                );
                assert_eq!(
                    message.payload.get("data").unwrap().as_str().unwrap(),
                    "hello"
                );
            }
            other => panic!("expected Message event, got {:?}", other),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Multiple connections tests
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_multiple_connections_tracked() {
        let (manager, _rx) = ConnectionManager::new(test_config());

        // Add 3 connections with different addresses
        let mut _clients = Vec::new();
        for i in 0..3 {
            let (server_ws, client_ws) = duplex_ws_pair().await;
            _clients.push(client_ws); // keep alive
            manager
                .handle_ws_stream(
                    server_ws,
                    format!("100.64.0.{}:9999", i + 1),
                    format!("node{}.ts.net", i + 1),
                    Direction::Incoming,
                )
                .await;
        }

        let conns = manager.get_connections().await;
        assert_eq!(conns.len(), 3);

        // Verify all three distinct addresses are present
        let addrs: Vec<&str> = conns.iter().map(|c| c.remote_addr.as_str()).collect();
        assert!(addrs.contains(&"100.64.0.1:9999"));
        assert!(addrs.contains(&"100.64.0.2:9999"));
        assert!(addrs.contains(&"100.64.0.3:9999"));
    }

    #[tokio::test]
    async fn test_get_connection_by_device_returns_correct() {
        let (manager, _rx) = ConnectionManager::new(test_config());

        // Two connections, each assigned a different device_id
        let (ws1, _c1) = duplex_ws_pair().await;
        let (ws2, _c2) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                ws1,
                "100.64.0.10:443".into(),
                "alpha.ts.net".into(),
                Direction::Incoming,
            )
            .await;
        manager
            .handle_ws_stream(
                ws2,
                "100.64.0.20:443".into(),
                "beta.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        manager
            .set_device_id("incoming:100.64.0.10:443", "device-alpha".into())
            .await;
        manager
            .set_device_id("incoming:100.64.0.20:443", "device-beta".into())
            .await;

        let alpha = manager
            .get_connection_by_device("device-alpha")
            .await
            .unwrap();
        assert_eq!(alpha.remote_addr, "100.64.0.10:443");
        assert_eq!(alpha.device_id.as_deref(), Some("device-alpha"));

        let beta = manager
            .get_connection_by_device("device-beta")
            .await
            .unwrap();
        assert_eq!(beta.remote_addr, "100.64.0.20:443");
        assert_eq!(beta.device_id.as_deref(), Some("device-beta"));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Cleanup tests
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_close_all_disconnects_everything() {
        let (manager, mut rx) = ConnectionManager::new(test_config());

        let mut _clients = Vec::new();
        for i in 0..3 {
            let (server_ws, client_ws) = duplex_ws_pair().await;
            _clients.push(client_ws);
            manager
                .handle_ws_stream(
                    server_ws,
                    format!("100.64.0.{}:443", i + 1),
                    format!("n{}.ts.net", i + 1),
                    Direction::Incoming,
                )
                .await;
        }

        // Drain the 3 Connected events
        for _ in 0..3 {
            let _ = rx.recv().await.unwrap();
        }

        assert_eq!(manager.get_connections().await.len(), 3);

        manager.close_all().await;

        // After close_all, get_connections should be empty
        assert!(manager.get_connections().await.is_empty());

        // Should have received 3 Disconnected events
        let mut disconnected_ids = Vec::new();
        for _ in 0..3 {
            match rx.try_recv() {
                Ok(TransportEvent::Disconnected {
                    connection_id,
                    reason,
                }) => {
                    assert_eq!(reason, "closed_by_local");
                    disconnected_ids.push(connection_id);
                }
                other => panic!("expected Disconnected event, got {:?}", other),
            }
        }

        assert_eq!(disconnected_ids.len(), 3);
    }

    #[tokio::test]
    async fn test_close_connection_removes_device_index() {
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.2:443";
        manager.set_device_id(conn_id, "device-123".into()).await;

        // Verify device lookup works
        assert!(manager.get_connection_by_device("device-123").await.is_some());

        // Close the connection
        manager.close_connection(conn_id).await;

        // Device lookup should now return None
        assert!(manager.get_connection_by_device("device-123").await.is_none());

        // Connection should be gone
        assert!(manager.get_connection(conn_id).await.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Transport event subscription tests
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_subscribe_receives_all_events() {
        let (manager, _initial_rx) = ConnectionManager::new(test_config());

        // Subscribe after creation
        let mut sub = manager.subscribe();

        // Generate a Connected event
        let (server_ws, _client_ws) = duplex_ws_pair().await;
        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // Generate a DeviceIdentified event
        let conn_id = "incoming:100.64.0.2:443";
        manager.set_device_id(conn_id, "dev-1".into()).await;

        // Generate a Disconnected event (via close_connection)
        manager.close_connection(conn_id).await;

        // Verify the subscriber received all three events in order
        let ev1 = sub.recv().await.unwrap();
        assert!(matches!(ev1, TransportEvent::Connected(_)));

        let ev2 = sub.recv().await.unwrap();
        assert!(matches!(
            ev2,
            TransportEvent::DeviceIdentified { .. }
        ));

        let ev3 = sub.recv().await.unwrap();
        assert!(matches!(
            ev3,
            TransportEvent::Disconnected { .. }
        ));
    }

    #[tokio::test]
    async fn test_multiple_subscribers_receive_events() {
        let (manager, _initial_rx) = ConnectionManager::new(test_config());

        let mut sub1 = manager.subscribe();
        let mut sub2 = manager.subscribe();

        let (server_ws, _client_ws) = duplex_ws_pair().await;
        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // Both subscribers should receive the Connected event
        let ev1 = sub1.recv().await.unwrap();
        let ev2 = sub2.recv().await.unwrap();

        assert!(matches!(ev1, TransportEvent::Connected(_)));
        assert!(matches!(ev2, TransportEvent::Connected(_)));

        // Verify both got the same connection info
        if let (
            TransportEvent::Connected(c1),
            TransportEvent::Connected(c2),
        ) = (ev1, ev2)
        {
            assert_eq!(c1.id, c2.id);
            assert_eq!(c1.remote_addr, c2.remote_addr);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Connection lookup / filtering tests
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_get_connection_by_id() {
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn = manager
            .get_connection("incoming:100.64.0.2:443")
            .await;
        assert!(conn.is_some());
        let conn = conn.unwrap();
        assert_eq!(conn.remote_dns_name, "peer.ts.net");

        // Non-existent ID
        let missing = manager.get_connection("no-such-id").await;
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_get_connections_mixed_directions() {
        let (manager, _rx) = ConnectionManager::new(test_config());

        let (ws_in, _c1) = duplex_ws_pair().await;
        let (ws_out, _c2) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                ws_in,
                "100.64.0.2:443".into(),
                "incoming-peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;
        manager
            .handle_ws_stream(
                ws_out,
                "100.64.0.3:443".into(),
                "outgoing-peer.ts.net".into(),
                Direction::Outgoing,
            )
            .await;

        let conns = manager.get_connections().await;
        assert_eq!(conns.len(), 2);

        let incoming: Vec<_> = conns
            .iter()
            .filter(|c| c.direction == Direction::Incoming)
            .collect();
        let outgoing: Vec<_> = conns
            .iter()
            .filter(|c| c.direction == Direction::Outgoing)
            .collect();

        assert_eq!(incoming.len(), 1);
        assert_eq!(outgoing.len(), 1);
        assert_eq!(incoming[0].remote_dns_name, "incoming-peer.ts.net");
        assert_eq!(outgoing[0].remote_dns_name, "outgoing-peer.ts.net");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Send tests
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_send_to_nonexistent_connection_returns_error() {
        let (manager, _rx) = ConnectionManager::new(test_config());

        let result = manager
            .send("no-such-connection", &serde_json::json!({"type": "test"}))
            .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), WsError::Closed));
    }

    #[tokio::test]
    async fn test_send_message_received_by_client() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;

        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, mut client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.2:443";

        // Send a message through the connection manager
        let msg = serde_json::json!({"namespace": "mesh", "type": "state", "value": 42});
        manager.send(conn_id, &msg).await.unwrap();

        // Client should receive it as a binary frame
        let received = tokio::time::timeout(Duration::from_secs(5), client_ws.next())
            .await
            .expect("timed out waiting for message")
            .expect("stream ended")
            .expect("ws error");

        match received {
            Message::Binary(data) => {
                let decoded = websocket::decode_frame(&data).unwrap();
                match decoded {
                    websocket::DecodedFrame::V3Data { payload, .. } => {
                        assert_eq!(
                            payload.get("type").unwrap().as_str().unwrap(),
                            "state"
                        );
                        assert_eq!(
                            payload.get("value").unwrap().as_i64().unwrap(),
                            42
                        );
                    }
                    other => panic!("expected V3Data, got {other:?}"),
                }
            }
            other => panic!("expected binary message, got {:?}", other),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Connection ID format tests
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_connection_id_format_incoming() {
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "192.168.1.100:12345".into(),
                "host.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conns = manager.get_connections().await;
        assert_eq!(conns[0].id, "incoming:192.168.1.100:12345");
    }

    #[tokio::test]
    async fn test_connection_id_format_outgoing() {
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "10.0.0.1:443".into(),
                "remote.ts.net".into(),
                Direction::Outgoing,
            )
            .await;

        let conns = manager.get_connections().await;
        assert_eq!(conns[0].id, "dial:10.0.0.1:443");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Adversarial edge case tests
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_client_drops_immediately_after_connect() {
        // Edge case #1: WebSocket upgrade completes, client drops before
        // any message. Verify Disconnected event fires quickly.
        let (manager, mut rx) = ConnectionManager::new(test_config());
        let (server_ws, client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.99:1111".into(),
                "drop-fast.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // Consume Connected
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, TransportEvent::Connected(_)));

        // Drop client immediately — no messages sent
        drop(client_ws);

        // Disconnected should fire within a short window
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for Disconnected after immediate drop")
            .expect("channel closed");

        match event {
            TransportEvent::Disconnected {
                connection_id,
                reason,
            } => {
                assert_eq!(connection_id, "incoming:100.64.0.99:1111");
                assert!(
                    reason.contains("remote_closed") || reason.contains("error"),
                    "unexpected reason after immediate drop: {reason}"
                );
            }
            other => panic!("expected Disconnected, got {:?}", other),
        }

        // Connection should be cleaned up
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(manager.get_connections().await.is_empty());
    }

    #[tokio::test]
    async fn test_send_to_closed_connection_returns_error() {
        // Edge case #2: Connection established, client drops. Server calls
        // send(). Verify it returns an error (not panic).
        let (manager, mut rx) = ConnectionManager::new(test_config());
        let (server_ws, client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.88:2222".into(),
                "send-closed.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // Consume Connected
        let _ = rx.recv().await.unwrap();
        let conn_id = "incoming:100.64.0.88:2222";

        // Drop client
        drop(client_ws);

        // Wait for Disconnected to be processed
        let _disc = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");

        // Now try to send — should return error, not panic
        let result = manager
            .send(conn_id, &serde_json::json!({"type": "test"}))
            .await;
        assert!(result.is_err(), "send to closed connection should fail");
    }

    #[tokio::test]
    async fn test_rapid_connect_disconnect_cycles() {
        // Edge case #3: Connect 10 times in rapid succession, each drops
        // immediately. Verify no resource leaks (get_connections returns
        // empty after all drop).
        let (manager, mut _rx) = ConnectionManager::new(test_config());

        for i in 0..10u16 {
            let (server_ws, client_ws) = duplex_ws_pair().await;
            manager
                .handle_ws_stream(
                    server_ws,
                    format!("100.64.0.{}:{}", i / 256, 3000 + i),
                    format!("rapid-{i}.ts.net"),
                    Direction::Incoming,
                )
                .await;
            // Drop client immediately
            drop(client_ws);
        }

        // Give spawned tasks time to clean up
        tokio::time::sleep(Duration::from_millis(500)).await;

        let conns = manager.get_connections().await;
        assert!(
            conns.is_empty(),
            "expected all connections cleaned up after rapid connect/disconnect, got {} remaining",
            conns.len()
        );
    }

    #[tokio::test]
    async fn test_set_device_id_on_closed_connection() {
        // Edge case #4: Connection closes, then set_device_id is called.
        // Verify graceful handling (no panic).
        let (manager, mut rx) = ConnectionManager::new(test_config());
        let (server_ws, client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.77:4444".into(),
                "closed-device.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let _ = rx.recv().await.unwrap(); // Connected
        let conn_id = "incoming:100.64.0.77:4444";

        // Close the connection
        drop(client_ws);
        // Wait for disconnect to be processed
        let _disc = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");

        // Wait a bit for cleanup
        tokio::time::sleep(Duration::from_millis(100)).await;

        // This should not panic — the connection is gone
        manager
            .set_device_id(conn_id, "ghost-device".into())
            .await;

        // The device should NOT be in the index since the connection is gone
        assert!(manager
            .get_connection_by_device("ghost-device")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn test_set_device_id_twice_updates_correctly() {
        // Edge case #5: Call set_device_id("dev-a"), then set_device_id("dev-b")
        // on same connection. Verify device index is updated correctly
        // (old mapping removed, new one works).
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.66:5555".into(),
                "double-id.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.66:5555";

        // Set first device ID
        manager.set_device_id(conn_id, "dev-a".into()).await;
        assert!(manager.get_connection_by_device("dev-a").await.is_some());

        // Set second device ID — should replace first
        manager.set_device_id(conn_id, "dev-b".into()).await;

        // Old device_id removed
        assert!(
            manager.get_connection_by_device("dev-a").await.is_none(),
            "old device_id 'dev-a' should be removed from index"
        );
        // New device_id works
        let found = manager
            .get_connection_by_device("dev-b")
            .await
            .expect("new device_id 'dev-b' should resolve");
        assert_eq!(found.id, conn_id);
        assert_eq!(found.device_id.as_deref(), Some("dev-b"));
    }

    #[tokio::test]
    async fn test_two_connections_same_device_id() {
        // Edge case #6: Two connections both get set_device_id("dev-1").
        // Verify get_connection_by_device returns the latest one.
        let (manager, _rx) = ConnectionManager::new(test_config());

        let (ws1, _c1) = duplex_ws_pair().await;
        let (ws2, _c2) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                ws1,
                "100.64.0.55:6001".into(),
                "dup-dev-1.ts.net".into(),
                Direction::Incoming,
            )
            .await;
        manager
            .handle_ws_stream(
                ws2,
                "100.64.0.55:6002".into(),
                "dup-dev-2.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn1_id = "incoming:100.64.0.55:6001";
        let conn2_id = "incoming:100.64.0.55:6002";

        // First connection claims "dev-1"
        manager.set_device_id(conn1_id, "dev-1".into()).await;
        let found = manager.get_connection_by_device("dev-1").await.unwrap();
        assert_eq!(found.id, conn1_id);

        // Second connection also claims "dev-1" — should win
        manager.set_device_id(conn2_id, "dev-1".into()).await;
        let found = manager.get_connection_by_device("dev-1").await.unwrap();
        assert_eq!(
            found.id, conn2_id,
            "latest set_device_id should win the device index"
        );
    }

    #[tokio::test]
    async fn test_close_all_with_no_connections() {
        // Edge case #14: Call close_all() on empty manager. Verify no panic.
        let (manager, _rx) = ConnectionManager::new(test_config());

        assert!(manager.get_connections().await.is_empty());

        // This should not panic
        manager.close_all().await;

        assert!(manager.get_connections().await.is_empty());
    }

    #[tokio::test]
    async fn test_get_connection_by_device_with_no_devices_bound() {
        // Edge case #15: No set_device_id called. Verify returns None.
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.44:7777".into(),
                "no-device.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // Connection exists but no device_id was set
        assert!(manager
            .get_connection_by_device("any-device")
            .await
            .is_none());
        assert!(manager
            .get_connection_by_device("")
            .await
            .is_none());
        assert!(manager
            .get_connection_by_device("incoming:100.64.0.44:7777")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn test_concurrent_subscribe_and_event() {
        // Edge case #16: Subscribe while events are being emitted.
        // Verify subscriber eventually gets events (no missed channel).
        let (manager, _initial_rx) = ConnectionManager::new(test_config());

        // Subscribe before any events
        let mut sub = manager.subscribe();

        // Generate events rapidly
        let mut _clients = Vec::new();
        for i in 0..5u8 {
            let (server_ws, client_ws) = duplex_ws_pair().await;
            _clients.push(client_ws);
            manager
                .handle_ws_stream(
                    server_ws,
                    format!("100.64.0.{}:8888", 30 + i),
                    format!("concurrent-{i}.ts.net"),
                    Direction::Incoming,
                )
                .await;
        }

        // Subscribe a second receiver mid-stream
        let mut sub2 = manager.subscribe();

        // Generate more events
        for i in 0..5u8 {
            let (server_ws, client_ws) = duplex_ws_pair().await;
            _clients.push(client_ws);
            manager
                .handle_ws_stream(
                    server_ws,
                    format!("100.64.0.{}:9999", 40 + i),
                    format!("concurrent-late-{i}.ts.net"),
                    Direction::Incoming,
                )
                .await;
        }

        // First subscriber should have all 10 Connected events
        let mut count1 = 0;
        while let Ok(ev) = sub.try_recv() {
            if matches!(ev, TransportEvent::Connected(_)) {
                count1 += 1;
            }
        }
        assert_eq!(count1, 10, "first subscriber should have all 10 events");

        // Second subscriber should have the 5 events after it subscribed
        let mut count2 = 0;
        while let Ok(ev) = sub2.try_recv() {
            if matches!(ev, TransportEvent::Connected(_)) {
                count2 += 1;
            }
        }
        assert_eq!(
            count2, 5,
            "second subscriber should have 5 events (subscribed mid-stream)"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // RFC 009 Phase 4: Handshake and Protocol Version tests
    // ═══════════════════════════════════════════════════════════════════════

    /// Build a TransportConfig that sends v3 handshakes.
    fn test_config_with_handshake(device_id: &str) -> TransportConfig {
        TransportConfig {
            heartbeat: HeartbeatConfig {
                ping_interval: Duration::from_secs(600),
                timeout: Duration::from_secs(600),
            },
            auto_reconnect: false,
            max_reconnect_delay: Duration::from_secs(1),
            debug_json_mode: false,
            local_device_id: Some(device_id.to_string()),
        }
    }

    #[test]
    fn ws_connection_default_protocol_version_is_v2() {
        let conn = WSConnection {
            id: "test".to_string(),
            device_id: None,
            direction: Direction::Incoming,
            remote_addr: "127.0.0.1:1234".to_string(),
            remote_dns_name: "test.ts.net".to_string(),
            status: ConnectionStatus::Connected,
            protocol_version: PROTOCOL_V2,
        };
        assert_eq!(conn.protocol_version, PROTOCOL_V2);
        assert_eq!(conn.protocol_version, 2);
    }

    #[test]
    fn protocol_version_constants() {
        assert_eq!(PROTOCOL_V2, 2);
        assert_eq!(PROTOCOL_V3, 3);
        assert!(PROTOCOL_V3 > PROTOCOL_V2);
    }

    #[tokio::test]
    async fn test_connection_starts_at_v2() {
        // When local_device_id is None, no handshake is sent.
        // Protocol version should remain at v2.
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, _client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.2:443";
        let version = manager.get_protocol_version(conn_id).await;
        assert_eq!(version, Some(PROTOCOL_V2));
    }

    #[tokio::test]
    async fn test_get_protocol_version_missing_connection() {
        let (manager, _rx) = ConnectionManager::new(test_config());
        assert_eq!(manager.get_protocol_version("no-such-conn").await, None);
    }

    #[tokio::test]
    async fn test_handshake_sent_when_device_id_configured() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;

        // Create a manager that has a local_device_id, triggering handshake
        let config = test_config_with_handshake("local-dev-1");
        let (manager, _rx) = ConnectionManager::new(config);
        let (server_ws, mut client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // The client side should receive a v3 handshake control frame
        let msg = tokio::time::timeout(Duration::from_secs(5), client_ws.next())
            .await
            .expect("timed out waiting for handshake")
            .expect("stream ended")
            .expect("ws error");

        match msg {
            Message::Binary(data) => {
                // Should be a v3 control frame
                let frame = websocket::decode_frame(&data).unwrap();
                match frame {
                    DecodedFrame::V3Control(ControlMessage::Handshake {
                        protocol_version,
                        device_id,
                        capabilities,
                    }) => {
                        assert_eq!(protocol_version, PROTOCOL_V3 as u32);
                        assert_eq!(device_id, "local-dev-1");
                        assert!(capabilities.is_empty());
                    }
                    other => panic!("expected Handshake, got {other:?}"),
                }
            }
            other => panic!("expected binary frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_no_handshake_sent_when_device_id_not_configured() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;

        // No local_device_id => no handshake sent
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, mut client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // The heartbeat loop sends a v3 ping immediately on the first tick.
        // We should NOT receive a Handshake frame. Any frame that arrives
        // should be a heartbeat ping, not a handshake.
        let result =
            tokio::time::timeout(Duration::from_millis(200), client_ws.next()).await;

        match result {
            Err(_) => {
                // Timeout — no message at all, which is fine
            }
            Ok(Some(Ok(Message::Binary(data)))) => {
                // A message arrived — verify it's NOT a handshake
                let frame = websocket::decode_frame(&data).unwrap();
                match frame {
                    DecodedFrame::V3Control(ControlMessage::Handshake { .. }) => {
                        panic!("received a Handshake when local_device_id is None");
                    }
                    _ => {
                        // Heartbeat ping or other — acceptable
                    }
                }
            }
            Ok(other) => {
                // Stream ended or text frame — fine for this test
                let _ = other;
            }
        }
    }

    #[tokio::test]
    async fn test_handshake_ack_upgrades_to_v3() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        // Create a manager with handshake enabled
        let config = test_config_with_handshake("local-dev-1");
        let (manager, _rx) = ConnectionManager::new(config);
        let (server_ws, mut client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.2:443";

        // Consume the handshake that the manager sent
        let _handshake_msg = tokio::time::timeout(
            Duration::from_secs(5),
            client_ws.next(),
        )
        .await
        .expect("timed out waiting for handshake")
        .expect("stream ended")
        .expect("ws error");

        // Still v2 before ack
        assert_eq!(
            manager.get_protocol_version(conn_id).await,
            Some(PROTOCOL_V2)
        );

        // Client sends HandshakeAck back
        let ack = ControlMessage::HandshakeAck {
            protocol_version: 3,
            device_id: "peer-dev-1".to_string(),
            negotiated_version: 3,
        };
        let ack_frame = websocket::encode_control_frame(&ack, false).unwrap();
        client_ws
            .send(Message::Binary(bytes::Bytes::from(ack_frame)))
            .await
            .unwrap();

        // Give the message loop time to process the ack
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Now the connection should be upgraded to v3
        assert_eq!(
            manager.get_protocol_version(conn_id).await,
            Some(PROTOCOL_V3)
        );
    }

    #[tokio::test]
    async fn test_incoming_handshake_sends_ack_and_upgrades() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        // Create a manager without local_device_id (won't send handshake first)
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, mut client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.5:443".into(),
                "v3-peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.5:443";

        // Peer sends a Handshake to us
        let handshake = ControlMessage::Handshake {
            protocol_version: 3,
            device_id: "remote-peer".to_string(),
            capabilities: vec!["mesh".to_string()],
        };
        let handshake_frame =
            websocket::encode_control_frame(&handshake, false).unwrap();
        client_ws
            .send(Message::Binary(bytes::Bytes::from(handshake_frame)))
            .await
            .unwrap();

        // We should receive a HandshakeAck back
        let ack_msg = tokio::time::timeout(Duration::from_secs(5), client_ws.next())
            .await
            .expect("timed out waiting for ack")
            .expect("stream ended")
            .expect("ws error");

        match ack_msg {
            Message::Binary(data) => {
                let frame = websocket::decode_frame(&data).unwrap();
                match frame {
                    DecodedFrame::V3Control(ControlMessage::HandshakeAck {
                        protocol_version,
                        negotiated_version,
                        device_id,
                    }) => {
                        assert_eq!(protocol_version, PROTOCOL_V3 as u32);
                        assert_eq!(negotiated_version, 3);
                        // device_id is "unknown" because no local_device_id was set
                        assert_eq!(device_id, "unknown");
                    }
                    other => panic!("expected HandshakeAck, got {other:?}"),
                }
            }
            other => panic!("expected binary frame, got {other:?}"),
        }

        // Connection should be upgraded to v3
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            manager.get_protocol_version(conn_id).await,
            Some(PROTOCOL_V3)
        );
    }

    #[tokio::test]
    async fn test_send_always_uses_v3_format() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;

        // send() always uses v3 format regardless of handshake state
        let (manager, _rx) = ConnectionManager::new(test_config());
        let (server_ws, mut client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.2:443";

        // Send a message — should always use v3 format (2-byte header)
        let msg = serde_json::json!({"namespace": "mesh", "type": "test"});
        manager.send(conn_id, &msg).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(5), client_ws.next())
            .await
            .expect("timed out")
            .expect("stream ended")
            .expect("ws error");

        match received {
            Message::Binary(data) => {
                // v3 format: byte 0 is FrameType::Data (0x02)
                assert_eq!(data[0], 0x02, "expected v3 Data frame type byte");
                // Verify it decodes as v3 data
                let decoded = websocket::decode_frame(&data).unwrap();
                match decoded {
                    websocket::DecodedFrame::V3Data { payload, .. } => {
                        assert_eq!(payload["type"], "test");
                    }
                    other => panic!("expected V3Data, got {other:?}"),
                }
            }
            other => panic!("expected binary, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_version_aware_send_v3_after_handshake() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        // Setup with handshake enabled
        let config = test_config_with_handshake("dev-local");
        let (manager, _rx) = ConnectionManager::new(config);
        let (server_ws, mut client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.2:443".into(),
                "peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.2:443";

        // Consume the handshake
        let _hs = tokio::time::timeout(Duration::from_secs(5), client_ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        // Send HandshakeAck from peer to upgrade
        let ack = ControlMessage::HandshakeAck {
            protocol_version: 3,
            device_id: "peer-dev".to_string(),
            negotiated_version: 3,
        };
        let ack_frame = websocket::encode_control_frame(&ack, false).unwrap();
        client_ws
            .send(Message::Binary(bytes::Bytes::from(ack_frame)))
            .await
            .unwrap();

        // Wait for upgrade
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            manager.get_protocol_version(conn_id).await,
            Some(PROTOCOL_V3)
        );

        // Now send a data message — should use v3 format
        let msg = serde_json::json!({"namespace": "sync", "type": "sync-full"});
        manager.send(conn_id, &msg).await.unwrap();

        // Read frames from the client, skipping any heartbeat pings, until
        // we find the data frame.
        let data_frame = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let msg = client_ws.next().await.expect("stream ended").expect("ws error");
                if let Message::Binary(data) = msg {
                    // Skip v3 control frames (heartbeat pings)
                    if data[0] == 0x01 {
                        continue;
                    }
                    return data.to_vec();
                }
            }
        })
        .await
        .expect("timed out waiting for v3 data frame");

        // v3 format: byte 0 = 0x02 (Data), byte 1 = flags
        assert_eq!(
            data_frame[0], 0x02,
            "expected v3 Data frame type byte (0x02), got 0x{:02X}",
            data_frame[0]
        );
        // Verify it decodes as v3 data
        let decoded = websocket::decode_frame(&data_frame).unwrap();
        match decoded {
            DecodedFrame::V3Data { payload, .. } => {
                assert_eq!(payload["type"], "sync-full");
            }
            other => panic!("expected V3Data, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_full_bidirectional_handshake() {
        // Both sides have handshake enabled
        let config_a = test_config_with_handshake("device-a");
        let config_b = test_config_with_handshake("device-b");

        let (manager_a, _rx_a) = ConnectionManager::new(config_a);
        let (manager_b, _rx_b) = ConnectionManager::new(config_b);

        let (ws_a, ws_b) = duplex_ws_pair().await;

        // Manager A handles one side (incoming)
        manager_a
            .handle_ws_stream(
                ws_a,
                "100.64.0.1:443".into(),
                "a.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        // Manager B handles the other side (outgoing)
        manager_b
            .handle_ws_stream(
                ws_b,
                "100.64.0.2:443".into(),
                "b.ts.net".into(),
                Direction::Outgoing,
            )
            .await;

        let conn_id_a = "incoming:100.64.0.1:443";
        let conn_id_b = "dial:100.64.0.2:443";

        // Both sides send handshakes and process acks.
        // Wait for the handshake exchange to complete.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Both should be upgraded to v3
        assert_eq!(
            manager_a.get_protocol_version(conn_id_a).await,
            Some(PROTOCOL_V3),
            "manager_a should be v3"
        );
        assert_eq!(
            manager_b.get_protocol_version(conn_id_b).await,
            Some(PROTOCOL_V3),
            "manager_b should be v3"
        );
    }

    #[tokio::test]
    async fn test_handshake_version_negotiation_min() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        // Create a manager that sends v3 handshake
        let config = test_config_with_handshake("local-node");
        let (manager, _rx) = ConnectionManager::new(config);
        let (server_ws, mut client_ws) = duplex_ws_pair().await;

        manager
            .handle_ws_stream(
                server_ws,
                "100.64.0.9:443".into(),
                "old-peer.ts.net".into(),
                Direction::Incoming,
            )
            .await;

        let conn_id = "incoming:100.64.0.9:443";

        // Consume the handshake
        let _hs = tokio::time::timeout(Duration::from_secs(5), client_ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        // Peer responds with negotiated_version: 2 (it only supports v2)
        let ack = ControlMessage::HandshakeAck {
            protocol_version: 2,
            device_id: "old-peer".to_string(),
            negotiated_version: 2,
        };
        let ack_frame = websocket::encode_control_frame(&ack, false).unwrap();
        client_ws
            .send(Message::Binary(bytes::Bytes::from(ack_frame)))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        // negotiated_version < 3, so we stay at v2
        assert_eq!(
            manager.get_protocol_version(conn_id).await,
            Some(PROTOCOL_V2),
            "should remain v2 when peer negotiates v2"
        );
    }
}
