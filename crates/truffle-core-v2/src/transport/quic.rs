//! QUIC transport — [`StreamTransport`] implementation over QUIC.
//!
//! Uses the `quinn` crate for the QUIC protocol, with self-signed certificates
//! generated at runtime via `rcgen`. Since truffle runs over Tailscale
//! (which already provides WireGuard encryption), the QUIC TLS layer is
//! redundant but required by the QUIC specification.
//!
//! # Connection flow
//!
//! **Client (connect)**:
//! 1. Create a QUIC client endpoint with a permissive TLS verifier
//! 2. `endpoint.connect(addr, "truffle")` to establish a QUIC connection
//! 3. Open a bidirectional stream for the handshake
//! 4. Exchange [`Handshake`] messages (JSON, same as WebSocket transport)
//! 5. Return a [`QuicFramedStream`] wrapping the QUIC bidirectional stream
//!
//! **Server (listen)**:
//! 1. Create a QUIC server endpoint with a self-signed certificate
//! 2. Accept incoming QUIC connections
//! 3. For each: accept the first bidirectional stream, perform handshake
//! 4. Yield [`QuicFramedStream`]s via [`StreamListener`]
//!
//! # Framing
//!
//! [`QuicFramedStream`] uses length-prefixed framing on top of QUIC's
//! reliable ordered stream:
//! - **send**: 4-byte big-endian length prefix + payload
//! - **recv**: read 4-byte length prefix, then read that many bytes
//!
//! This is simpler than WebSocket framing and well-suited for binary data.
//!
//! # tsnet support
//!
//! When connected to a [`NetworkProvider`] that supports UDP (e.g.,
//! [`TailscaleProvider`](crate::network::tailscale::TailscaleProvider)),
//! the transport uses [`TsnetUdpSocket`](super::quic_socket::TsnetUdpSocket)
//! to route QUIC datagrams through the tsnet relay instead of creating
//! a direct host-network socket. This enables QUIC over userspace Tailscale
//! (tsnet) where the host UDP socket cannot reach peers.
//!
//! When no network provider UDP support is available (e.g., loopback tests
//! with a mock provider), the transport falls back to quinn's default
//! host-network sockets.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use quinn::{RecvStream, SendStream};

use crate::network::{NetworkProvider, PeerAddr};

use super::{
    resolve_dial_addr, FramedStream, Handshake, StreamListener, StreamTransport, TransportError,
    PROTOCOL_VERSION,
};
use super::quic_socket::TsnetUdpSocket;

/// Handshake timeout — maximum time to wait for QUIC handshake exchange.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum framed message size (64 MiB). Protects against malicious/corrupt length prefixes.
const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for QUIC transport.
#[derive(Debug, Clone)]
pub struct QuicConfig {
    /// Port to listen on for incoming QUIC connections. Default: 9420.
    pub port: u16,
    /// Maximum number of concurrent bidirectional streams per connection.
    /// Default: 100.
    pub max_streams: u32,
}

