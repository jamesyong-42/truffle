//! # truffle-sidecar
//!
//! Provides the truffle mesh networking sidecar binary.
//!
//! This crate downloads the platform-specific Go sidecar binary at build time
//! from truffle's GitHub releases. The binary is required by `truffle-core`
//! to establish Tailscale mesh connections.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use truffle_sidecar::sidecar_path;
//!
//! let path = sidecar_path();
//! println!("Sidecar at: {}", path.display());
//! ```
//!
//! ## Override
//!
//! Set `TRUFFLE_SIDECAR_PATH` environment variable at build time or runtime
//! to use a custom sidecar binary:
//!
//! ```bash
//! TRUFFLE_SIDECAR_PATH=/usr/local/bin/sidecar-slim cargo build
//! ```
//!
//! Set `TRUFFLE_SIDECAR_SKIP_DOWNLOAD=1` to skip the build-time download
//! (useful in CI where the sidecar is provided separately).

use std::path::PathBuf;

/// Returns the path to the truffle sidecar binary.
///
/// Resolution order:
/// 1. `TRUFFLE_SIDECAR_PATH` environment variable (runtime override)
/// 2. Build-time downloaded binary (set during `cargo build`)
/// 3. Falls back to `"truffle-sidecar"` (assumes on PATH)
pub fn sidecar_path() -> PathBuf {
    // Runtime override takes precedence
    if let Ok(path) = std::env::var("TRUFFLE_SIDECAR_PATH") {
        return PathBuf::from(path);
    }

    // Build-time path (set by build.rs via cargo:rustc-env)
    PathBuf::from(env!("TRUFFLE_SIDECAR_PATH"))
}

/// Returns the version of the sidecar this crate was built to download.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
