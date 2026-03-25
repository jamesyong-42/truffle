//! QUIC transport — stub implementation.
//!
//! QUIC will implement both [`StreamTransport`] and [`RawTransport`] in a
//! future phase, providing multiplexed streams and datagrams over UDP.
//!
//! This stub exists so that the transport module structure is complete and
//! Layer 5/7 code can reference the type. All methods return
//! [`TransportError::NotImplemented`].

use std::sync::Arc;

use crate::network::{NetworkProvider, PeerAddr};

use super::{
    FramedStream, RawListener, RawTransport, StreamListener, StreamTransport, TransportError,
};

// ---------------------------------------------------------------------------
// QuicTransport (stub)
// ---------------------------------------------------------------------------

/// QUIC transport stub.
///
/// Will implement multiplexed streams over QUIC in a future phase.
/// Currently all methods return [`TransportError::NotImplemented`].
pub struct QuicTransport<N: NetworkProvider> {
    /// Layer 3 network provider (unused until implementation).
    #[allow(dead_code)]
    network: Arc<N>,
}

impl<N: NetworkProvider + 'static> QuicTransport<N> {
    /// Create a new QUIC transport stub.
    pub fn new(network: Arc<N>) -> Self {
        Self { network }
    }
}

/// Placeholder framed stream for QUIC.
///
/// This type exists only to satisfy the `StreamTransport::Stream` associated
/// type. It cannot be constructed outside this module.
pub struct QuicFramedStream {
    /// Unreachable — exists only for type satisfaction.
    _private: (),
}

impl FramedStream for QuicFramedStream {
    async fn send(&mut self, _data: &[u8]) -> Result<(), TransportError> {
        Err(TransportError::NotImplemented("QUIC send".to_string()))
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        Err(TransportError::NotImplemented("QUIC recv".to_string()))
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        Err(TransportError::NotImplemented("QUIC close".to_string()))
    }

    fn peer_addr(&self) -> String {
        String::new()
    }
}

impl<N: NetworkProvider + 'static> StreamTransport for QuicTransport<N> {
    type Stream = QuicFramedStream;

    async fn connect(&self, _addr: &PeerAddr) -> Result<Self::Stream, TransportError> {
        Err(TransportError::NotImplemented(
            "QUIC stream transport not yet implemented".to_string(),
        ))
    }

    async fn listen(&self) -> Result<StreamListener<Self::Stream>, TransportError> {
        Err(TransportError::NotImplemented(
            "QUIC stream listener not yet implemented".to_string(),
        ))
    }
}

impl<N: NetworkProvider + 'static> RawTransport for QuicTransport<N> {
    async fn open(
        &self,
        _addr: &PeerAddr,
        _port: u16,
    ) -> Result<tokio::net::TcpStream, TransportError> {
        Err(TransportError::NotImplemented(
            "QUIC raw transport not yet implemented".to_string(),
        ))
    }

    async fn listen(&self, _port: u16) -> Result<RawListener, TransportError> {
        Err(TransportError::NotImplemented(
            "QUIC raw listener not yet implemented".to_string(),
        ))
    }
}
