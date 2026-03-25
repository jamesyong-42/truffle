//! # truffle-core
//!
//! Clean architecture rebuild of truffle's core networking library.
//!
//! This crate implements the layered architecture described in RFC 012:
//! - **Layer 3 (Network)**: Peer discovery, addressing, encrypted tunnels via Tailscale
//! - **Layer 4 (Transport)**: WebSocket, TCP, QUIC protocol transports
//! - **Layer 5 (Session)**: Peer registry, lazy connections, message routing
//! - **Layer 6 (Envelope)**: Namespace-based message framing
//! - **Node API**: Single public entry point wiring all layers together
//!
//! ## Quick start
//!
//! ```ignore
//! use truffle_core::{Node, NodeBuilder};
//!
//! let node = Node::builder()
//!     .name("my-app")
//!     .sidecar_path("/usr/local/bin/truffle-sidecar")
//!     .build()
//!     .await?;
//!
//! let peers = node.peers().await;
//! node.send(&peers[0].id, "chat", b"hello!").await?;
//! ```
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
//! ## Layer 5 — Session
//!
//! The [`PeerRegistry`](session::PeerRegistry) manages peer state and WebSocket connections.
//! Peers exist in the registry when Layer 3 discovers them, even before any transport
//! connections are established.
//!
//! ## Layer 6 — Envelope
//!
//! The [`Envelope`] struct wraps all application messages with a namespace string
//! for routing and an opaque JSON payload that truffle-core never inspects.
//!
//! ## Node API
//!
//! The [`Node`] struct is the single public entry point. It exposes ~12 methods
//! covering discovery, messaging, raw streams, and diagnostics.

pub mod network;
pub mod transport;
pub mod session;
pub mod envelope;
pub mod node;

// Re-export the main public types for convenience.
pub use node::{Node, NodeBuilder, Peer, NamespacedMessage, NodeError};
pub use envelope::Envelope;
