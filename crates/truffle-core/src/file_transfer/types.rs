//! Types for the file transfer protocol.
//!
//! This module defines:
//! - Wire protocol messages (`FtMessage`)
//! - Transfer results and errors
//! - The offer/decision/responder API for accept/reject workflows
//! - Progress and event types for observability

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Wire protocol messages (sent via WS namespace "ft")
// ---------------------------------------------------------------------------

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
    Reject { token: String, reason: String },

    /// Request a file from a remote peer (for pull/download).
    #[serde(rename = "pull_request")]
    PullRequest {
        path: String,
        requester_id: String,
        token: String,
    },
}

// ---------------------------------------------------------------------------
// Transfer result
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Transfer error
// ---------------------------------------------------------------------------

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
                write!(f, "SHA-256 mismatch: expected {expected}, got {actual}")
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

// ---------------------------------------------------------------------------
// Offer API — accept/reject workflow
// ---------------------------------------------------------------------------

/// An incoming file offer from a remote peer.
///
/// The application inspects this and decides whether to accept (providing a
/// save path) or reject (providing a reason). Use the paired
/// [`OfferResponder`] to communicate the decision.
#[derive(Debug, Clone)]
pub struct FileOffer {
    /// Stable node ID of the sending peer.
    pub from_peer: String,
    /// Human-readable name of the sending peer.
    pub from_name: String,
    /// File name being offered.
    pub file_name: String,
    /// File size in bytes.
    pub size: u64,
    /// Expected SHA-256 hash (hex).
    pub sha256: String,
    /// Suggested save path from the sender.
    pub suggested_path: String,
    /// Unique token for this transfer.
    pub token: String,
}

/// The application's decision on a file offer.
#[derive(Debug, Clone)]
pub enum OfferDecision {
    /// Accept the file, saving it to the given path.
    Accept { save_path: String },
    /// Reject the file with a reason.
    Reject { reason: String },
}

/// One-shot responder for an incoming file offer.
///
/// Created as a pair with a [`FileOffer`]. The application calls either
/// `accept()` or `reject()` exactly once. If the responder is dropped
/// without a response, the transfer times out on the receiver side.
pub struct OfferResponder {
    tx: tokio::sync::oneshot::Sender<OfferDecision>,
}

impl OfferResponder {
    /// Create a new responder from a oneshot sender.
    pub(crate) fn new(tx: tokio::sync::oneshot::Sender<OfferDecision>) -> Self {
        Self { tx }
    }

    /// Accept the file, saving it to `save_path`.
    pub fn accept(self, save_path: &str) {
        let _ = self.tx.send(OfferDecision::Accept {
            save_path: save_path.to_string(),
        });
    }

    /// Reject the file with a reason.
    pub fn reject(self, reason: &str) {
        let _ = self.tx.send(OfferDecision::Reject {
            reason: reason.to_string(),
        });
    }
}

impl std::fmt::Debug for OfferResponder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfferResponder").finish()
    }
}

// ---------------------------------------------------------------------------
// Transfer direction
// ---------------------------------------------------------------------------

/// Direction of a file transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Sending a file to a peer.
    Send,
    /// Receiving a file from a peer.
    Receive,
}

// ---------------------------------------------------------------------------
// Transfer progress
// ---------------------------------------------------------------------------

/// Progress update for an in-flight file transfer.
#[derive(Debug, Clone)]
pub struct TransferProgress {
    /// Unique token for this transfer.
    pub token: String,
    /// Direction of the transfer.
    pub direction: TransferDirection,
    /// File name being transferred.
    pub file_name: String,
    /// Bytes transferred so far.
    pub bytes_transferred: u64,
    /// Total file size in bytes.
    pub total_bytes: u64,
    /// Current transfer speed in bytes per second.
    pub speed_bps: f64,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Events emitted by the file transfer subsystem.
///
/// Subscribe via [`FileTransfer::subscribe`](super::FileTransfer::subscribe)
/// to observe transfer lifecycle events.
#[derive(Debug, Clone)]
pub enum FileTransferEvent {
    /// A new file offer has been received (informational; the offer channel
    /// is the primary mechanism for handling offers).
    OfferReceived(FileOffer),

    /// Progress update for an in-flight transfer.
    Progress(TransferProgress),

    /// A transfer completed successfully.
    Completed {
        token: String,
        direction: TransferDirection,
        file_name: String,
        bytes_transferred: u64,
        sha256: String,
        elapsed_secs: f64,
    },

    /// A transfer failed.
    Failed {
        token: String,
        direction: TransferDirection,
        file_name: String,
        reason: String,
    },
}