impl Default for QuicConfig {
    fn default() -> Self {
        Self {
            port: 9420,
            max_streams: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// TLS helpers
// ---------------------------------------------------------------------------

/// Generate a self-signed certificate and private key for QUIC TLS.
///
/// Since Tailscale already encrypts all traffic via WireGuard, this TLS layer
/// is redundant but required by the QUIC protocol specification.
fn generate_self_signed_cert() -> Result<(rustls::pki_types::CertificateDer<'static>, rustls::pki_types::PrivateKeyDer<'static>), TransportError> {
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["truffle".to_string(), "localhost".to_string()])
            .map_err(|e| TransportError::ConnectFailed(format!("generate self-signed cert: {e}")))?;

    let cert_der = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(key_pair.serialize_der())
        .map_err(|e| TransportError::ConnectFailed(format!("serialize private key: {e}")))?;

    Ok((cert_der, key_der))
}

/// Build a `quinn::ServerConfig` with a self-signed certificate.
fn build_server_config(config: &QuicConfig) -> Result<quinn::ServerConfig, TransportError> {
    let (cert_der, key_der) = generate_self_signed_cert()?;

    let mut tls_config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| TransportError::ListenFailed(format!("rustls protocol versions: {e}")))?
    .with_no_client_auth()
    .with_single_cert(vec![cert_der], key_der)
    .map_err(|e| TransportError::ListenFailed(format!("rustls server config: {e}")))?;

    tls_config.alpn_protocols = vec![b"truffle".to_vec()];

    let mut transport_config = quinn::TransportConfig::default();
    transport_config.max_concurrent_bidi_streams(config.max_streams.into());
    transport_config.max_concurrent_uni_streams(0u32.into());

    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)
            .map_err(|e| TransportError::ListenFailed(format!("quic server config: {e}")))?,
    ));
    server_config.transport_config(Arc::new(transport_config));

    Ok(server_config)
}

/// Build a `quinn::ClientConfig` that skips server certificate verification.
///
/// This is safe because Tailscale already provides mutual authentication
/// and encryption via WireGuard. The QUIC TLS is purely for protocol compliance.
fn build_client_config() -> Result<quinn::ClientConfig, TransportError> {
    let mut tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| TransportError::ConnectFailed(format!("rustls protocol versions: {e}")))?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
    .with_no_client_auth();

    // Must match the server's ALPN protocol, otherwise the TLS handshake fails.
    tls_config.alpn_protocols = vec![b"truffle".to_vec()];

    let mut client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
            .map_err(|e| TransportError::ConnectFailed(format!("quic client config: {e}")))?,
    ));

    let mut transport_config = quinn::TransportConfig::default();
    transport_config.max_concurrent_bidi_streams(100u32.into());
    transport_config.max_concurrent_uni_streams(0u32.into());
    // Keep-alive to prevent idle timeouts during tests and real usage
    transport_config.keep_alive_interval(Some(Duration::from_secs(5)));
    client_config.transport_config(Arc::new(transport_config));

    Ok(client_config)
}

/// A `rustls` certificate verifier that accepts any server certificate.
///
/// QUIC mandates TLS 1.3, but since Tailscale already provides encryption
/// and authentication, we skip verification here.
#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ---------------------------------------------------------------------------
// QuicTransport
// ---------------------------------------------------------------------------

/// QUIC-based [`StreamTransport`] implementation.
///
/// Generic over the [`NetworkProvider`] type `N`. Provides multiplexed
/// bidirectional streams over a single QUIC connection.
///
/// QUIC is ideal for scenarios requiring multiple concurrent streams
/// without head-of-line blocking (e.g., video streaming via fondue).
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
/// use truffle_core_v2::transport::quic::{QuicTransport, QuicConfig};
///
/// let quic = QuicTransport::new(Arc::new(provider), QuicConfig::default());
/// let stream = quic.connect(&peer_addr).await?;
/// ```
pub struct QuicTransport<N: NetworkProvider> {
    /// Layer 3 network provider (used for identity in handshake).
    network: Arc<N>,
    /// QUIC configuration.
    config: QuicConfig,
}

