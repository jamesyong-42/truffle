use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Transfer ID format: "ft-<random-hex-16>" (64 bits of randomness).
pub type TransferID = String;

/// Transfer state (accessed atomically on hot path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferState {
    Registered = 0,
    Transferring = 1,
    Completed = 2,
    Failed = 3,
    Cancelled = 4,
}

impl TransferState {
    pub fn from_i32(v: i32) -> Self {
        match v {
            0 => Self::Registered,
            1 => Self::Transferring,
            2 => Self::Completed,
            3 => Self::Failed,
            4 => Self::Cancelled,
            _ => Self::Failed,
        }
    }
}

impl std::fmt::Display for TransferState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registered => write!(f, "registered"),
            Self::Transferring => write!(f, "transferring"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// File metadata for a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub size: i64,
    pub sha256: String,
}

/// Direction of a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferDirection {
    Send,
    Receive,
}

impl std::fmt::Display for TransferDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Send => write!(f, "send"),
            Self::Receive => write!(f, "receive"),
        }
    }
}

/// A single file transfer, tracking state for either sender or receiver side.
///
/// State is accessed atomically (hot path in CancellableAsyncRead).
/// inFlight prevents concurrent PUT handlers from interleaving writes.
pub struct Transfer {
    pub id: TransferID,
    pub file: FileInfo,
    /// Receiver only: where to save the file.
    pub save_path: Option<String>,
    /// Sender only: source file path.
    pub file_path: Option<String>,
    pub token: String,
    pub direction: TransferDirection,

    /// Atomic state (hot path).
    state: AtomicI32,
    /// 1 when a PUT handler is actively processing; prevents concurrent handlers.
    in_flight: AtomicI32,

    /// Progress tracking (guarded by mutex).
    pub bytes_transferred: AtomicI64,
    pub registered_at: std::time::Instant,
    pub started_at: Mutex<Option<std::time::Instant>>,
    pub last_progress_at: Mutex<Option<std::time::Instant>>,

    /// Cancellation token for this transfer.
    pub cancel_token: CancellationToken,
}

