//! Daemon process for the truffle CLI v2.
//!
//! The daemon holds a `Node<TailscaleProvider>` and listens on IPC for JSON-RPC
//! requests from CLI commands.
//!
//! This is MUCH simpler than v1:
//! - No FileTransferManager, BridgeManager, DialFn, axum, or TcpProxyHandler
//! - Just: Node + IPC loop. That's it.

pub mod client;
pub mod handler;
pub mod ipc;
pub mod pid;
pub mod protocol;
pub mod server;
