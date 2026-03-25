//! Types for the file transfer protocol.

use serde::{Deserialize, Serialize};

/// File transfer signaling message types (sent via WS namespace "ft").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FtMessage {
    /// Sender offers a file to the receiver.
    #[serde(rename = "offer")]
    Offer {
        file_name: String,
        size: u64,
        sha256: String,
        save_path: String,
        token: String,
        /// TCP port the sender/receiver should use (0 = any).
        tcp_port: u16,
    },

    /// Receiver accepts the offer.
    #[serde(rename = "accept")]
    Accept {
        token: String,
        /// TCP port the receiver is listening on (for downloads).
        tcp_port: u16,
    },

    /// Receiver rejects the offer.
    #[serde(rename = "reject")]
    Reject {
        token: String,
        reason: String,
    },

    /// Request a file from a remote peer (for `truffle cp server:/path ./`).
    #[serde(rename = "pull_request")]
    PullRequest {
        path: String,
        requester_id: String,
        token: String,
    },
}

/// Result of a completed file transfer.
#[derive(Debug, Clone)]
pub struct TransferResult {
    /// Number of bytes transferred.
    pub bytes_transferred: u64,
    /// SHA-256 hash of the transferred file.
    pub sha256: String,
    /// Elapsed time in seconds.
    pub elapsed_secs: f64,
}

/// Errors during file transfer.
#[derive(Debug)]
pub enum TransferError {
    /// File I/O error.
    Io(std::io::Error),
    /// Network/Node error.
    Node(String),
    /// Peer rejected the transfer.
    Rejected(String),
    /// SHA-256 mismatch.
    IntegrityError { expected: String, actual: String },
    /// Timeout waiting for peer response.
    Timeout,
    /// Protocol error (unexpected message, etc.).
    Protocol(String),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferError::Io(e) => write!(f, "I/O error: {e}"),
            TransferError::Node(e) => write!(f, "Node error: {e}"),
            TransferError::Rejected(reason) => write!(f, "Transfer rejected: {reason}"),
            TransferError::IntegrityError { expected, actual } => {
                write!(
                    f,
                    "SHA-256 mismatch: expected {expected}, got {actual}"
                )
            }
            TransferError::Timeout => write!(f, "Timed out waiting for peer response"),
            TransferError::Protocol(e) => write!(f, "Protocol error: {e}"),
        }
    }
}

impl std::error::Error for TransferError {}

impl From<std::io::Error> for TransferError {
    fn from(e: std::io::Error) -> Self {
        TransferError::Io(e)
    }
}
