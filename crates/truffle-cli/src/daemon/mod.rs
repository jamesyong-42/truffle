//! Daemon process for the truffle CLI.
//!
//! The daemon holds a `TruffleRuntime` instance and listens on a
//! platform-appropriate IPC transport (Unix socket on macOS/Linux, named pipe
//! on Windows) for JSON-RPC 2.0 requests from CLI commands. This eliminates
//! the need to start/stop the runtime for every command invocation.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │  truffle daemon                                          │
//! │                                                          │
//! │  TruffleRuntime (MeshNode + Bridge + GoShim)            │
//! │  IPC Server (Unix socket / Windows named pipe)          │
//! │  PID File (<config_dir>/truffle/truffle.pid)            │
//! └─────────────────────────────────────────────────────────┘
//! ```

pub mod client;
pub mod handler;
pub mod ipc;
pub mod pid;
pub mod protocol;
pub mod server;
