use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::put;
use axum::Router;

use super::manager::FileTransferManager;
use super::types::{error_codes, HttpErrorResponse, TransferState};

/// Build the axum Router for file transfer HTTP endpoints.
pub fn file_transfer_router(manager: Arc<FileTransferManager>) -> Router {
    Router::new()
        .route("/transfer/{transfer_id}", put(handle_put).head(handle_head))
        .with_state(manager)
}

pub(crate) fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = serde_json::to_string(&HttpErrorResponse {
        code: code.to_string(),
        message: message.to_string(),
    })
    .unwrap_or_default();

    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

/// HEAD /transfer/{transfer_id} - Query resume offset.
async fn handle_head(
    State(manager): State<Arc<FileTransferManager>>,
    Path(transfer_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let token = headers
        .get("x-transfer-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let transfer = match manager.get_transfer(&transfer_id, token) {
        Ok(t) => t,
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.contains("not found") {
                return json_error(StatusCode::NOT_FOUND, error_codes::TRANSFER_NOT_FOUND, "transfer not found");
            }
            return json_error(StatusCode::FORBIDDEN, error_codes::FORBIDDEN, "invalid or missing transfer token");
        }
    };

    // Extend the registration TTL
    {
        let mut last = transfer.last_progress_at.lock().await;
        *last = Some(std::time::Instant::now());
    }

    let offset = super::resume::current_offset(
        transfer.save_path.as_deref().unwrap_or(""),
    )
    .await;

    Response::builder()
        .status(StatusCode::OK)
        .header("upload-offset", offset.to_string())
        .body(Body::empty())
        .unwrap()
}

/// PUT /transfer/{transfer_id} - Receive file bytes.
async fn handle_put(
    State(manager): State<Arc<FileTransferManager>>,
    Path(transfer_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = headers
        .get("x-transfer-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Validate transfer and token
    let transfer = match manager.get_transfer(&transfer_id, token) {
        Ok(t) => t,
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.contains("not found") {
                return json_error(StatusCode::NOT_FOUND, error_codes::TRANSFER_NOT_FOUND, "transfer not found");
            }
            return json_error(StatusCode::FORBIDDEN, error_codes::FORBIDDEN, "invalid or missing transfer token");
        }
    };

    // Enforce max concurrent incoming transfers
    if !manager.try_acquire_recv_semaphore() {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            error_codes::TOO_MANY_TRANSFERS,
            &format!("max concurrent transfers ({}) reached", manager.config().max_concurrent_recv),
        );
    }

    // Will release semaphore when done
    let _sem_guard = scopeguard::guard((), |_| {
        manager.release_recv_semaphore();
    });

    // Enforce MaxFileSize
    if transfer.file.size > manager.config().max_file_size {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            error_codes::FILE_TOO_LARGE,
            &format!("file size {} exceeds limit {}", transfer.file.size, manager.config().max_file_size),
        );
    }
    if transfer.file.size <= 0 {
        return json_error(StatusCode::BAD_REQUEST, error_codes::INVALID_SIZE, "invalid file size");
    }

    // State transition: Registered -> Transferring, or allow resume from Transferring
    let state = transfer.state();
    match state {
        TransferState::Registered => {
            if !transfer.compare_and_swap_state(TransferState::Registered, TransferState::Transferring) {
                return json_error(
                    StatusCode::CONFLICT,
                    error_codes::TRANSFER_NOT_ACCEPTING,
                    "transfer not accepting data (state changed concurrently)",
                );
            }
        }
        TransferState::Transferring => {
            // Resume after connection drop
        }
        _ => {
            return json_error(
                StatusCode::CONFLICT,
                error_codes::TRANSFER_NOT_ACCEPTING,
                &format!("transfer in terminal state: {state}"),
            );
        }
    }

    // Acquire in-flight lock
    if !transfer.try_acquire_in_flight() {
        return json_error(
            StatusCode::CONFLICT,
            error_codes::TRANSFER_IN_FLIGHT,
            "another handler is actively processing this transfer",
        );
    }

    // Release in-flight on exit
    let transfer_clone = transfer.clone();
    let _flight_guard = scopeguard::guard((), move |_| {
        transfer_clone.release_in_flight();
    });

    {
        let mut started = transfer.started_at.lock().await;
        *started = Some(std::time::Instant::now());
    }

    // Parse Content-Range header
    let content_range = headers
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let mut offset: i64 = 0;

    if state == TransferState::Transferring && content_range.is_none() {
        return json_error(
            StatusCode::BAD_REQUEST,
            error_codes::MISSING_RANGE_FOR_RESUME,
            "Content-Range header required when resuming a transfer",
        );
    }

    if let Some(ref cr) = content_range {
        match parse_content_range(cr) {
            Ok((start, end, total)) => {
                if total != transfer.file.size {
                    return json_error(
                        StatusCode::BAD_REQUEST,
                        error_codes::RANGE_SIZE_MISMATCH,
                        &format!("Content-Range total {total} != expected file size {}", transfer.file.size),
                    );
                }
                if end != total - 1 {
                    return json_error(
                        StatusCode::BAD_REQUEST,
                        error_codes::RANGE_END_INVALID,
                        "Content-Range end must be total-1 for full remainder",
                    );
                }
                if start < 0 || start > total {
                    return json_error(
                        StatusCode::BAD_REQUEST,
                        error_codes::RANGE_START_INVALID,
                        "Content-Range start out of bounds",
                    );
                }
                offset = start;

                let save_path = transfer.save_path.as_deref().unwrap_or("");
                let actual = super::resume::current_offset(save_path).await;
                if start != actual {
                    return json_error(
                        StatusCode::CONFLICT,
                        error_codes::OFFSET_MISMATCH,
                        &format!("Content-Range start {start} != actual partial size {actual}"),
                    );
                }
            }
            Err(_) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    error_codes::MALFORMED_RANGE,
                    "malformed Content-Range header",
                );
            }
        }
    }

    // Validate Content-Length
    let expected_len = transfer.file.size - offset;
    if let Some(cl) = headers.get("content-length") {
        if let Ok(cl_str) = cl.to_str() {
            if let Ok(cl_val) = cl_str.parse::<i64>() {
                if cl_val > 0 && cl_val != expected_len {
                    return json_error(
                        StatusCode::BAD_REQUEST,
                        error_codes::CONTENT_LENGTH_MISMATCH,
                        &format!("Content-Length {cl_val} != expected remaining {expected_len}"),
                    );
                }
            }
        }
    }

    // Disk space check
    let save_path = transfer.save_path.as_deref().unwrap_or("");
    if let Err(e) = check_disk_space(save_path, expected_len).await {
        let msg = e.to_string();
        manager.fail_transfer(&transfer, error_codes::INSUFFICIENT_DISK_SPACE, &msg);
        return json_error(StatusCode::INSUFFICIENT_STORAGE, error_codes::INSUFFICIENT_DISK_SPACE, &msg);
    }

    // Perform the actual receive
    match manager.receive_file_bytes(&transfer, body, offset, expected_len).await {
        Ok(()) => {
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::empty())
                .unwrap()
        }
        Err(recv_err) => recv_err,
    }
}

