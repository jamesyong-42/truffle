//! # truffle
//!
//! P2P mesh networking for your devices, built on Tailscale.
//!
//! This is the convenience crate that bundles `truffle-core` (the Rust library)
//! and `truffle-sidecar` (auto-downloaded Go binary). For most users, this is
//! the only dependency you need.
//!
//! ## Quick start
//!
//! ```toml
//! [dependencies]
//! truffle = "0.4"
//! ```
//!
//! ```rust,no_run
//! use truffle::{Node, sidecar_path};
//! use truffle::network::tailscale::TailscaleProvider;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let node = Node::<TailscaleProvider>::builder()
//!     .app_id("my-app")?
//!     .device_name("my-device")
//!     .sidecar_path(sidecar_path())
//!     .build()
//!     .await?;
//!
//! // Send a message to all peers
//! node.broadcast("chat", b"hello!").await;
//!
//! // Subscribe to messages
//! let mut rx = node.subscribe("chat");
//! # Ok(())
//! # }
//! ```
//!
//! ## Advanced: BYO sidecar
//!
//! If you want to manage the sidecar binary yourself, depend on `truffle-core`
//! directly:
//!
//! ```toml
//! [dependencies]
//! truffle-core = "0.4"
//! ```

// Re-export everything from truffle-core
pub use truffle_core::*;

/// Returns the path to the truffle sidecar binary.
///
/// This uses the build-time downloaded binary from `truffle-sidecar`.
/// Override with `TRUFFLE_SIDECAR_PATH` environment variable.
pub fn sidecar_path() -> std::path::PathBuf {
    truffle_sidecar::sidecar_path()
}

/// Returns the version of the bundled sidecar.
pub fn sidecar_version() -> &'static str {
    truffle_sidecar::version()
}
