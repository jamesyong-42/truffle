//! TCP transport — [`RawTransport`] implementation over plain TCP.
//!
//! This is a thin wrapper around Layer 3's [`NetworkProvider`] that provides
//! the [`RawTransport`] trait boundary so that Layer 5 and Layer 7 code
//! does not depend on `NetworkProvider` directly.
//!
//! # Connection flow
//!
//! - `open(addr, port)` → delegates to `NetworkProvider::dial_tcp(addr, port)`
//! - `listen(port)` → delegates to `NetworkProvider::listen_tcp(port)` and
//!   wraps incoming connections as a [`RawListener`]
//!
//! No protocol upgrade, no framing, no handshake. The caller gets a raw
//! `TcpStream` for byte-oriented I/O.

use std::sync::Arc;

use crate::network::{NetworkProvider, PeerAddr};

use super::{RawIncoming, RawListener, RawTransport, TransportError};

// ---------------------------------------------------------------------------
// TcpTransport
// ---------------------------------------------------------------------------

/// Plain TCP [`RawTransport`] implementation.
///
/// Generic over the [`NetworkProvider`] type `N`. Delegates all connectivity
/// to Layer 3. This struct exists to provide the trait boundary — Layer 5/7
/// code depends on `RawTransport`, not `NetworkProvider`.
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
/// use truffle_core_v2::transport::tcp::TcpTransport;
///
/// let tcp = TcpTransport::new(Arc::new(provider));
/// let stream = tcp.open(&peer_addr, 8080).await?;
/// ```
pub struct TcpTransport<N: NetworkProvider> {
    /// Layer 3 network provider.
    network: Arc<N>,
}

impl<N: NetworkProvider + 'static> TcpTransport<N> {
    /// Create a new TCP transport backed by the given network provider.
    pub fn new(network: Arc<N>) -> Self {
        Self { network }
    }
}

impl<N: NetworkProvider + 'static> RawTransport for TcpTransport<N> {
    async fn open(
        &self,
        addr: &PeerAddr,
        port: u16,
    ) -> Result<tokio::net::TcpStream, TransportError> {
        let dial_addr = resolve_dial_addr(addr);
        tracing::debug!(addr = %dial_addr, port, "tcp: dialing peer");

        let stream = self
            .network
            .dial_tcp(&dial_addr, port)
            .await
            .map_err(|e| TransportError::ConnectFailed(format!("tcp dial: {e}")))?;

        tracing::debug!(addr = %dial_addr, port, "tcp: connected");
        Ok(stream)
    }

    async fn listen(&self, port: u16) -> Result<RawListener, TransportError> {
        tracing::debug!(port, "tcp: starting listener");

        let mut tcp_listener = self
            .network
            .listen_tcp(port)
            .await
            .map_err(|e| TransportError::ListenFailed(format!("tcp listen: {e}")))?;

        // Use the actual port from the NetworkTcpListener (important when
        // the requested port is 0 and the OS assigns an ephemeral port).
        let actual_port = tcp_listener.port;

        // Spawn a task that forwards incoming connections from the
        // NetworkTcpListener channel to the RawListener channel.
        let (tx, rx) = tokio::sync::mpsc::channel::<RawIncoming>(64);

        tokio::spawn(async move {
            loop {
                match tcp_listener.incoming.recv().await {
                    Some(incoming) => {
                        let raw = RawIncoming {
                            stream: incoming.stream,
                            remote_addr: incoming.remote_addr,
                        };
                        if tx.send(raw).await.is_err() {
                            tracing::debug!("tcp: listener channel closed");
                            break;
                        }
                    }
                    None => {
                        tracing::debug!("tcp: network listener channel closed");
                        break;
                    }
                }
            }
        });

        Ok(RawListener::new(rx, actual_port))
    }
}

/// Resolve the best dial address from a [`PeerAddr`].
///
/// Prefers IP address, falls back to DNS name, then hostname.
fn resolve_dial_addr(addr: &PeerAddr) -> String {
    if let Some(ip) = &addr.ip {
        ip.to_string()
    } else if let Some(dns) = &addr.dns_name {
        dns.clone()
    } else {
        addr.hostname.clone()
    }
}
