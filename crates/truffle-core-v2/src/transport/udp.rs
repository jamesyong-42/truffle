//! UDP transport — [`DatagramTransport`] implementation.
//!
//! Provides unreliable datagram delivery for real-time use cases: video/audio
//! streaming, game state synchronization, and any application where low latency
//! matters more than guaranteed delivery.
//!
//! # Binding
//!
//! `UdpTransport::bind(port)` creates a [`DatagramSocket`] bound to
//! `0.0.0.0:{port}`. When `port` is 0 the OS assigns an ephemeral port.
//!
//! # TODO
//!
//! Once `NetworkProvider::bind_udp` is implemented for `TailscaleProvider`,
//! this transport should delegate to it instead of binding directly via
//! `tokio::net::UdpSocket`. For now, binding to `0.0.0.0` works for both
//! loopback testing and local-network scenarios.

use std::sync::Arc;

use crate::network::NetworkProvider;

use super::{DatagramSocket, DatagramTransport, TransportError};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for UDP transport.
#[derive(Debug, Clone)]
pub struct UdpConfig {
    /// Port to bind on. Default: 0 (OS-assigned ephemeral port).
    pub port: u16,
}

impl Default for UdpConfig {
    fn default() -> Self {
        Self { port: 0 }
    }
}

// ---------------------------------------------------------------------------
// UdpTransport
// ---------------------------------------------------------------------------

/// UDP [`DatagramTransport`] implementation.
///
/// Generic over the [`NetworkProvider`] type `N`. Currently binds directly
/// via `tokio::net::UdpSocket` since most `NetworkProvider` implementations
/// do not yet support `bind_udp`.
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
/// use truffle_core_v2::transport::udp::{UdpTransport, UdpConfig};
///
/// let udp = UdpTransport::new(Arc::new(provider), UdpConfig::default());
/// let socket = udp.bind(0).await?;
/// socket.send_to(b"ping", "127.0.0.1:9000").await?;
/// ```
pub struct UdpTransport<N: NetworkProvider> {
    /// Layer 3 network provider.
    ///
    /// TODO: Use `network.bind_udp(port)` once TailscaleProvider supports it.
    /// For now we bind directly to 0.0.0.0.
    #[allow(dead_code)]
    network: Arc<N>,
    /// UDP configuration.
    #[allow(dead_code)]
    config: UdpConfig,
}

impl<N: NetworkProvider + 'static> UdpTransport<N> {
    /// Create a new UDP transport backed by the given network provider.
    pub fn new(network: Arc<N>, config: UdpConfig) -> Self {
        Self { network, config }
    }
}

impl<N: NetworkProvider + 'static> DatagramTransport for UdpTransport<N> {
    async fn bind(&self, port: u16) -> Result<DatagramSocket, TransportError> {
        let bind_addr = format!("0.0.0.0:{port}");
        tracing::debug!(addr = %bind_addr, "udp: binding socket");

        // TODO: Use self.network.bind_udp(port) when TailscaleProvider supports it.
        // For now, bind directly via tokio. This works for loopback and LAN.
        let socket = tokio::net::UdpSocket::bind(&bind_addr)
            .await
            .map_err(|e| TransportError::ListenFailed(format!("udp bind {bind_addr}: {e}")))?;

        let local_addr = socket
            .local_addr()
            .map_err(|e| TransportError::Io(e))?;
        tracing::debug!(local_addr = %local_addr, "udp: socket bound");

        Ok(DatagramSocket { socket })
    }
}