impl<N: NetworkProvider + 'static> QuicTransport<N> {
    /// Create a new QUIC transport.
    ///
    /// - `network`: An `Arc<N>` where `N: NetworkProvider`.
    /// - `config`: QUIC configuration (port, max streams, etc.).
    pub fn new(network: Arc<N>, config: QuicConfig) -> Self {
        Self { network, config }
    }

    /// Build the local handshake message using the network provider's identity.
    fn local_handshake(&self) -> Handshake {
        let identity = self.network.local_identity();
        Handshake {
            peer_id: identity.id,
            capabilities: vec!["quic".to_string(), "binary".to_string()],
            protocol_version: PROTOCOL_VERSION,
        }
    }

    /// Try to create a QUIC client endpoint backed by a [`TsnetUdpSocket`].
    ///
    /// Binds a UDP socket through the network provider and wraps it in a
    /// `TsnetUdpSocket` for quinn. Returns the configured `Endpoint`.
    ///
    /// Returns `Err` if the network provider does not support UDP
    /// (e.g., mock providers in tests).
    async fn try_create_tsnet_client_endpoint(
        &self,
        client_config: &quinn::ClientConfig,
    ) -> Result<quinn::Endpoint, TransportError> {
        // Bind on port 0 (ephemeral) for the client
        let net_socket = self.network.bind_udp(0).await.map_err(|e| {
            TransportError::ConnectFailed(format!("bind_udp for tsnet client: {e}"))
        })?;

        let tsnet_port = net_socket.tsnet_port();
        let local_ip = self.network.local_addr().ip.unwrap_or_else(|| {
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        });
        let tsnet_addr = SocketAddr::new(local_ip, tsnet_port);

        let tsnet_socket = Arc::new(TsnetUdpSocket::new(net_socket, tsnet_addr));

        let runtime = quinn::default_runtime().ok_or_else(|| {
            TransportError::ConnectFailed("no async runtime available for quinn".to_string())
        })?;

        let mut endpoint = quinn::Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            None,
            tsnet_socket,
            runtime,
        )
        .map_err(|e| TransportError::ConnectFailed(format!("tsnet quic endpoint: {e}")))?;

        endpoint.set_default_client_config(client_config.clone());

        tracing::debug!(
            tsnet_port,
            tsnet_addr = %tsnet_addr,
            "quic: created tsnet client endpoint"
        );

        Ok(endpoint)
    }

    /// Try to create a QUIC server endpoint backed by a [`TsnetUdpSocket`].
    ///
    /// Binds a UDP socket on the configured port through the network provider
    /// and wraps it in a `TsnetUdpSocket` for quinn.
    ///
    /// Returns `(Endpoint, actual_port)` or `Err` if the provider does not
    /// support UDP.
    async fn try_create_tsnet_server_endpoint(
        &self,
        server_config: &quinn::ServerConfig,
    ) -> Result<(quinn::Endpoint, u16), TransportError> {
        let port = self.config.port;
        let net_socket = self.network.bind_udp(port).await.map_err(|e| {
            TransportError::ListenFailed(format!("bind_udp for tsnet server: {e}"))
        })?;

        let tsnet_port = net_socket.tsnet_port();
        let local_ip = self.network.local_addr().ip.unwrap_or_else(|| {
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        });
        let tsnet_addr = SocketAddr::new(local_ip, tsnet_port);

        let tsnet_socket = Arc::new(TsnetUdpSocket::new(net_socket, tsnet_addr));

        let runtime = quinn::default_runtime().ok_or_else(|| {
            TransportError::ListenFailed("no async runtime available for quinn".to_string())
        })?;

        let endpoint = quinn::Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            Some(server_config.clone()),
            tsnet_socket,
            runtime,
        )
        .map_err(|e| TransportError::ListenFailed(format!("tsnet quic endpoint: {e}")))?;

        tracing::debug!(
            tsnet_port,
            tsnet_addr = %tsnet_addr,
            "quic: created tsnet server endpoint"
        );

        Ok((endpoint, tsnet_port))
    }
}

