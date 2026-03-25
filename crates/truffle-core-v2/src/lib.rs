//! # truffle-core-v2
//!
//! Clean architecture rebuild of truffle's core networking library.
//!
//! This crate implements the layered architecture described in RFC 012:
//! - **Layer 3 (Network)**: Peer discovery, addressing, encrypted tunnels via Tailscale
//! - **Layer 4 (Transport)**: WebSocket, TCP, QUIC protocol transports (future phases)
//! - **Layer 5 (Session)**: Peer registry, connection lifecycle (future phases)
//! - **Layer 6 (Envelope)**: Namespace-based message framing (future phases)
//!
//! ## Phase 1: Layer 3 — Network
//!
//! The [`NetworkProvider`] trait defines a generic interface for peer discovery
//! and raw connectivity. The [`TailscaleProvider`] implementation wraps the Go
//! sidecar (tsnet) to provide encrypted Tailscale tunnels.
//!
//! ```ignore
//! use truffle_core_v2::network::{NetworkProvider, TailscaleProvider, TailscaleConfig};
//!
//! let config = TailscaleConfig { /* ... */ };
//! let mut provider = TailscaleProvider::new(config);
//! provider.start().await?;
//!
//! let peers = provider.peers().await;
//! let stream = provider.dial_tcp("peer.tailnet.ts.net", 9417).await?;
//! ```

pub mod network;
