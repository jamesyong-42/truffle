//! WebSocket transport — [`StreamTransport`] implementation over WebSocket.
//!
//! Uses `tokio-tungstenite` for the WS protocol and delegates all raw TCP
//! connectivity to Layer 3's [`NetworkProvider`].
//!
//! # Connection flow
//!
//! **Client (connect)**:
//! 1. `NetworkProvider::dial_tcp(addr, port)` -> raw `TcpStream`
//! 2. WebSocket client handshake (`tokio_tungstenite::client_async_with_config`)
//! 3. Send a [`HelloEnvelope`] (RFC 017 §8) as a text frame (JSON)
//! 4. Receive peer's [`HelloEnvelope`], validate `app_id` and version
//! 5. Split stream into read/write halves, return [`WsFramedStream`]
//!
//! **Server (listen)**:
//! 1. `NetworkProvider::listen_tcp(port)` -> incoming `TcpStream`s
//! 2. For each: WebSocket server handshake (`tokio_tungstenite::accept_async_with_config`)
//! 3. Receive peer's [`HelloEnvelope`], validate, send own [`HelloEnvelope`]
//! 4. Split stream into read/write halves, yield [`WsFramedStream`] via [`StreamListener`]
//!
//! The `app_id` check happens during the hello exchange: a mismatch closes
//! the socket with close code 4001 so the remote sees a specific reason. A
//! missing or malformed hello closes with code 4002.
//!
//! # Heartbeat
//!
//! After the hello exchange, a background task sends WebSocket Ping frames
//! at `WsConfig::ping_interval` on the write half. The read half updates a
//! shared `last_pong` timestamp when it receives a Pong. The heartbeat task
//! checks this timestamp on each ping cycle and closes the connection if
//! `pong_timeout` has been exceeded.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::network::{NetworkProvider, PeerAddr};
use crate::session::hello::{
    HelloEnvelope, HelloKind, PeerIdentity, CLOSE_APP_MISMATCH, CLOSE_HELLO_PROTOCOL, HELLO_TIMEOUT,
};

use super::{
    resolve_dial_addr, FramedStream, StreamListener, StreamTransport, TransportError, WsConfig,
};

// ---------------------------------------------------------------------------
// WebSocketTransport
// ---------------------------------------------------------------------------

/// WebSocket-based [`StreamTransport`] implementation.
///
/// Generic over the [`NetworkProvider`] type `N`. Owns a shared reference
/// to Layer 3 for establishing raw TCP connections, and a [`WsConfig`]
/// for protocol parameters.
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
/// use truffle_core::transport::{WsConfig, websocket::WebSocketTransport};
///
/// let ws = WebSocketTransport::new(Arc::new(provider), WsConfig::default());
/// let stream = ws.connect(&peer_addr).await?;
/// ```
pub struct WebSocketTransport<N: NetworkProvider> {
    /// Layer 3 network provider for raw TCP dial/listen.
    network: Arc<N>,
    /// WebSocket configuration.
    config: WsConfig,
}

impl<N: NetworkProvider + 'static> WebSocketTransport<N> {
    /// Create a new WebSocket transport.
    ///
    /// - `network`: An `Arc<N>` where `N: NetworkProvider`.
    /// - `config`: WebSocket configuration (port, heartbeat intervals, etc.).
    pub fn new(network: Arc<N>, config: WsConfig) -> Self {
        Self { network, config }
    }

    /// Build the local hello envelope from the network provider's identity.
    ///
    /// RFC 017 Phase 2: the transport handshake is now a [`HelloEnvelope`]
    /// carrying `app_id` + `device_id` + `device_name` + `os` + an escape
    /// hatch `tailscale_id` used by the session layer to correlate the
    /// link with its Layer 3 peer entry.
    fn local_hello(&self) -> HelloEnvelope {
        let identity = self.network.local_identity();
        HelloEnvelope::new(PeerIdentity {
            app_id: identity.app_id,
            device_id: identity.device_id,
            device_name: identity.device_name,
            os: std::env::consts::OS.to_string(),
            tailscale_id: identity.tailscale_id,
        })
    }

    /// Build a tungstenite `WebSocketConfig` from our `WsConfig`.
    fn ws_protocol_config(&self) -> WebSocketConfig {
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(self.config.max_message_size);
        config.max_frame_size = Some(self.config.max_message_size);
        config
    }
}