impl Transfer {
    pub fn new(
        id: TransferID,
        file: FileInfo,
        token: String,
        direction: TransferDirection,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            file,
            save_path: None,
            file_path: None,
            token,
            direction,
            state: AtomicI32::new(TransferState::Registered as i32),
            in_flight: AtomicI32::new(0),
            bytes_transferred: AtomicI64::new(0),
            registered_at: std::time::Instant::now(),
            started_at: Mutex::new(None),
            last_progress_at: Mutex::new(None),
            cancel_token: CancellationToken::new(),
        })
    }

    pub fn new_with_save_path(
        id: TransferID,
        file: FileInfo,
        token: String,
        save_path: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            file,
            save_path: Some(save_path),
            file_path: None,
            token,
            direction: TransferDirection::Receive,
            state: AtomicI32::new(TransferState::Registered as i32),
            in_flight: AtomicI32::new(0),
            bytes_transferred: AtomicI64::new(0),
            registered_at: std::time::Instant::now(),
            started_at: Mutex::new(None),
            last_progress_at: Mutex::new(None),
            cancel_token: CancellationToken::new(),
        })
    }

    pub fn new_sender(
        id: TransferID,
        file: FileInfo,
        token: String,
        file_path: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            file,
            save_path: None,
            file_path: Some(file_path),
            token,
            direction: TransferDirection::Send,
            state: AtomicI32::new(TransferState::Transferring as i32),
            in_flight: AtomicI32::new(0),
            bytes_transferred: AtomicI64::new(0),
            registered_at: std::time::Instant::now(),
            started_at: Mutex::new(Some(std::time::Instant::now())),
            last_progress_at: Mutex::new(None),
            cancel_token: CancellationToken::new(),
        })
    }

    /// Get current state atomically.
    pub fn state(&self) -> TransferState {
        TransferState::from_i32(self.state.load(Ordering::SeqCst))
    }

    /// Set state atomically.
    pub fn set_state(&self, s: TransferState) {
        self.state.store(s as i32, Ordering::SeqCst);
    }

    /// CAS for state transition. Returns true if successful.
    pub fn compare_and_swap_state(&self, old: TransferState, new: TransferState) -> bool {
        self.state
            .compare_exchange(old as i32, new as i32, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Try to acquire the in-flight lock. Returns true if acquired.
    pub fn try_acquire_in_flight(&self) -> bool {
        self.in_flight
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Release the in-flight lock.
    pub fn release_in_flight(&self) {
        self.in_flight.store(0, Ordering::SeqCst);
    }

    /// Check if cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.state() == TransferState::Cancelled
    }

    /// Cancel this transfer.
    pub fn cancel(&self) {
        self.set_state(TransferState::Cancelled);
        self.cancel_token.cancel();
    }
}

/// Configuration for the file transfer manager.
#[derive(Debug, Clone)]
pub struct FileTransferConfig {
    /// Maximum file size in bytes. Default: 4GB.
    pub max_file_size: i64,
    /// Max concurrent incoming transfers. Default: 5. 0 = unlimited.
    pub max_concurrent_recv: usize,
    /// Minimum interval between progress events. Default: 500ms.
    pub progress_interval: Duration,
    /// Minimum bytes between progress events. Default: 256KB.
    pub progress_bytes: i64,
    /// Graceful shutdown timeout. Default: 30s.
    pub shutdown_timeout: Duration,
    /// How long completed/failed/cancelled transfers stay in memory. Default: 5min.
    pub completed_retain_ttl: Duration,
    /// How long a registered (not yet started) transfer lives. Default: 2min.
    pub registered_timeout_ttl: Duration,
}

impl Default for FileTransferConfig {
    fn default() -> Self {
        Self {
            max_file_size: 100 * 1024 * 1024 * 1024, // 100GB
            max_concurrent_recv: 5,
            progress_interval: Duration::from_millis(500),
            progress_bytes: 256 * 1024, // 256KB
            shutdown_timeout: Duration::from_secs(30),
            completed_retain_ttl: Duration::from_secs(5 * 60),
            registered_timeout_ttl: Duration::from_secs(2 * 60),
        }
    }
}

/// Events emitted by the file transfer system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum FileTransferEvent {
    /// File preparation progress (hashing).
    #[serde(rename = "file:preparing_progress")]
    PreparingProgress {
        transfer_id: String,
        bytes_hashed: i64,
        total_bytes: i64,
        percent: f64,
    },

    /// File prepared (stat + hash complete).
    #[serde(rename = "file:prepared")]
    Prepared {
        transfer_id: String,
        name: String,
        size: i64,
        sha256: String,
    },

    /// Transfer progress.
    #[serde(rename = "file:progress")]
    Progress {
        transfer_id: String,
        bytes_transferred: i64,
        total_bytes: i64,
        percent: f64,
        bytes_per_second: f64,
        eta: f64,
        direction: String,
    },

    /// Transfer complete.
    #[serde(rename = "file:complete")]
    Complete {
        transfer_id: String,
        sha256: String,
        size: i64,
        duration_ms: u64,
        direction: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },

    /// Transfer error.
    #[serde(rename = "file:error")]
    Error {
        transfer_id: String,
        code: String,
        message: String,
        resumable: bool,
    },

    /// Transfer cancelled.
    #[serde(rename = "file:cancelled")]
    Cancelled { transfer_id: String },

    /// Transfer list response.
    #[serde(rename = "file:list")]
    List { transfers: Vec<TransferInfo> },
}

/// Summary info for a transfer (for list responses).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferInfo {
    pub id: String,
    pub state: String,
    pub file: FileInfo,
    pub direction: String,
    pub bytes_transferred: i64,
}

/// HTTP error response body for non-200 responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpErrorResponse {
    pub code: String,
    pub message: String,
}

