//! File transfer — thin wrapper over truffle-core's file_transfer module.
//!
//! The core handles all protocol logic (offer, accept, TCP streaming, SHA-256).
//! This module provides the CLI-specific receive handler that bridges the core's
//! offer channel to the CLI's auto-accept or TUI's interactive accept/reject.

pub mod receive;
pub mod types;
