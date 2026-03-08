use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::WebSocketStream;

use crate::bridge::header::Direction;
use crate::bridge::manager::BridgeConnection;

use super::heartbeat::{self, HeartbeatConfig, HeartbeatTimeout};
use super::reconnect::ReconnectTarget;
use super::websocket::{self, WireMessage, WsError};

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
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            heartbeat: HeartbeatConfig::default(),
            auto_reconnect: true,
            max_reconnect_delay: super::reconnect::MAX_RECONNECT_DELAY,
            debug_json_mode: false,
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
        };

        self.setup_connection(connection_id, ws_stream, ws_conn)
            .await;
    }

    /// Set up read/write pumps and heartbeat for a connected WebSocket.
    async fn setup_connection(
        &self,
        connection_id: String,
        ws_stream: WebSocketStream<TcpStream>,
        ws_conn: WSConnection,
    ) {
        let (write_half, read_half) = ws_stream.split();

        // Channels
        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(256);
        let (msg_tx, mut msg_rx) = mpsc::channel::<WireMessage>(256);
        let (activity_tx, activity_rx) = mpsc::channel::<()>(64);

        let active_conn = ActiveConnection {
            info: ws_conn.clone(),
            write_tx: write_tx.clone(),
            activity_tx: activity_tx.clone(),
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

        // Spawn the connection lifecycle task
        let conn_id_r = connection_id.clone();
        let event_tx = self.event_tx.clone();
        let connections_for_pong = self.connections.clone();
        let connections_for_cleanup = self.connections.clone();
        let device_to_conn = self.device_to_connection.clone();
        let config = self.config.clone();

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

            // Message forwarding loop
            let conn_id_msg = conn_id_r.clone();
            let event_tx_msg = event_tx.clone();
            let msg_forward = tokio::spawn(async move {
                while let Some(msg) = msg_rx.recv().await {
                    // Record activity for heartbeat
                    let _ = activity_tx.send(()).await;

                    // Check if it's a heartbeat message
                    if heartbeat::is_heartbeat_message(&msg.payload) {
                        if let Some(ts) = msg.payload.get("timestamp").and_then(|v| v.as_u64()) {
                            if msg.payload.get("type").and_then(|v| v.as_str()) == Some("ping") {
                                let pong = heartbeat::create_pong(ts);
                                let conns = connections_for_pong.read().await;
                                if let Some(ac) = conns.get(&conn_id_msg) {
                                    if let Ok(encoded) = websocket::encode_message(&pong, false) {
                                        let _ = ac.write_tx.send(encoded).await;
                                    }
                                }
                            }
                        }
                        continue;
                    }

                    // Forward application message
                    let _ = event_tx_msg.send(TransportEvent::Message {
                        connection_id: conn_id_msg.clone(),
                        message: msg,
                    });
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
    pub async fn send(
        &self,
        connection_id: &str,
        value: &serde_json::Value,
    ) -> Result<(), WsError> {
        let conns = self.connections.read().await;
        let conn = conns.get(connection_id).ok_or(WsError::Closed)?;

        let encoded = websocket::encode_message(value, self.config.debug_json_mode)?;
        conn.write_tx
            .send(encoded)
            .await
            .map_err(|_| WsError::Closed)?;

        Ok(())
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

        // Send a test message as WS binary with flags prefix
        let test_msg = websocket::encode_message(
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
            let decoded = websocket::decode_message(&data).unwrap();
            assert_eq!(
                decoded.payload.get("type").unwrap().as_str().unwrap(),
                "hello"
            );
            assert!(!decoded.was_json);
        } else {
            panic!("expected binary echo");
        }

        // Send a JSON-mode message
        let json_msg = websocket::encode_message(
            &serde_json::json!({"namespace": "mesh", "type": "announce"}),
            true,
        )
        .unwrap();
        write
            .send(Message::Binary(bytes::Bytes::from(json_msg)))
            .await
            .unwrap();

        if let Some(Ok(Message::Binary(data))) = read.next().await {
            let decoded = websocket::decode_message(&data).unwrap();
            assert_eq!(
                decoded.payload.get("type").unwrap().as_str().unwrap(),
                "announce"
            );
            assert!(decoded.was_json);
        } else {
            panic!("expected binary echo for JSON mode");
        }

        server_handle.abort();
    }
}