/// Error codes for file transfers.
pub mod error_codes {
    pub const FORBIDDEN: &str = "FORBIDDEN";
    pub const TRANSFER_NOT_FOUND: &str = "TRANSFER_NOT_FOUND";
    pub const TRANSFER_NOT_ACCEPTING: &str = "TRANSFER_NOT_ACCEPTING";
    pub const TRANSFER_IN_FLIGHT: &str = "TRANSFER_IN_FLIGHT";
    pub const OFFSET_MISMATCH: &str = "OFFSET_MISMATCH";
    pub const INTEGRITY_MISMATCH: &str = "INTEGRITY_MISMATCH";
    pub const HASH_HEADER_MISMATCH: &str = "HASH_HEADER_MISMATCH";
    pub const MALFORMED_RANGE: &str = "MALFORMED_RANGE";
    pub const RANGE_SIZE_MISMATCH: &str = "RANGE_SIZE_MISMATCH";
    pub const RANGE_END_INVALID: &str = "RANGE_END_INVALID";
    pub const RANGE_START_INVALID: &str = "RANGE_START_INVALID";
    pub const CONTENT_LENGTH_MISMATCH: &str = "CONTENT_LENGTH_MISMATCH";
    pub const MISSING_RANGE_FOR_RESUME: &str = "MISSING_RANGE_FOR_RESUME";
    pub const INCOMPLETE_BODY: &str = "INCOMPLETE_BODY";
    pub const INVALID_SIZE: &str = "INVALID_SIZE";
    pub const FILE_TOO_LARGE: &str = "FILE_TOO_LARGE";
    pub const TOO_MANY_TRANSFERS: &str = "TOO_MANY_TRANSFERS";
    pub const INSUFFICIENT_DISK_SPACE: &str = "INSUFFICIENT_DISK_SPACE";
    pub const WRITE_ERROR: &str = "WRITE_ERROR";
    pub const DISK_ERROR: &str = "DISK_ERROR";
    pub const HASH_ERROR: &str = "HASH_ERROR";
    pub const RENAME_ERROR: &str = "RENAME_ERROR";
    pub const SEND_ERROR: &str = "SEND_ERROR";
    pub const REMOTE_ERROR: &str = "REMOTE_ERROR";
    pub const RESUME_QUERY_ERROR: &str = "RESUME_QUERY_ERROR";
    pub const FILE_OPEN_ERROR: &str = "FILE_OPEN_ERROR";
    pub const SEEK_ERROR: &str = "SEEK_ERROR";
    pub const REQUEST_ERROR: &str = "REQUEST_ERROR";
    pub const INVALID_COMMAND: &str = "INVALID_COMMAND";
    pub const INVALID_PATH: &str = "INVALID_PATH";
    pub const FILE_NOT_FOUND: &str = "FILE_NOT_FOUND";
    pub const IS_DIRECTORY: &str = "IS_DIRECTORY";

