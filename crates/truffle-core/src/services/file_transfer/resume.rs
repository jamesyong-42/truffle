use serde::{Deserialize, Serialize};

use super::types::FileInfo;

/// Metadata about an in-progress transfer for crash recovery.
/// Stored as a `.partial.meta` JSON file alongside the `.partial` file.
///
/// The `.partial` file size on disk is the authoritative resume offset.
/// This metadata is advisory -- it stores context for re-registering
/// a transfer after a crash.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialMeta {
    pub transfer_id: String,
    pub file: FileInfo,
    pub save_path: String,
    pub offset: i64,
}

/// Write a `.partial.meta` JSON file alongside the partial file.
pub async fn write_partial_meta(
    save_path: &str,
    transfer_id: &str,
    file: &FileInfo,
    offset: i64,
) -> std::io::Result<()> {
    let meta = PartialMeta {
        transfer_id: transfer_id.to_string(),
        file: file.clone(),
        save_path: save_path.to_string(),
        offset,
    };

    let data = serde_json::to_string_pretty(&meta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    tokio::fs::write(format!("{save_path}.partial.meta"), data).await
}

/// Read a `.partial.meta` file, if it exists.
pub async fn read_partial_meta(save_path: &str) -> Option<PartialMeta> {
    let data = tokio::fs::read_to_string(format!("{save_path}.partial.meta"))
        .await
        .ok()?;
    serde_json::from_str(&data).ok()
}

/// Get the current offset (size of the .partial file) for a transfer.
/// Returns 0 if no partial file exists.
pub async fn current_offset(save_path: &str) -> i64 {
    match tokio::fs::metadata(format!("{save_path}.partial")).await {
        Ok(meta) => meta.len() as i64,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn write_and_read_partial_meta() {
        let dir = TempDir::new().unwrap();
        let save_path = dir.path().join("test_file.bin");
        let save_path_str = save_path.to_str().unwrap();

        let file_info = FileInfo {
            name: "test_file.bin".to_string(),
            size: 1024,
            sha256: "abc123".to_string(),
        };

        write_partial_meta(save_path_str, "ft-test-1", &file_info, 512)
            .await
            .unwrap();

        let meta = read_partial_meta(save_path_str).await.unwrap();
        assert_eq!(meta.transfer_id, "ft-test-1");
        assert_eq!(meta.file.name, "test_file.bin");
        assert_eq!(meta.offset, 512);
    }

    #[tokio::test]
    async fn read_partial_meta_missing() {
        let result = read_partial_meta("/nonexistent/path").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn current_offset_no_partial() {
        let offset = current_offset("/nonexistent/path").await;
        assert_eq!(offset, 0);
    }

    #[tokio::test]
    async fn current_offset_with_partial() {
        let dir = TempDir::new().unwrap();
        let save_path = dir.path().join("test_file.bin");
        let partial_path = format!("{}.partial", save_path.to_str().unwrap());

        // Write 512 bytes to the partial file
        tokio::fs::write(&partial_path, vec![0u8; 512])
            .await
            .unwrap();

        let offset = current_offset(save_path.to_str().unwrap()).await;
        assert_eq!(offset, 512);
    }

    // ══════════════════════════════════════════════════════════════════════
    // Adversarial edge-case tests (Layer 4: Services — Resume)
    // ══════════════════════════════════════════════════════════════════════

    // ── 23. Resume with no prior state (offset 0) ────────────────────────
    #[tokio::test]
    async fn resume_no_prior_state_returns_offset_zero() {
        let dir = TempDir::new().unwrap();
        let save_path = dir.path().join("fresh_transfer.bin");

        // No .partial file exists
        let offset = current_offset(save_path.to_str().unwrap()).await;
        assert_eq!(offset, 0, "No partial file means offset 0");

        // No .partial.meta file either
        let meta = read_partial_meta(save_path.to_str().unwrap()).await;
        assert!(meta.is_none(), "No meta file means None");
    }

    // ── 24. Resume with complete file (partial = full size) ──────────────
    #[tokio::test]
    async fn resume_complete_file_returns_full_offset() {
        let dir = TempDir::new().unwrap();
        let save_path = dir.path().join("complete_transfer.bin");
        let partial_path = format!("{}.partial", save_path.to_str().unwrap());

        let file_size: i64 = 4096;
        // Write a partial file that is the full size of the transfer
        tokio::fs::write(&partial_path, vec![0u8; file_size as usize])
            .await
            .unwrap();

        let offset = current_offset(save_path.to_str().unwrap()).await;
        assert_eq!(offset, file_size, "Offset should equal file size when fully received");

        // Also test that meta can track this
        let file_info = FileInfo {
            name: "complete_transfer.bin".to_string(),
            size: file_size,
            sha256: "deadbeef".to_string(),
        };
        write_partial_meta(
            save_path.to_str().unwrap(),
            "ft-complete-1",
            &file_info,
            file_size,
        )
        .await
        .unwrap();

        let meta = read_partial_meta(save_path.to_str().unwrap()).await.unwrap();
        assert_eq!(meta.offset, file_size);
        assert_eq!(meta.transfer_id, "ft-complete-1");
    }

    // ── 25. Resume with corrupted partial (detect via hash mismatch) ─────
    // Note: current_offset itself doesn't validate hashes; it just returns
    // the file size. The hash validation is done by the receiver. This test
    // verifies that a corrupted partial still returns its size as offset,
    // and that the meta can be read independently.
    #[tokio::test]
    async fn resume_corrupted_partial_returns_file_size() {
        let dir = TempDir::new().unwrap();
        let save_path = dir.path().join("corrupted.bin");
        let partial_path = format!("{}.partial", save_path.to_str().unwrap());

        // Write "corrupted" partial data (doesn't match expected hash)
        let corrupted_data = vec![0xFF; 512];
        tokio::fs::write(&partial_path, &corrupted_data).await.unwrap();

        // current_offset doesn't validate hash, returns size
        let offset = current_offset(save_path.to_str().unwrap()).await;
        assert_eq!(offset, 512, "current_offset returns file size regardless of content");

        // Write meta referencing a different expected hash
        let file_info = FileInfo {
            name: "corrupted.bin".to_string(),
            size: 1024,
            sha256: "expected_hash_that_wont_match".to_string(),
        };
        write_partial_meta(
            save_path.to_str().unwrap(),
            "ft-corrupt-1",
            &file_info,
            512,
        )
        .await
        .unwrap();

        // The meta is independently readable
        let meta = read_partial_meta(save_path.to_str().unwrap()).await.unwrap();
        assert_eq!(meta.offset, 512);
        assert_eq!(meta.file.sha256, "expected_hash_that_wont_match");
    }

    // ── Edge: write_partial_meta with empty path components ──────────────
    #[tokio::test]
    async fn partial_meta_roundtrip_with_special_chars() {
        let dir = TempDir::new().unwrap();
        let save_path = dir.path().join("file with spaces.bin");
        let save_path_str = save_path.to_str().unwrap();

        let file_info = FileInfo {
            name: "file with spaces.bin".to_string(),
            size: 999,
            sha256: "abc".to_string(),
        };

        write_partial_meta(save_path_str, "ft-special-1", &file_info, 100)
            .await
            .unwrap();

        let meta = read_partial_meta(save_path_str).await.unwrap();
        assert_eq!(meta.file.name, "file with spaces.bin");
        assert_eq!(meta.transfer_id, "ft-special-1");
    }

    // ── Edge: current_offset with zero-length partial file ───────────────
    #[tokio::test]
    async fn current_offset_zero_length_partial() {
        let dir = TempDir::new().unwrap();
        let save_path = dir.path().join("zero.bin");
        let partial_path = format!("{}.partial", save_path.to_str().unwrap());

        // Create empty partial file
        tokio::fs::write(&partial_path, b"").await.unwrap();

        let offset = current_offset(save_path.to_str().unwrap()).await;
        assert_eq!(offset, 0, "Zero-length partial returns 0 offset");
    }

    // ── Edge: read_partial_meta with corrupted JSON ──────────────────────
    #[tokio::test]
    async fn read_partial_meta_corrupted_json() {
        let dir = TempDir::new().unwrap();
        let save_path = dir.path().join("bad_meta.bin");
        let meta_path = format!("{}.partial.meta", save_path.to_str().unwrap());

        // Write invalid JSON
        tokio::fs::write(&meta_path, b"not valid json {{{").await.unwrap();

        let meta = read_partial_meta(save_path.to_str().unwrap()).await;
        assert!(meta.is_none(), "Corrupted JSON should return None");
    }
}