/// Send a close frame to the remote and drop the stream.
///
/// Tries to deliver the close code so the peer can log a specific reason for
/// the drop. Errors from the close path are swallowed — at this point the
/// connection is already doomed and the caller will propagate a richer
/// [`TransportError`].
async fn close_ws_with_code(ws: &mut WebSocketStream<TcpStream>, code: u16, reason: &str) {
    let close_frame = CloseFrame {
        code: CloseCode::from(code),
        reason: reason.to_string().into(),
    };
    let _ = ws.send(Message::Close(Some(close_frame))).await;
    let _ = ws.close(None).await;
}

/// Receive and parse a [`HelloEnvelope`] from a WebSocket stream, applying a
/// 5 second timeout.
///
/// Consumes frames from the stream until the first application-level message
/// (Text or Binary) arrives. Control frames (Ping, Pong, Close) are handled
/// transparently. Returns a specific `TransportError` for each failure mode
/// so the caller can emit the right close code.
async fn receive_hello(
    ws: &mut WebSocketStream<TcpStream>,
) -> Result<HelloEnvelope, TransportError> {
    let fut = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str::<HelloEnvelope>(&text)
                        .map_err(|e| TransportError::HelloMalformed(format!("parse hello: {e}")));
                }
                Some(Ok(Message::Binary(data))) => {
                    return serde_json::from_slice::<HelloEnvelope>(&data)
                        .map_err(|e| TransportError::HelloMalformed(format!("parse hello: {e}")));
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                Some(Ok(Message::Close(_))) => {
                    return Err(TransportError::HelloMalformed(
                        "peer closed connection before hello".to_string(),
                    ));
                }
                Some(Err(e)) => {
                    return Err(TransportError::HelloMalformed(format!(
                        "receive hello: {e}"
                    )));
                }
                None => {
                    return Err(TransportError::HelloMalformed(
                        "connection closed before hello".to_string(),
                    ));
                }
            }
        }
    };

    match tokio::time::timeout(HELLO_TIMEOUT, fut).await {
        Ok(inner) => inner,
        Err(_) => Err(TransportError::HelloTimeout),
    }
}

/// Validate a received hello against our own. Returns the remote identity
/// on success, or a classified error that maps to an RFC 017 close code.
fn validate_hello(
    remote: HelloEnvelope,
    local_app_id: &str,
) -> Result<PeerIdentity, TransportError> {
    if remote.kind != HelloKind::Hello {
        return Err(TransportError::HelloMalformed(format!(
            "unexpected hello kind: {:?}",
            remote.kind
        )));
    }
    if remote.version < HelloEnvelope::MIN_SUPPORTED_VERSION {
        return Err(TransportError::HelloMalformed(format!(
            "unsupported hello version {} (minimum supported: {})",
            remote.version,
            HelloEnvelope::MIN_SUPPORTED_VERSION
        )));
    }
    if remote.identity.app_id != local_app_id {
        return Err(TransportError::AppMismatch {
            local: local_app_id.to_string(),
            remote: remote.identity.app_id.clone(),
        });
    }
    Ok(remote.identity)
}

/// Serialise and send a [`HelloEnvelope`] as a text frame.
async fn send_hello(
    ws: &mut WebSocketStream<TcpStream>,
    hello: &HelloEnvelope,
) -> Result<(), TransportError> {
    let payload =
        serde_json::to_string(hello).map_err(|e| TransportError::Serialize(e.to_string()))?;
    ws.send(Message::Text(payload.into()))
        .await
        .map_err(|e| TransportError::HandshakeFailed(format!("send hello: {e}")))
}

/// Client-side hello exchange: send ours first, read theirs, validate, then
/// return the remote identity. On error, close the socket with the matching
/// RFC 017 close code before returning the error.
async fn client_hello_exchange(
    ws: &mut WebSocketStream<TcpStream>,
    local_hello: &HelloEnvelope,
) -> Result<PeerIdentity, TransportError> {
    // Step 1: send our hello first — this is envelope zero.
    if let Err(e) = send_hello(ws, local_hello).await {
        close_ws_with_code(ws, CLOSE_HELLO_PROTOCOL, "local send failed").await;
        return Err(e);
    }

    // Step 2: read remote hello (timed).
    let remote = match receive_hello(ws).await {
        Ok(envelope) => envelope,
        Err(e) => {
            match &e {
                TransportError::HelloTimeout => {
                    close_ws_with_code(ws, CLOSE_HELLO_PROTOCOL, "hello timeout").await;
                }
                TransportError::HelloMalformed(_) => {
                    close_ws_with_code(ws, CLOSE_HELLO_PROTOCOL, "malformed hello").await;
                }
                _ => {}
            }
            return Err(e);
        }
    };

    // Step 3: validate against our expectations.
    match validate_hello(remote, &local_hello.identity.app_id) {
        Ok(identity) => Ok(identity),
        Err(e) => {
            match &e {
                TransportError::AppMismatch { .. } => {
                    close_ws_with_code(ws, CLOSE_APP_MISMATCH, "app mismatch").await;
                }
                TransportError::HelloMalformed(_) => {
                    close_ws_with_code(ws, CLOSE_HELLO_PROTOCOL, "bad hello").await;
                }
                _ => {}
            }
            Err(e)
        }
    }
}