impl<N: NetworkProvider + 'static> StreamTransport for QuicTransport<N> {
    type Stream = QuicFramedStream;

    async fn connect(&self, addr: &PeerAddr) -> Result<Self::Stream, TransportError> {
        let dial_addr = resolve_dial_addr(addr);
        tracing::debug!(addr = %dial_addr, port = self.config.port, "quic: dialing peer");

        // Step 1: Create a QUIC client endpoint.
        // Try tsnet first (network provider UDP), fall back to direct socket.
        let client_config = build_client_config()?;

        let endpoint = match self.try_create_tsnet_client_endpoint(&client_config).await {
            Ok(ep) => {
                tracing::info!("quic: using TsnetUdpSocket for client endpoint");
                ep
            }
            Err(tsnet_err) => {
                tracing::debug!(
                    err = %tsnet_err,
                    "quic: tsnet client endpoint unavailable, falling back to direct socket"
                );
                let mut ep = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())
                    .map_err(|e| TransportError::ConnectFailed(format!("create quic endpoint: {e}")))?;
                ep.set_default_client_config(client_config);
                ep
            }
        };

        // Step 2: Connect to the peer
        let remote_addr: SocketAddr = format!("{dial_addr}:{}", self.config.port)
            .parse()
            .map_err(|e| TransportError::ConnectFailed(format!("parse address: {e}")))?;

        let connection = endpoint
            .connect(remote_addr, "truffle")
            .map_err(|e| TransportError::ConnectFailed(format!("quic connect: {e}")))?
            .await
            .map_err(|e| TransportError::ConnectFailed(format!("quic handshake: {e}")))?;

        tracing::debug!(
            remote = %connection.remote_address(),
            "quic: connection established"
        );

        // Step 3: Open a bidirectional stream for the transport handshake
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| TransportError::ConnectFailed(format!("open bi stream: {e}")))?;

        let mut stream = QuicFramedStream::new(send, recv, connection.remote_address().to_string());

        // Step 4: Exchange handshake (with timeout)
        let local_hs = self.local_handshake();
        let remote_hs = tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            client_handshake(&mut stream, &local_hs),
        )
        .await
        .map_err(|_| TransportError::Timeout("quic handshake timed out".to_string()))??;

        // Store remote peer info
        stream.remote_peer_id = remote_hs.peer_id.clone();

        tracing::info!(
            remote_peer = %remote_hs.peer_id,
            remote_version = remote_hs.protocol_version,
            "quic: connected"
        );

        Ok(stream)
    }

    async fn listen(&self) -> Result<StreamListener<Self::Stream>, TransportError> {
        let port = self.config.port;
        tracing::debug!(port, "quic: starting listener");

        // Step 1: Build server config with self-signed cert
        let server_config = build_server_config(&self.config)?;

        // Step 2: Create server endpoint.
        // Try tsnet first (network provider UDP), fall back to direct socket.
        let (endpoint, actual_port) = match self.try_create_tsnet_server_endpoint(&server_config).await {
            Ok((ep, p)) => {
                tracing::info!(port = p, "quic: using TsnetUdpSocket for server endpoint");
                (ep, p)
            }
            Err(tsnet_err) => {
                tracing::debug!(
                    err = %tsnet_err,
                    "quic: tsnet server endpoint unavailable, falling back to direct socket"
                );
                let bind_addr: SocketAddr = format!("0.0.0.0:{port}")
                    .parse()
                    .map_err(|e| TransportError::ListenFailed(format!("parse bind address: {e}")))?;

                let ep = quinn::Endpoint::server(server_config, bind_addr)
                    .map_err(|e| TransportError::ListenFailed(format!("quic listen: {e}")))?;

                let actual = ep
                    .local_addr()
                    .map_err(|e| TransportError::ListenFailed(format!("local addr: {e}")))?
                    .port();

                (ep, actual)
            }
        };

        tracing::debug!(actual_port, "quic: listener bound");

        // Step 3: Spawn accept loop
        let (tx, rx) = tokio::sync::mpsc::channel::<QuicFramedStream>(64);
        let local_hs = self.local_handshake();

        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let tx = tx.clone();
                let local_hs = local_hs.clone();

                tokio::spawn(async move {
                    // Accept the QUIC connection
                    let connection = match incoming.await {
                        Ok(conn) => conn,
                        Err(e) => {
                            tracing::warn!("quic: accept connection failed: {e}");
                            return;
                        }
                    };

                    let remote_addr = connection.remote_address().to_string();
                    tracing::debug!(remote = %remote_addr, "quic: accepted connection");

                    // Accept the first bidirectional stream (handshake stream)
                    let (send, recv) = match connection.accept_bi().await {
                        Ok(streams) => streams,
                        Err(e) => {
                            tracing::warn!(
                                remote = %remote_addr,
                                "quic: accept bi stream failed: {e}"
                            );
                            return;
                        }
                    };

                    let mut stream = QuicFramedStream::new(send, recv, remote_addr.clone());

                    // Server-side handshake (with timeout)
                    let remote_hs = match tokio::time::timeout(
                        HANDSHAKE_TIMEOUT,
                        server_handshake(&mut stream, &local_hs),
                    )
                    .await
                    {
                        Ok(Ok(hs)) => hs,
                        Ok(Err(e)) => {
                            tracing::warn!(
                                remote = %remote_addr,
                                "quic: handshake failed: {e}"
                            );
                            return;
                        }
                        Err(_) => {
                            tracing::warn!(
                                remote = %remote_addr,
                                "quic: handshake timed out"
                            );
                            return;
                        }
                    };

                    stream.remote_peer_id = remote_hs.peer_id.clone();

                    tracing::info!(
                        remote_peer = %remote_hs.peer_id,
                        remote_addr = %remote_addr,
                        "quic: accepted connection"
                    );

                    if tx.send(stream).await.is_err() {
                        tracing::debug!("quic: listener channel closed");
                    }
                });
            }

            tracing::debug!("quic: endpoint accept loop ended");
        });

        Ok(StreamListener::new(rx, actual_port))
    }
}

