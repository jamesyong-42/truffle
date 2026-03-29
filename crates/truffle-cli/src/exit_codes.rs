//! Structured exit codes for the truffle CLI.
//!
//! These codes allow scripts and agents to distinguish between different
//! failure modes without parsing error text.

pub const SUCCESS: i32 = 0;
pub const ERROR: i32 = 1;
pub const USAGE: i32 = 2;
pub const NOT_FOUND: i32 = 3;
pub const NOT_ONLINE: i32 = 4;
pub const TIMEOUT: i32 = 5;
pub const TRANSFER_FAILED: i32 = 6;