/// Server-side hello exchange: read theirs first, validate, then send ours.
/// On error, close the socket with the matching RFC 017 close code.
async fn server_hello_exchange(
    ws: &mut WebSocketStream<TcpStream>,
    local_hello: &HelloEnvelope,
) -> Result<PeerIdentity, TransportError> {
    // Step 1: read remote hello (timed).
    let remote = match receive_hello(ws).await {
        Ok(envelope) => envelope,
        Err(e) => {
            match &e {
                TransportError::HelloTimeout => {
                    close_ws_with_code(ws, CLOSE_HELLO_PROTOCOL, "hello timeout").await;
                }
                TransportError::HelloMalformed(_) => {
                    close_ws_with_code(ws, CLOSE_HELLO_PROTOCOL, "malformed hello").await;
                }
                _ => {}
            }
            return Err(e);
        }
    };

    // Step 2: validate against our expectations.
    let identity = match validate_hello(remote, &local_hello.identity.app_id) {
        Ok(identity) => identity,
        Err(e) => {
            match &e {
                TransportError::AppMismatch { .. } => {
                    close_ws_with_code(ws, CLOSE_APP_MISMATCH, "app mismatch").await;
                }
                TransportError::HelloMalformed(_) => {
                    close_ws_with_code(ws, CLOSE_HELLO_PROTOCOL, "bad hello").await;
                }
                _ => {}
            }
            return Err(e);
        }
    };

    // Step 3: reply with our hello.
    if let Err(e) = send_hello(ws, local_hello).await {
        close_ws_with_code(ws, CLOSE_HELLO_PROTOCOL, "local send failed").await;
        return Err(e);
    }

    Ok(identity)
}