// ---------------------------------------------------------------------------
// Handshake helpers
// ---------------------------------------------------------------------------

/// Client-side handshake: send ours, receive theirs.
async fn client_handshake(
    stream: &mut QuicFramedStream,
    local_hs: &Handshake,
) -> Result<Handshake, TransportError> {
    // Send our handshake
    let hs_json = serde_json::to_vec(local_hs)
        .map_err(|e| TransportError::Serialize(e.to_string()))?;
    stream.send(&hs_json).await?;

    // Receive peer's handshake
    let remote_data = stream
        .recv()
        .await?
        .ok_or_else(|| TransportError::HandshakeFailed("connection closed before handshake".to_string()))?;

    let remote_hs: Handshake = serde_json::from_slice(&remote_data)
        .map_err(|e| TransportError::HandshakeFailed(format!("parse handshake: {e}")))?;

    // Validate protocol version
    if remote_hs.protocol_version != PROTOCOL_VERSION {
        return Err(TransportError::VersionMismatch {
            local: PROTOCOL_VERSION,
            remote: remote_hs.protocol_version,
        });
    }

    Ok(remote_hs)
}

/// Server-side handshake: receive theirs, send ours.
async fn server_handshake(
    stream: &mut QuicFramedStream,
    local_hs: &Handshake,
) -> Result<Handshake, TransportError> {
    // Receive peer's handshake first
    let remote_data = stream
        .recv()
        .await?
        .ok_or_else(|| TransportError::HandshakeFailed("connection closed before handshake".to_string()))?;

    let remote_hs: Handshake = serde_json::from_slice(&remote_data)
        .map_err(|e| TransportError::HandshakeFailed(format!("parse handshake: {e}")))?;

    // Validate protocol version
    if remote_hs.protocol_version != PROTOCOL_VERSION {
        return Err(TransportError::VersionMismatch {
            local: PROTOCOL_VERSION,
            remote: remote_hs.protocol_version,
        });
    }

    // Send our handshake
    let hs_json = serde_json::to_vec(local_hs)
        .map_err(|e| TransportError::Serialize(e.to_string()))?;
    stream.send(&hs_json).await?;

    Ok(remote_hs)
}

// ---------------------------------------------------------------------------
// QuicFramedStream
// ---------------------------------------------------------------------------