/// Parse "bytes start-end/total" format.
fn parse_content_range(s: &str) -> Result<(i64, i64, i64), ()> {
    let s = s.strip_prefix("bytes ").ok_or(())?;
    let (range, total) = s.split_once('/').ok_or(())?;
    let (start, end) = range.split_once('-').ok_or(())?;
    let start: i64 = start.parse().map_err(|_| ())?;
    let end: i64 = end.parse().map_err(|_| ())?;
    let total: i64 = total.parse().map_err(|_| ())?;
    Ok((start, end, total))
}

/// Check disk space at the directory containing save_path.
async fn check_disk_space(save_path: &str, required: i64) -> Result<(), String> {
    let dir = std::path::Path::new(save_path)
        .parent()
        .unwrap_or(std::path::Path::new("/"));

    #[cfg(unix)]
    {
        use nix::sys::statvfs::statvfs;
        match statvfs(dir) {
            Ok(stat) => {
                let available = stat.blocks_available() as i64 * stat.fragment_size() as i64;
                if available < required {
                    return Err(format!(
                        "insufficient disk space: need {required} bytes, have {available} available"
                    ));
                }
            }
            Err(_) => {
                // If we can't stat, don't block the transfer
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = (dir, required);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_content_range_valid() {
        let (start, end, total) = parse_content_range("bytes 52428800-104857599/104857600").unwrap();
        assert_eq!(start, 52428800);
        assert_eq!(end, 104857599);
        assert_eq!(total, 104857600);
    }

    #[test]
    fn parse_content_range_zero() {
        let (start, end, total) = parse_content_range("bytes 0-99/100").unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 99);
        assert_eq!(total, 100);
    }

    #[test]
    fn parse_content_range_invalid() {
        assert!(parse_content_range("invalid").is_err());
        assert!(parse_content_range("bytes abc-def/ghi").is_err());
        assert!(parse_content_range("bytes 0-99").is_err());
    }

    // ══════════════════════════════════════════════════════════════════════
    // Adversarial edge-case tests (Layer 4: Services — Receiver)
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_content_range_missing_bytes_prefix() {
        assert!(parse_content_range("0-99/100").is_err());
    }

    #[test]
    fn parse_content_range_empty_string() {
        assert!(parse_content_range("").is_err());
    }

    #[test]
    fn parse_content_range_negative_values() {
        // parse will fail because i64 parse on "-1" in the start position
        // is actually ambiguous — "bytes -1-99/100" splits on first '-'
        assert!(parse_content_range("bytes -1-99/100").is_err());
    }

    #[test]
    fn parse_content_range_very_large_values() {
        let result = parse_content_range("bytes 0-9223372036854775806/9223372036854775807");
        assert!(result.is_ok());
        let (start, end, total) = result.unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, i64::MAX - 1);
        assert_eq!(total, i64::MAX);
    }

    #[test]
    fn parse_content_range_single_byte() {
        let (start, end, total) = parse_content_range("bytes 0-0/1").unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 0);
        assert_eq!(total, 1);
    }

    #[test]
    fn parse_content_range_extra_spaces() {
        // Should fail - strict parsing expects "bytes " prefix
        assert!(parse_content_range("bytes  0-99/100").is_err());
    }

    #[test]
    fn parse_content_range_no_slash() {
        assert!(parse_content_range("bytes 0-99").is_err());
    }

    #[test]
    fn parse_content_range_no_dash() {
        assert!(parse_content_range("bytes 099/100").is_err());
    }

    #[test]
    fn json_error_produces_valid_response() {
        let resp = json_error(StatusCode::NOT_FOUND, "TEST_ERROR", "test message");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers().get("content-type").unwrap().to_str().unwrap(),
            "application/json"
        );
    }
}
