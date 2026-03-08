use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use pin_project_lite::pin_project;
use tokio::io::AsyncRead;
use tokio::sync::Mutex;

use super::types::FileTransferConfig;

/// Callback type for progress reporting.
pub type ProgressCallback = Arc<dyn Fn(i64) + Send + Sync>;

pin_project! {
    /// AsyncRead wrapper that reports progress via a callback,
    /// rate-limited by both time interval and byte threshold.
    pub struct ProgressReader<R> {
        #[pin]
        inner: R,
        total: i64,
        offset: i64,
        read: AtomicI64,
        callback: ProgressCallback,
        progress_interval_ms: u64,
        progress_bytes: i64,
        last_callback_at: Mutex<Instant>,
        last_callback_bytes: AtomicI64,
    }
}

impl<R> ProgressReader<R> {
    pub fn new(
        inner: R,
        total: i64,
        offset: i64,
        callback: ProgressCallback,
        config: &FileTransferConfig,
    ) -> Self {
        Self {
            inner,
            total,
            offset,
            read: AtomicI64::new(0),
            callback,
            progress_interval_ms: config.progress_interval.as_millis() as u64,
            progress_bytes: config.progress_bytes,
            last_callback_at: Mutex::new(Instant::now()),
            last_callback_bytes: AtomicI64::new(0),
        }
    }

    /// Get total bytes read so far (including offset).
    pub fn total_read(&self) -> i64 {
        self.offset + self.read.load(Ordering::Relaxed)
    }
}

impl<R: AsyncRead> AsyncRead for ProgressReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.project();

        let before = buf.filled().len();
        let result = this.inner.poll_read(cx, buf);
        let after = buf.filled().len();
        let n = (after - before) as i64;

        if n > 0 {
            this.read.fetch_add(n, Ordering::Relaxed);
            let total_read = *this.offset + this.read.load(Ordering::Relaxed);

            // Check if we should report progress.
            // Use try_lock to avoid blocking the hot path.
            let should_report = if let Ok(mut last_at) = this.last_callback_at.try_lock() {
                let now = Instant::now();
                let elapsed_ms = now.duration_since(*last_at).as_millis() as u64;
                let bytes_since = total_read - this.last_callback_bytes.load(Ordering::Relaxed);

                if elapsed_ms >= *this.progress_interval_ms || bytes_since >= *this.progress_bytes {
                    *last_at = now;
                    this.last_callback_bytes.store(total_read, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if should_report {
                (this.callback)(total_read);
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI64 as StdAtomicI64;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn progress_reader_reports() {
        let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(
            tokio_stream::once(Ok::<_, std::io::Error>(bytes::Bytes::from(vec![0u8; 1024]))),
        ));

        let reported = Arc::new(StdAtomicI64::new(0));
        let reported_clone = reported.clone();

        let mut config = FileTransferConfig::default();
        config.progress_bytes = 100; // Report every 100 bytes
        config.progress_interval = std::time::Duration::from_millis(0); // No time throttle

        let callback: ProgressCallback = Arc::new(move |bytes| {
            reported_clone.store(bytes, Ordering::SeqCst);
        });

        let mut progress = ProgressReader::new(reader, 1024, 0, callback, &config);

        let mut buf = vec![0u8; 2048];
        let n = progress.read(&mut buf).await.unwrap();
        assert_eq!(n, 1024);

        // Should have reported at least once
        assert!(reported.load(Ordering::SeqCst) > 0);
    }

    #[tokio::test]
    async fn progress_reader_with_offset() {
        let reader = tokio_util::io::StreamReader::new(tokio_stream::once(Ok::<
            _,
            std::io::Error,
        >(
            bytes::Bytes::from(vec![1u8; 512]),
        )));

        let reported = Arc::new(StdAtomicI64::new(0));
        let reported_clone = reported.clone();

        let mut config = FileTransferConfig::default();
        config.progress_bytes = 1;
        config.progress_interval = std::time::Duration::from_millis(0);

        let callback: ProgressCallback = Arc::new(move |bytes| {
            reported_clone.store(bytes, Ordering::SeqCst);
        });

        let mut progress = ProgressReader::new(reader, 1024, 512, callback, &config);

        let mut buf = vec![0u8; 1024];
        let _ = progress.read(&mut buf).await.unwrap();

        // Should report offset + bytes_read = 512 + 512 = 1024
        assert_eq!(reported.load(Ordering::SeqCst), 1024);
        assert_eq!(progress.total_read(), 1024);
    }
}