/// A QUIC-backed [`FramedStream`].
///
/// Uses length-prefixed framing (4-byte big-endian length + payload) on top
/// of a QUIC bidirectional stream. Each `send()` writes one framed message;
/// each `recv()` reads one framed message.
///
/// Unlike the WebSocket transport, there is no heartbeat — QUIC handles
/// keep-alive and congestion control natively.
pub struct QuicFramedStream {
    /// QUIC send stream (write half).
    send: SendStream,
    /// QUIC receive stream (read half).
    recv: RecvStream,
    /// Remote peer ID (from handshake). Empty until handshake completes.
    remote_peer_id: String,
    /// Remote address string.
    remote_addr: String,
    /// Flag indicating the stream has been closed.
    closed: Arc<AtomicBool>,
}

impl std::fmt::Debug for QuicFramedStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicFramedStream")
            .field("remote_peer_id", &self.remote_peer_id)
            .field("remote_addr", &self.remote_addr)
            .field("closed", &self.closed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

// SAFETY: QuicFramedStream is only accessed via &mut self (exclusive reference).
// SendStream and RecvStream are Send but not Sync; since we never share
// references across threads (only &mut self access), this is safe.
unsafe impl Sync for QuicFramedStream {}

impl QuicFramedStream {
    /// Create a new QUIC framed stream from quinn stream halves.
    fn new(send: SendStream, recv: RecvStream, remote_addr: String) -> Self {
        Self {
            send,
            recv,
            remote_peer_id: String::new(),
            remote_addr,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get the remote peer ID (from the transport handshake).
    pub fn remote_peer_id(&self) -> &str {
        &self.remote_peer_id
    }
}

impl FramedStream for QuicFramedStream {
    async fn send(&mut self, data: &[u8]) -> Result<(), TransportError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(TransportError::ConnectionClosed(
                "quic stream already closed".to_string(),
            ));
        }

        // Length-prefix framing: 4-byte BE length + payload
        let len = data.len() as u32;
        let len_bytes = len.to_be_bytes();

        self.send
            .write_all(&len_bytes)
            .await
            .map_err(|e| TransportError::Io(e.into()))?;

        self.send
            .write_all(data)
            .await
            .map_err(|e| TransportError::Io(e.into()))?;

        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(None);
        }

        // Read 4-byte length prefix
        let mut len_buf = [0u8; 4];
        match self.recv.read_exact(&mut len_buf).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(_)) => {
                // Clean stream finish — the peer closed the send side
                self.closed.store(true, Ordering::Release);
                return Ok(None);
            }
            Err(quinn::ReadExactError::ReadError(e)) => {
                self.closed.store(true, Ordering::Release);
                return Err(TransportError::ConnectionClosed(format!("quic read: {e}")));
            }
        }

        let len = u32::from_be_bytes(len_buf) as usize;

        // Sanity check: reject absurdly large messages
        if len > MAX_MESSAGE_SIZE {
            self.closed.store(true, Ordering::Release);
            return Err(TransportError::ConnectionClosed(format!(
                "message too large: {len} bytes (max {MAX_MESSAGE_SIZE})"
            )));
        }

        // Read the payload
        let mut buf = vec![0u8; len];
        match self.recv.read_exact(&mut buf).await {
            Ok(()) => Ok(Some(buf)),
            Err(quinn::ReadExactError::FinishedEarly(_)) => {
                self.closed.store(true, Ordering::Release);
                Ok(None)
            }
            Err(quinn::ReadExactError::ReadError(e)) => {
                self.closed.store(true, Ordering::Release);
                Err(TransportError::ConnectionClosed(format!("quic read: {e}")))
            }
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(()); // Already closed
        }

        // Finish the send stream (signals clean close to the peer)
        self.send
            .finish()
            .map_err(|e| TransportError::ConnectionClosed(format!("quic finish: {e}")))?;

        Ok(())
    }

    fn peer_addr(&self) -> String {
        self.remote_addr.clone()
    }
}