impl<N: NetworkProvider + 'static> StreamTransport for WebSocketTransport<N> {
    type Stream = WsFramedStream;

    async fn connect(&self, addr: &PeerAddr) -> Result<Self::Stream, TransportError> {
        // Step 1: Dial TCP via Layer 3
        let dial_addr = resolve_dial_addr(addr);
        tracing::debug!(addr = %dial_addr, port = self.config.port, "ws: dialing peer");

        let tcp_stream = self
            .network
            .dial_tcp(&dial_addr, self.config.port)
            .await
            .map_err(|e| TransportError::ConnectFailed(format!("dial tcp: {e}")))?;

        // Step 2: WebSocket client upgrade (with max_message_size config)
        let ws_url = format!("ws://{dial_addr}:{}/ws", self.config.port);
        let ws_config = self.ws_protocol_config();
        let (mut ws, _response) =
            tokio_tungstenite::client_async_with_config(ws_url, tcp_stream, Some(ws_config))
                .await
                .map_err(|e| TransportError::ConnectFailed(format!("ws upgrade: {e}")))?;

        // Step 3: Hello exchange (RFC 017 §8)
        let local_hello = self.local_hello();
        let remote_identity = match client_hello_exchange(&mut ws, &local_hello).await {
            Ok(identity) => identity,
            Err(e) => {
                match &e {
                    TransportError::AppMismatch { local, remote } => {
                        tracing::info!(
                            local_app_id = %local,
                            remote_app_id = %remote,
                            "ws: closing connection — app_id mismatch"
                        );
                    }
                    TransportError::HelloTimeout => {
                        tracing::warn!("ws: hello timeout on outgoing connection");
                    }
                    TransportError::HelloMalformed(msg) => {
                        tracing::warn!(error = %msg, "ws: malformed hello on outgoing connection");
                    }
                    _ => {}
                }
                return Err(e);
            }
        };

        tracing::info!(
            remote_device_id = %remote_identity.device_id,
            remote_device_name = %remote_identity.device_name,
            remote_tailscale_id = %remote_identity.tailscale_id,
            "ws: connected (hello exchanged)"
        );

        // Step 4: Build framed stream with split read/write halves
        Ok(WsFramedStream::new(
            ws,
            remote_identity.tailscale_id.clone(),
            Some(remote_identity),
            dial_addr,
            self.config.ping_interval,
            self.config.pong_timeout,
        ))
    }

    async fn listen(&self) -> Result<StreamListener<Self::Stream>, TransportError> {
        let port = self.config.port;
        tracing::debug!(port, "ws: starting listener");

        // Step 1: Listen on TCP via Layer 3
        let mut tcp_listener = self
            .network
            .listen_tcp(port)
            .await
            .map_err(|e| TransportError::ListenFailed(format!("listen tcp: {e}")))?;

        // Step 2: Spawn accept loop that upgrades each connection to WS
        let (tx, rx) = tokio::sync::mpsc::channel::<WsFramedStream>(64);
        let local_hello = self.local_hello();
        let ping_interval = self.config.ping_interval;
        let pong_timeout = self.config.pong_timeout;
        let ws_config = self.ws_protocol_config();

        tokio::spawn(async move {
            loop {
                match tcp_listener.incoming.recv().await {
                    Some(incoming) => {
                        let tx = tx.clone();
                        let local_hello = local_hello.clone();
                        let remote_addr = incoming.remote_addr.clone();
                        let ws_config = ws_config;

                        tokio::spawn(async move {
                            // WS server upgrade (with max_message_size config)
                            let mut ws = match tokio_tungstenite::accept_async_with_config(
                                incoming.stream,
                                Some(ws_config),
                            )
                            .await
                            {
                                Ok(ws) => ws,
                                Err(e) => {
                                    tracing::warn!(
                                        remote = %remote_addr,
                                        "ws: upgrade failed: {e}"
                                    );
                                    return;
                                }
                            };

                            // Server-side hello exchange (with timeout)
                            let remote_identity = match server_hello_exchange(&mut ws, &local_hello)
                                .await
                            {
                                Ok(identity) => identity,
                                Err(e) => {
                                    match &e {
                                        TransportError::AppMismatch { local, remote } => {
                                            tracing::info!(
                                                remote_addr = %remote_addr,
                                                local_app_id = %local,
                                                remote_app_id = %remote,
                                                "ws: closing incoming connection — app_id mismatch"
                                            );
                                        }
                                        TransportError::HelloTimeout => {
                                            tracing::warn!(
                                                remote_addr = %remote_addr,
                                                "ws: hello timeout on incoming connection"
                                            );
                                        }
                                        TransportError::HelloMalformed(msg) => {
                                            tracing::warn!(
                                                remote_addr = %remote_addr,
                                                error = %msg,
                                                "ws: malformed hello on incoming connection"
                                            );
                                        }
                                        _ => {
                                            tracing::warn!(
                                                remote_addr = %remote_addr,
                                                "ws: hello exchange failed: {e}"
                                            );
                                        }
                                    }
                                    return;
                                }
                            };

                            tracing::info!(
                                remote_device_id = %remote_identity.device_id,
                                remote_device_name = %remote_identity.device_name,
                                remote_tailscale_id = %remote_identity.tailscale_id,
                                remote_addr = %remote_addr,
                                "ws: accepted connection (hello exchanged)"
                            );

                            let stream = WsFramedStream::new(
                                ws,
                                remote_identity.tailscale_id.clone(),
                                Some(remote_identity),
                                remote_addr,
                                ping_interval,
                                pong_timeout,
                            );

                            if tx.send(stream).await.is_err() {
                                tracing::debug!("ws: listener channel closed");
                            }
                        });
                    }
                    None => {
                        tracing::debug!("ws: tcp listener channel closed");
                        break;
                    }
                }
            }
        });

        Ok(StreamListener::new(rx, port))
    }
}

// ---------------------------------------------------------------------------
// WsFramedStream
// ---------------------------------------------------------------------------

