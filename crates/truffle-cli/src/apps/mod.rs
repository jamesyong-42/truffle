//! Layer 7 applications.
//!
//! Each application module uses ONLY the `Node` public API.
//! No direct access to GoShim, BridgeManager, or any internal layer.

pub mod diagnostics;
pub mod file_transfer;
pub mod messaging;
