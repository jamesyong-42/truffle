//! UDP transport — stub implementation.
//!
//! UDP will implement [`DatagramTransport`] in a future phase, providing
//! unreliable datagram delivery for real-time use cases (video, audio,
//! game state synchronization).
//!
//! This stub exists so that the transport module structure is complete.
//! All methods return [`TransportError::NotImplemented`].

use std::sync::Arc;

use crate::network::NetworkProvider;

use super::{DatagramSocket, DatagramTransport, TransportError};

// ---------------------------------------------------------------------------
// UdpTransport (stub)
// ---------------------------------------------------------------------------

/// UDP transport stub.
///
/// Will implement unreliable datagram delivery in a future phase.
/// Currently all methods return [`TransportError::NotImplemented`].
pub struct UdpTransport<N: NetworkProvider> {
    /// Layer 3 network provider (unused until implementation).
    #[allow(dead_code)]
    network: Arc<N>,
}

impl<N: NetworkProvider + 'static> UdpTransport<N> {
    /// Create a new UDP transport stub.
    pub fn new(network: Arc<N>) -> Self {
        Self { network }
    }
}

impl<N: NetworkProvider + 'static> DatagramTransport for UdpTransport<N> {
    async fn bind(&self, _port: u16) -> Result<DatagramSocket, TransportError> {
        Err(TransportError::NotImplemented(
            "UDP transport not yet implemented".to_string(),
        ))
    }
}
