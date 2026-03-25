//! # truffle-core-v2
//!
//! Clean architecture rebuild of truffle's core networking library.
//!
//! This crate implements the layered architecture described in RFC 012:
//! - **Layer 3 (Network)**: Peer discovery, addressing, encrypted tunnels via Tailscale
//! - **Layer 4 (Transport)**: WebSocket, TCP, QUIC protocol transports
//! - **Layer 5 (Session)**: Peer registry, lazy connections, message routing
//! - **Layer 6 (Envelope)**: Namespace-based message framing (future phases)
//!
//! ## Layer 3 — Network
//!
//! The [`NetworkProvider`](network::NetworkProvider) trait defines a generic interface for peer
//! discovery and raw connectivity. The [`TailscaleProvider`](network::tailscale::TailscaleProvider)
//! implementation wraps the Go sidecar (tsnet) to provide encrypted Tailscale tunnels.
//!
//! ## Layer 4 — Transport
//!
//! Three transport trait families sit on top of Layer 3:
//!
//! - [`StreamTransport`](transport::StreamTransport) + [`FramedStream`](transport::FramedStream):
//!   Message-oriented bidirectional connections (WebSocket, future QUIC streams).
//! - [`RawTransport`](transport::RawTransport): Raw byte streams (TCP).
//! - [`DatagramTransport`](transport::DatagramTransport): Unreliable datagrams (future UDP/QUIC).
//!
//! ```ignore
//! use std::sync::Arc;
//! use truffle_core_v2::transport::{StreamTransport, FramedStream, WsConfig};
//! use truffle_core_v2::transport::websocket::WebSocketTransport;
//!
//! let ws = WebSocketTransport::new(network_provider, WsConfig::default());
//! let mut stream = ws.connect(&peer_addr).await?;
//! stream.send(b"hello").await?;
//! let reply = stream.recv().await?;
//! ```

pub mod network;
pub mod transport;
pub mod session;