/// A WebSocket-backed [`FramedStream`].
///
/// Uses split read/write halves to avoid mutex contention between the
/// heartbeat task and the main send/recv path:
///
/// - **Write half** (`SplitSink`): Shared via `Arc<Mutex<_>>` between
///   `send()` and the heartbeat task (which sends Ping frames).
/// - **Read half** (`SplitStream`): Owned exclusively by `recv()` — no
///   mutex needed.
/// - **Heartbeat**: Sends Ping on the write half at `ping_interval`. Tracks
///   last Pong via `Arc<AtomicU64>` (epoch millis). If `last_pong` exceeds
///   `pong_timeout`, the connection is closed.
pub struct WsFramedStream {
    /// Write half of the WebSocket stream, shared with heartbeat task.
    write: Arc<Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>>,
    /// Read half of the WebSocket stream, owned exclusively by recv().
    read: SplitStream<WebSocketStream<TcpStream>>,
    /// Remote peer's Tailscale stable ID — the session layer routes by this.
    /// Derived from the hello envelope's `identity.tailscale_id`.
    remote_peer_id: String,
    /// Remote peer identity from the hello envelope (RFC 017 §8).
    remote_identity: Option<PeerIdentity>,
    /// Remote address string.
    remote_addr: String,
    /// Handle to the heartbeat task (aborted on close/drop).
    heartbeat_handle: Option<tokio::task::JoinHandle<()>>,
    /// Timestamp (epoch millis) of the last received Pong.
    last_pong: Arc<AtomicU64>,
    /// Flag indicating the connection has been closed.
    closed: Arc<AtomicBool>,
}