    /// Returns whether the given error code allows retry.
    pub fn is_resumable(code: &str) -> bool {
        matches!(
            code,
            SEND_ERROR
                | REMOTE_ERROR
                | RESUME_QUERY_ERROR
                | TOO_MANY_TRANSFERS
                | TRANSFER_NOT_ACCEPTING
                | TRANSFER_IN_FLIGHT
                | WRITE_ERROR
                | INCOMPLETE_BODY
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_state_display() {
        assert_eq!(TransferState::Registered.to_string(), "registered");
        assert_eq!(TransferState::Transferring.to_string(), "transferring");
        assert_eq!(TransferState::Completed.to_string(), "completed");
        assert_eq!(TransferState::Failed.to_string(), "failed");
        assert_eq!(TransferState::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn transfer_state_from_i32() {
        assert_eq!(TransferState::from_i32(0), TransferState::Registered);
        assert_eq!(TransferState::from_i32(1), TransferState::Transferring);
        assert_eq!(TransferState::from_i32(4), TransferState::Cancelled);
        assert_eq!(TransferState::from_i32(99), TransferState::Failed);
    }

    #[test]
    fn transfer_state_cas() {
        let t = Transfer::new(
            "ft-test".to_string(),
            FileInfo { name: "f.txt".to_string(), size: 100, sha256: "abc".to_string() },
            "token".to_string(),
            TransferDirection::Receive,
        );
        assert_eq!(t.state(), TransferState::Registered);

        assert!(t.compare_and_swap_state(TransferState::Registered, TransferState::Transferring));
        assert_eq!(t.state(), TransferState::Transferring);

        // CAS should fail if old state doesn't match
        assert!(!t.compare_and_swap_state(TransferState::Registered, TransferState::Completed));
        assert_eq!(t.state(), TransferState::Transferring);
    }

    #[test]
    fn transfer_in_flight_lock() {
        let t = Transfer::new(
            "ft-test".to_string(),
            FileInfo { name: "f.txt".to_string(), size: 100, sha256: "abc".to_string() },
            "token".to_string(),
            TransferDirection::Receive,
        );
        assert!(t.try_acquire_in_flight());
        assert!(!t.try_acquire_in_flight()); // Already locked
        t.release_in_flight();
        assert!(t.try_acquire_in_flight()); // Released, can acquire again
    }

    #[test]
    fn transfer_cancel() {
        let t = Transfer::new(
            "ft-test".to_string(),
            FileInfo { name: "f.txt".to_string(), size: 100, sha256: "abc".to_string() },
            "token".to_string(),
            TransferDirection::Receive,
        );
        assert!(!t.is_cancelled());
        t.cancel();
        assert!(t.is_cancelled());
        assert!(t.cancel_token.is_cancelled());
    }

    #[test]
    fn default_config() {
        let cfg = FileTransferConfig::default();
        assert_eq!(cfg.max_file_size, 100 * 1024 * 1024 * 1024);
        assert_eq!(cfg.max_concurrent_recv, 5);
        assert_eq!(cfg.progress_interval, Duration::from_millis(500));
        assert_eq!(cfg.progress_bytes, 256 * 1024);
    }

    #[test]
    fn error_codes_resumable() {
        assert!(error_codes::is_resumable("SEND_ERROR"));
        assert!(error_codes::is_resumable("WRITE_ERROR"));
        assert!(error_codes::is_resumable("INCOMPLETE_BODY"));
        assert!(!error_codes::is_resumable("FORBIDDEN"));
        assert!(!error_codes::is_resumable("INTEGRITY_MISMATCH"));
        assert!(!error_codes::is_resumable("FILE_NOT_FOUND"));
    }

    #[test]
    fn file_info_serde() {
        let info = FileInfo {
            name: "photo.jpg".to_string(),
            size: 1024 * 1024,
            sha256: "e3b0c44298fc1c149afbf4c8996fb924".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: FileInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, info);
    }

    #[test]
    fn http_error_serde() {
        let err = HttpErrorResponse {
            code: "TRANSFER_NOT_FOUND".to_string(),
            message: "transfer not found".to_string(),
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("TRANSFER_NOT_FOUND"));
    }

    // ══════════════════════════════════════════════════════════════════════
    // Adversarial edge-case tests (Layer 4: Services — Types)
    // ══════════════════════════════════════════════════════════════════════

    // ── 26. FileInfo with zero size ──────────────────────────────────────
    #[test]
    fn file_info_zero_size_no_divide_by_zero() {
        let info = FileInfo {
            name: "empty.txt".to_string(),
            size: 0,
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
        };

        // Simulate progress calculation: percent = bytes_transferred / size * 100
        let bytes_transferred: f64 = 0.0;
        let percent = if info.size > 0 {
            bytes_transferred / info.size as f64 * 100.0
        } else {
            0.0
        };
        assert_eq!(percent, 0.0, "Zero-size file should yield 0% without panic");

        // ETA calculation: eta = remaining / speed
        let speed: f64 = 0.0;
        let eta = if speed > 0.0 {
            (info.size as f64 - bytes_transferred) / speed
        } else {
            0.0
        };
        assert_eq!(eta, 0.0, "Zero speed with zero size should yield 0 ETA");

        // Serde roundtrip
        let json = serde_json::to_string(&info).unwrap();
        let parsed: FileInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.size, 0);
    }

    // ── 27. FileInfo with very large size ────────────────────────────────
    #[test]
    fn file_info_max_i64_size_no_overflow() {
        let info = FileInfo {
            name: "huge.bin".to_string(),
            size: i64::MAX,
            sha256: "abc".to_string(),
        };

        // Progress calculation with max size
        let bytes_transferred: i64 = i64::MAX / 2;
        let percent = if info.size > 0 {
            bytes_transferred as f64 / info.size as f64 * 100.0
        } else {
            0.0
        };
        assert!(percent > 0.0 && percent < 100.0, "Should compute valid percent");
        assert!((percent - 50.0).abs() < 1.0, "Should be approximately 50%");

        // Speed/ETA calculation
        let speed: f64 = 1_000_000_000.0; // 1 GB/s
        let remaining = info.size - bytes_transferred;
        let eta = remaining as f64 / speed;
        assert!(eta > 0.0 && eta.is_finite(), "ETA should be positive and finite");

        // Serde roundtrip with large value
        let json = serde_json::to_string(&info).unwrap();
        let parsed: FileInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.size, i64::MAX);
    }

    // ── 28. Transfer state transitions ───────────────────────────────────
    #[test]
    fn transfer_state_valid_transitions_pending_to_sending_to_complete() {
        let t = Transfer::new(
            "ft-transition-1".to_string(),
            FileInfo { name: "f.bin".to_string(), size: 100, sha256: "abc".to_string() },
            "a".repeat(64),
            TransferDirection::Send,
        );

        // Registered -> Transferring
        assert_eq!(t.state(), TransferState::Registered);
        assert!(t.compare_and_swap_state(TransferState::Registered, TransferState::Transferring));
        assert_eq!(t.state(), TransferState::Transferring);

        // Transferring -> Completed
        t.set_state(TransferState::Completed);
        assert_eq!(t.state(), TransferState::Completed);
    }

    #[test]
    fn transfer_state_valid_transitions_pending_to_rejected() {
        let t = Transfer::new(
            "ft-transition-2".to_string(),
            FileInfo { name: "f.bin".to_string(), size: 100, sha256: "abc".to_string() },
            "b".repeat(64),
            TransferDirection::Receive,
        );

        // Registered -> Failed (rejected)
        assert_eq!(t.state(), TransferState::Registered);
        t.set_state(TransferState::Failed);
        assert_eq!(t.state(), TransferState::Failed);
    }

    #[test]
    fn transfer_state_valid_transitions_sending_to_cancelled() {
        let t = Transfer::new(
            "ft-transition-3".to_string(),
            FileInfo { name: "f.bin".to_string(), size: 100, sha256: "abc".to_string() },
            "c".repeat(64),
            TransferDirection::Send,
        );

        // Registered -> Transferring -> Cancelled
        assert!(t.compare_and_swap_state(TransferState::Registered, TransferState::Transferring));
        assert_eq!(t.state(), TransferState::Transferring);

        t.cancel();
        assert_eq!(t.state(), TransferState::Cancelled);
        assert!(t.cancel_token.is_cancelled());
    }

    // ── Edge: CAS from wrong state fails ─────────────────────────────────
    #[test]
    fn transfer_state_cas_from_wrong_state_fails() {
        let t = Transfer::new(
            "ft-cas-fail".to_string(),
            FileInfo { name: "f.bin".to_string(), size: 100, sha256: "abc".to_string() },
            "d".repeat(64),
            TransferDirection::Receive,
        );

        // Try to CAS from Transferring when actually Registered
        assert!(!t.compare_and_swap_state(TransferState::Transferring, TransferState::Completed));
        assert_eq!(t.state(), TransferState::Registered, "State should not change on failed CAS");
    }

    // ── Edge: Double cancel ──────────────────────────────────────────────
    #[test]
    fn transfer_double_cancel_is_safe() {
        let t = Transfer::new(
            "ft-double-cancel".to_string(),
            FileInfo { name: "f.bin".to_string(), size: 100, sha256: "abc".to_string() },
            "e".repeat(64),
            TransferDirection::Send,
        );

        t.cancel();
        assert!(t.is_cancelled());

        // Second cancel should not panic
        t.cancel();
        assert!(t.is_cancelled());
        assert!(t.cancel_token.is_cancelled());
    }

    // ── Edge: All TransferState from_i32 values ──────────────────────────
    #[test]
    fn transfer_state_from_i32_exhaustive() {
        assert_eq!(TransferState::from_i32(0), TransferState::Registered);
        assert_eq!(TransferState::from_i32(1), TransferState::Transferring);
        assert_eq!(TransferState::from_i32(2), TransferState::Completed);
        assert_eq!(TransferState::from_i32(3), TransferState::Failed);
        assert_eq!(TransferState::from_i32(4), TransferState::Cancelled);

        // Negative values
        assert_eq!(TransferState::from_i32(-1), TransferState::Failed);
        assert_eq!(TransferState::from_i32(i32::MIN), TransferState::Failed);
        assert_eq!(TransferState::from_i32(i32::MAX), TransferState::Failed);
    }

    // ── Edge: new_sender starts in Transferring state ────────────────────
    #[test]
    fn new_sender_starts_transferring() {
        let t = Transfer::new_sender(
            "ft-sender-state".to_string(),
            FileInfo { name: "f.bin".to_string(), size: 100, sha256: "abc".to_string() },
            "f".repeat(64),
            "/tmp/source.bin".to_string(),
        );

        assert_eq!(t.state(), TransferState::Transferring, "Senders start in Transferring");
        assert!(t.file_path.as_deref() == Some("/tmp/source.bin"));
        assert!(t.save_path.is_none());
    }

    // ── Edge: new_with_save_path direction is Receive ────────────────────
    #[test]
    fn new_with_save_path_is_receive_direction() {
        let t = Transfer::new_with_save_path(
            "ft-recv-dir".to_string(),
            FileInfo { name: "f.bin".to_string(), size: 100, sha256: "abc".to_string() },
            "g".repeat(64),
            "/tmp/dest.bin".to_string(),
        );

        assert_eq!(t.direction, TransferDirection::Receive);
        assert_eq!(t.state(), TransferState::Registered);
        assert_eq!(t.save_path.as_deref(), Some("/tmp/dest.bin"));
        assert!(t.file_path.is_none());
    }

    // ── Edge: Concurrent in-flight acquire attempts ──────────────────────
    #[test]
    fn in_flight_prevents_double_acquisition() {
        let t = Transfer::new(
            "ft-inflight".to_string(),
            FileInfo { name: "f.bin".to_string(), size: 100, sha256: "abc".to_string() },
            "h".repeat(64),
            TransferDirection::Receive,
        );

        assert!(t.try_acquire_in_flight(), "First acquire should succeed");
        assert!(!t.try_acquire_in_flight(), "Second acquire should fail");
        assert!(!t.try_acquire_in_flight(), "Third acquire should also fail");

        t.release_in_flight();
        assert!(t.try_acquire_in_flight(), "After release, acquire should succeed");
    }

    // ── Edge: FileTransferEvent serde roundtrip ──────────────────────────
    #[test]
    fn file_transfer_event_serde_roundtrip() {
        let events = vec![
            FileTransferEvent::PreparingProgress {
                transfer_id: "ft-1".to_string(),
                bytes_hashed: 512,
                total_bytes: 1024,
                percent: 50.0,
            },
            FileTransferEvent::Prepared {
                transfer_id: "ft-2".to_string(),
                name: "file.bin".to_string(),
                size: 1024,
                sha256: "abc".to_string(),
            },
            FileTransferEvent::Progress {
                transfer_id: "ft-3".to_string(),
                bytes_transferred: 256,
                total_bytes: 1024,
                percent: 25.0,
                bytes_per_second: 100.0,
                eta: 7.68,
                direction: "send".to_string(),
            },
            FileTransferEvent::Complete {
                transfer_id: "ft-4".to_string(),
                sha256: "xyz".to_string(),
                size: 1024,
                duration_ms: 500,
                direction: "receive".to_string(),
                path: Some("/tmp/out.bin".to_string()),
            },
            FileTransferEvent::Error {
                transfer_id: "ft-5".to_string(),
                code: "SEND_ERROR".to_string(),
                message: "connection lost".to_string(),
                resumable: true,
            },
            FileTransferEvent::Cancelled {
                transfer_id: "ft-6".to_string(),
            },
        ];

        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let parsed: FileTransferEvent = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, json2, "Serde roundtrip should be stable");
        }
    }

    // ── Edge: TransferDirection display ───────────────────────────────────
    #[test]
    fn transfer_direction_display() {
        assert_eq!(TransferDirection::Send.to_string(), "send");
        assert_eq!(TransferDirection::Receive.to_string(), "receive");
    }
}
