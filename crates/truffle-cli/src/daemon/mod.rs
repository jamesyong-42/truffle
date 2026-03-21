//! Daemon process for the truffle CLI.
//!
//! The daemon holds a `TruffleRuntime` instance and listens on a Unix socket
//! for JSON-RPC 2.0 requests from CLI commands. This eliminates the need to
//! start/stop the runtime for every command invocation.
//!
//! # Architecture
//!
//! ```text
//! ┌────────────────────────────────────────────────┐
//! │  truffle daemon                                 │
//! │                                                 │
//! │  TruffleRuntime (MeshNode + Bridge + GoShim)   │
//! │  Unix Socket Server (~/.config/truffle/sock)    │
//! │  PID File (~/.config/truffle/truffle.pid)       │
//! └────────────────────────────────────────────────┘
//! ```

pub mod client;
pub mod handler;
pub mod pid;
pub mod protocol;
pub mod server;