impl std::fmt::Debug for WsFramedStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsFramedStream")
            .field("remote_peer_id", &self.remote_peer_id)
            .field("remote_addr", &self.remote_addr)
            .field("closed", &self.closed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

// SAFETY: All fields are Send. `SplitStream` is Send because
// `WebSocketStream<TcpStream>` is Send. The Arc-wrapped fields are Sync.
// We need the explicit Sync impl because `SplitStream` is not Sync,
// but `WsFramedStream` is only accessed via `&mut self` (exclusive ref)
// so Sync is safe.
unsafe impl Sync for WsFramedStream {}

/// Get the current epoch time in milliseconds.
fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl WsFramedStream {
    /// Create a new framed stream with split read/write halves and heartbeat.
    fn new(
        ws: WebSocketStream<TcpStream>,
        remote_peer_id: String,
        remote_identity: Option<PeerIdentity>,
        remote_addr: String,
        ping_interval: Duration,
        pong_timeout: Duration,
    ) -> Self {
        let (write, read) = ws.split();
        let write = Arc::new(Mutex::new(write));
        let last_pong = Arc::new(AtomicU64::new(epoch_millis()));
        let closed = Arc::new(AtomicBool::new(false));

        // Spawn heartbeat task
        let hb_write = write.clone();
        let hb_last_pong = last_pong.clone();
        let hb_closed = closed.clone();
        let hb_addr = remote_addr.clone();
        let heartbeat_handle = tokio::spawn(async move {
            heartbeat_loop(
                hb_write,
                hb_last_pong,
                hb_closed,
                ping_interval,
                pong_timeout,
                &hb_addr,
            )
            .await;
        });

        Self {
            write,
            read,
            remote_peer_id,
            remote_identity,
            remote_addr,
            heartbeat_handle: Some(heartbeat_handle),
            last_pong,
            closed,
        }
    }

    /// Get the remote peer's Tailscale stable ID (from the hello envelope).
    ///
    /// This is the session layer's routing key. For the application-visible
    /// device identifier, use [`remote_identity().device_id`](Self::remote_identity).
    pub fn remote_peer_id(&self) -> &str {
        &self.remote_peer_id
    }

    /// Get the full remote [`PeerIdentity`] advertised in the hello envelope.
    ///
    /// Returns `None` only in test fixtures that synthesise a stream without
    /// running the hello exchange — production streams always have this.
    pub fn remote_identity(&self) -> Option<&PeerIdentity> {
        self.remote_identity.as_ref()
    }
}

/// Background heartbeat loop that sends Ping frames and monitors Pong timestamps.
///
/// On each `ping_interval` tick:
/// 1. Check if `last_pong` is within `pong_timeout` — if not, close.
/// 2. Send a Ping frame on the write half.
///
/// This design avoids the old bug where `select!` between `ping_interval` (10s)
/// and `pong_timeout` (30s) always picked the shorter timer, making timeout
/// detection dead code.
async fn heartbeat_loop(
    write: Arc<Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>>,
    last_pong: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    ping_interval: Duration,
    pong_timeout: Duration,
    remote_addr: &str,
) {
    let mut interval = tokio::time::interval(ping_interval);
    // Skip the first immediate tick
    interval.tick().await;

    loop {
        interval.tick().await;

        // If the connection was closed externally, stop.
        if closed.load(Ordering::Acquire) {
            return;
        }

        // Check if last pong is within timeout
        let last = last_pong.load(Ordering::Acquire);
        let now = epoch_millis();
        let elapsed = Duration::from_millis(now.saturating_sub(last));

        if elapsed > pong_timeout {
            tracing::warn!(
                remote = %remote_addr,
                elapsed = ?elapsed,
                "heartbeat: pong timeout after {pong_timeout:?}"
            );
            // Close the connection
            closed.store(true, Ordering::Release);
            let mut w = write.lock().await;
            let _ = w.close().await;
            return;
        }

        // Send a ping
        {
            let mut w = write.lock().await;
            let ping_data = b"truffle-ping".to_vec();
            if let Err(e) = w.send(Message::Ping(ping_data.into())).await {
                tracing::debug!(remote = %remote_addr, "heartbeat: ping send failed: {e}");
                closed.store(true, Ordering::Release);
                return;
            }
        }
    }
}

impl FramedStream for WsFramedStream {
    async fn send(&mut self, data: &[u8]) -> Result<(), TransportError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(TransportError::ConnectionClosed(
                "connection already closed".to_string(),
            ));
        }
        let mut w = self.write.lock().await;
        w.send(Message::Binary(data.to_vec().into()))
            .await
            .map_err(|e| TransportError::WebSocket(format!("send: {e}")))
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(None);
        }
        loop {
            match self.read.next().await {
                Some(Ok(Message::Binary(data))) => return Ok(Some(data.to_vec())),
                Some(Ok(Message::Text(text))) => {
                    // Layer 4 treats text frames as binary data
                    return Ok(Some(text.as_bytes().to_vec()));
                }
                Some(Ok(Message::Ping(_))) => {
                    // We received a Ping — tungstenite auto-sends Pong at the
                    // protocol level. Just skip and continue reading.
                    continue;
                }
                Some(Ok(Message::Pong(_))) => {
                    // Update last_pong timestamp for the heartbeat checker
                    self.last_pong.store(epoch_millis(), Ordering::Release);
                    continue;
                }
                Some(Ok(Message::Close(_))) => {
                    self.closed.store(true, Ordering::Release);
                    return Ok(None);
                }
                Some(Ok(Message::Frame(_))) => {
                    // Raw frame — skip
                    continue;
                }
                Some(Err(e)) => {
                    self.closed.store(true, Ordering::Release);
                    return Err(TransportError::WebSocket(format!("recv: {e}")));
                }
                None => {
                    self.closed.store(true, Ordering::Release);
                    return Ok(None);
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        // Abort heartbeat task
        if let Some(handle) = self.heartbeat_handle.take() {
            handle.abort();
        }

        self.closed.store(true, Ordering::Release);

        let mut w = self.write.lock().await;
        w.close()
            .await
            .map_err(|e| TransportError::WebSocket(format!("close: {e}")))
    }

    fn peer_addr(&self) -> String {
        self.remote_addr.clone()
    }
}

impl Drop for WsFramedStream {
    fn drop(&mut self) {
        // Ensure heartbeat task is cleaned up
        if let Some(handle) = self.heartbeat_handle.take() {
            handle.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn resolve_dial_addr_prefers_ip() {
        let addr = PeerAddr {
            ip: Some("100.64.0.1".parse().unwrap()),
            hostname: "peer".to_string(),
            dns_name: Some("peer.tailnet.ts.net".to_string()),
        };
        assert_eq!(resolve_dial_addr(&addr), "100.64.0.1");
    }

    #[test]
    fn resolve_dial_addr_falls_back_to_dns() {
        let addr = PeerAddr {
            ip: None,
            hostname: "peer".to_string(),
            dns_name: Some("peer.tailnet.ts.net".to_string()),
        };
        assert_eq!(resolve_dial_addr(&addr), "peer.tailnet.ts.net");
    }

    #[test]
    fn resolve_dial_addr_falls_back_to_hostname() {
        let addr = PeerAddr {
            ip: None,
            hostname: "peer".to_string(),
            dns_name: None,
        };
        assert_eq!(resolve_dial_addr(&addr), "peer");
    }
}
