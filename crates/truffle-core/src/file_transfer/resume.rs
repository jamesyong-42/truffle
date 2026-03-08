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
}
