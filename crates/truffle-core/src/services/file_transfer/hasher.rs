use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;
use sha2::{Digest, Sha256};
use tokio::io::AsyncRead;

pin_project! {
    /// AsyncRead wrapper that feeds all bytes through a SHA-256 hasher.
    ///
    /// Equivalent to Go's `io.TeeReader(body, sha256.New())`.
    /// Uses pin-project-lite for safe pin projection.
    pub struct HashingReader<R> {
        #[pin]
        inner: R,
        hasher: Sha256,
    }
}

impl<R> HashingReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    /// Feed existing data into the hasher (for resuming).
    /// Call this before reading new data to rebuild SHA-256 state.
    pub fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    /// Finalize and return the hex-encoded SHA-256 hash.
    pub fn finalize(self) -> String {
        hex::encode(self.hasher.finalize())
    }
}

impl<R: AsyncRead> AsyncRead for HashingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.project();

        let before = buf.filled().len();
        let result = this.inner.poll_read(cx, buf);
        let after = buf.filled().len();

        if after > before {
            this.hasher.update(&buf.filled()[before..after]);
        }

        result
    }
}

/// Compute SHA-256 of a file, streaming in chunks.
/// Calls `on_progress` with bytes hashed so far, rate-limited.
pub async fn hash_file(
    path: &str,
    _total_size: i64,
    on_progress: Option<&(dyn Fn(i64) + Send + Sync)>,
) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 32 * 1024]; // 32KB chunks
    let mut hashed: i64 = 0;
    let mut last_report: i64 = 0;
    let mut last_report_time = std::time::Instant::now();

    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        hasher.update(&buf[..n]);
        hashed += n as i64;

        if let Some(on_progress) = on_progress {
            let now = std::time::Instant::now();
            if hashed - last_report >= 256 * 1024
                || now.duration_since(last_report_time).as_millis() >= 200
            {
                on_progress(hashed);
                last_report = hashed;
                last_report_time = now;
            }
        }
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Hash the first `length` bytes of a partial file into an existing hasher.
/// Used to rebuild SHA-256 state when resuming a transfer.
pub async fn hash_partial_file(path: &str, length: i64, hasher: &mut Sha256) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 32 * 1024];
    let mut remaining = length;

    while remaining > 0 {
        let to_read = std::cmp::min(remaining as usize, buf.len());
        let n = file.read(&mut buf[..to_read]).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        remaining -= n as i64;
    }

    if remaining > 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("partial file shorter than expected: {remaining} bytes remaining"),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use tempfile::TempDir;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn hashing_reader_computes_hash() {
        let data = b"hello world";
        let reader = tokio_util::io::StreamReader::new(tokio_stream::once(Ok::<
            _,
            std::io::Error,
        >(
            bytes::Bytes::from_static(data),
        )));

        let mut hashing = HashingReader::new(reader);
        let mut buf = vec![0u8; 64];
        let n = hashing.read(&mut buf).await.unwrap();
        assert_eq!(n, 11);

        let hash = hashing.finalize();

        // Verify against direct sha256
        let mut expected = Sha256::new();
        expected.update(data);
        let expected_hash = hex::encode(expected.finalize());
        assert_eq!(hash, expected_hash);
    }

    #[tokio::test]
    async fn hashing_reader_resume_with_update() {
        let part1 = b"hello ";
        let part2 = b"world";

        // Simulate resume: feed part1 via update, then read part2
        let reader = tokio_util::io::StreamReader::new(tokio_stream::once(Ok::<
            _,
            std::io::Error,
        >(
            bytes::Bytes::from_static(part2),
        )));

        let mut hashing = HashingReader::new(reader);
        hashing.update(part1);

        let mut buf = vec![0u8; 64];
        let _ = hashing.read(&mut buf).await.unwrap();
        let hash = hashing.finalize();

        // Should equal hash of "hello world"
        let mut expected = Sha256::new();
        expected.update(b"hello world");
        let expected_hash = hex::encode(expected.finalize());
        assert_eq!(hash, expected_hash);
    }

    #[tokio::test]
    async fn hash_file_computes_correctly() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.bin");
        let data = b"test file content for hashing";
        tokio::fs::write(&path, data).await.unwrap();

        let hash = hash_file(path.to_str().unwrap(), data.len() as i64, None)
            .await
            .unwrap();

        let mut expected = Sha256::new();
        expected.update(data);
        let expected_hash = hex::encode(expected.finalize());
        assert_eq!(hash, expected_hash);
    }

    #[tokio::test]
    async fn hash_file_with_progress() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("large.bin");
        let data = vec![42u8; 1024 * 1024]; // 1MB
        tokio::fs::write(&path, &data).await.unwrap();

        let progress_called = std::sync::atomic::AtomicBool::new(false);
        let hash = hash_file(path.to_str().unwrap(), data.len() as i64, Some(&|_bytes| {
            progress_called.store(true, std::sync::atomic::Ordering::SeqCst);
        }))
        .await
        .unwrap();

        assert!(progress_called.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!hash.is_empty());
    }

    #[tokio::test]
    async fn hash_partial_file_works() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("partial.bin");
        let data = b"first part second part";
        tokio::fs::write(&path, data).await.unwrap();

        let mut hasher = Sha256::new();
        hash_partial_file(path.to_str().unwrap(), 11, &mut hasher)
            .await
            .unwrap();

        // Feed the rest
        hasher.update(b"second part");

        let hash = hex::encode(hasher.finalize());

        let mut expected = Sha256::new();
        expected.update(data);
        let expected_hash = hex::encode(expected.finalize());
        assert_eq!(hash, expected_hash);
    }

    // ══════════════════════════════════════════════════════════════════════
    // Adversarial edge-case tests (Layer 4: Services — Hasher)
    // ══════════════════════════════════════════════════════════════════════

    // ── 20. Hash empty file ──────────────────────────────────────────────
    #[tokio::test]
    async fn hash_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.bin");
        tokio::fs::write(&path, b"").await.unwrap();

        let hash = hash_file(path.to_str().unwrap(), 0, None)
            .await
            .unwrap();

        // Known SHA-256 of empty input
        let expected = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(hash, expected, "SHA-256 of empty input must match known value");
    }

    // ── 20b. HashingReader with zero-length input ────────────────────────
    #[tokio::test]
    async fn hashing_reader_empty_input() {
        let reader = tokio_util::io::StreamReader::new(tokio_stream::once(Ok::<
            _,
            std::io::Error,
        >(
            bytes::Bytes::new(), // empty
        )));

        let mut hashing = HashingReader::new(reader);
        let mut buf = vec![0u8; 64];
        let n = hashing.read(&mut buf).await.unwrap();
        assert_eq!(n, 0, "Empty input should read 0 bytes");

        let hash = hashing.finalize();
        let expected = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(hash, expected);
    }

    // ── 21. Hash consistency ─────────────────────────────────────────────
    #[tokio::test]
    async fn hash_consistency_same_data_twice() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("consistency.bin");
        let data = b"deterministic hash test data 1234567890";
        tokio::fs::write(&path, data).await.unwrap();

        let hash1 = hash_file(path.to_str().unwrap(), data.len() as i64, None)
            .await
            .unwrap();
        let hash2 = hash_file(path.to_str().unwrap(), data.len() as i64, None)
            .await
            .unwrap();

        assert_eq!(hash1, hash2, "Hashing same data twice must produce identical results");
    }

    // ── 21b. HashingReader consistency ────────────────────────────────────
    #[tokio::test]
    async fn hashing_reader_consistency() {
        let data = b"consistent hash data";

        let mut hashes = Vec::new();
        for _ in 0..3 {
            let reader = tokio_util::io::StreamReader::new(tokio_stream::once(Ok::<
                _,
                std::io::Error,
            >(
                bytes::Bytes::from_static(data),
            )));

            let mut hashing = HashingReader::new(reader);
            let mut buf = vec![0u8; 64];
            let _ = hashing.read(&mut buf).await.unwrap();
            hashes.push(hashing.finalize());
        }

        assert_eq!(hashes[0], hashes[1]);
        assert_eq!(hashes[1], hashes[2]);
    }

    // ── 22. Partial hash matches full file's prefix ──────────────────────
    #[tokio::test]
    async fn partial_hash_matches_prefix_of_full_hash() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("twopart.bin");

        // Write 2KB file
        let data: Vec<u8> = (0..2048).map(|i| (i % 256) as u8).collect();
        tokio::fs::write(&path, &data).await.unwrap();

        // Hash first 1024 bytes via hash_partial_file
        let mut partial_hasher = Sha256::new();
        hash_partial_file(path.to_str().unwrap(), 1024, &mut partial_hasher)
            .await
            .unwrap();

        // Feed the remaining 1024 bytes manually
        partial_hasher.update(&data[1024..]);
        let combined_hash = hex::encode(partial_hasher.finalize());

        // Hash entire file
        let full_hash = hash_file(path.to_str().unwrap(), data.len() as i64, None)
            .await
            .unwrap();

        assert_eq!(
            combined_hash, full_hash,
            "Partial hash + remainder must equal full file hash"
        );
    }

    // ── Edge: hash_partial_file with file shorter than expected ───────────
    #[tokio::test]
    async fn hash_partial_file_shorter_than_expected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("short.bin");
        tokio::fs::write(&path, b"short").await.unwrap();

        let mut hasher = Sha256::new();
        let result = hash_partial_file(path.to_str().unwrap(), 100, &mut hasher).await;
        assert!(result.is_err(), "Should fail if file is shorter than requested length");
        assert!(
            result.unwrap_err().to_string().contains("shorter than expected"),
            "Error should mention truncation"
        );
    }

    // ── Edge: hash_file on nonexistent file ──────────────────────────────
    #[tokio::test]
    async fn hash_file_nonexistent() {
        let result = hash_file("/nonexistent/path/file.bin", 100, None).await;
        assert!(result.is_err());
    }

    // ── Edge: HashingReader with multiple reads (chunked) ────────────────
    #[tokio::test]
    async fn hashing_reader_chunked_reads() {
        // Build a multi-chunk stream
        let chunk1 = bytes::Bytes::from_static(b"hello ");
        let chunk2 = bytes::Bytes::from_static(b"world");

        let stream = tokio_stream::iter(vec![
            Ok::<_, std::io::Error>(chunk1),
            Ok::<_, std::io::Error>(chunk2),
        ]);
        let reader = tokio_util::io::StreamReader::new(stream);

        let mut hashing = HashingReader::new(reader);
        let mut all_data = Vec::new();
        let mut buf = vec![0u8; 4]; // Small buffer to force multiple reads
        loop {
            let n = hashing.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            all_data.extend_from_slice(&buf[..n]);
        }

        assert_eq!(all_data, b"hello world");

        let hash = hashing.finalize();
        let mut expected = Sha256::new();
        expected.update(b"hello world");
        let expected_hash = hex::encode(expected.finalize());
        assert_eq!(hash, expected_hash, "Chunked reads must produce same hash as single read");
    }
}
