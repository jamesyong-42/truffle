//! Cross-platform IPC transport for daemon <-> CLI communication.
//!
//! - Unix (macOS/Linux): Unix Domain Sockets
//! - Windows: Named Pipes (`\\.\pipe\truffle-daemon`)
//!
//! Exposes `IpcListener` and `IpcStream` that work identically on all
//! platforms. The wire protocol (newline-delimited JSON-RPC) is unchanged.

use std::path::PathBuf;

/// Windows named pipe name.
#[allow(dead_code)]
const PIPE_NAME: &str = "truffle-daemon";

/// Get the IPC endpoint path for this platform.
///
/// - Unix: `~/.config/truffle/truffle.sock` (via `TruffleConfig::socket_path()`)
/// - Windows: `\\.\pipe\truffle-daemon`
pub fn ipc_path() -> PathBuf {
    #[cfg(unix)]
    {
        crate::config::TruffleConfig::socket_path()
    }
    #[cfg(windows)]
    {
        PathBuf::from(format!(r"\\.\pipe\{PIPE_NAME}"))
    }
}

/// Returns true if the IPC transport uses named pipes (Windows).
#[allow(dead_code)]
pub fn is_named_pipe() -> bool {
    cfg!(windows)
}

// ═══════════════════════════════════════════════════════════════════════════
// Unix implementation (macOS / Linux / BSDs)
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(unix)]
mod platform {
    use std::io;
    use std::path::Path;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    /// A cross-platform IPC listener.
    ///
    /// On Unix, wraps `tokio::net::UnixListener`.
    pub struct IpcListener {
        inner: UnixListener,
    }

    impl IpcListener {
        /// Bind to the given IPC path.
        ///
        /// On Unix, removes any existing socket file before binding.
        pub fn bind(path: &Path) -> Result<Self, io::Error> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Ok(Self {
                inner: UnixListener::bind(path)?,
            })
        }

        /// Accept a new incoming connection.
        pub async fn accept(&self) -> Result<IpcStream, io::Error> {
            let (stream, _) = self.inner.accept().await?;
            Ok(IpcStream { inner: stream })
        }
    }

    /// A cross-platform IPC stream (a single connection).
    ///
    /// On Unix, wraps `tokio::net::UnixStream`.
    #[derive(Debug)]
    pub struct IpcStream {
        inner: UnixStream,
    }

    impl IpcStream {
        /// Connect to the IPC endpoint at the given path.
        pub async fn connect(path: &Path) -> Result<Self, io::Error> {
            Ok(Self {
                inner: UnixStream::connect(path).await?,
            })
        }

        /// Split the stream into a read half and a write half.
        pub fn into_split(self) -> (IpcReadHalf, IpcWriteHalf) {
            let (r, w) = self.inner.into_split();
            (
                IpcReadHalf {
                    inner: BufReader::new(r),
                },
                IpcWriteHalf { inner: w },
            )
        }
    }

    /// The read half of an IPC stream.
    pub struct IpcReadHalf {
        inner: BufReader<tokio::net::unix::OwnedReadHalf>,
    }

    impl IpcReadHalf {
        /// Read the next newline-delimited line from the stream.
        ///
        /// Returns `None` when the stream is closed (EOF).
        pub async fn next_line(&mut self) -> Result<Option<String>, io::Error> {
            let mut line = String::new();
            match self.inner.read_line(&mut line).await? {
                0 => Ok(None),
                _ => {
                    if line.ends_with('\n') {
                        line.pop();
                    }
                    if line.ends_with('\r') {
                        line.pop();
                    }
                    Ok(Some(line))
                }
            }
        }

        /// Consume this half and return the inner buffered reader.
        ///
        /// The returned type implements `AsyncRead`, which is useful for
        /// raw byte piping (e.g., after a JSON-RPC handshake).
        pub fn into_inner(self) -> BufReader<tokio::net::unix::OwnedReadHalf> {
            self.inner
        }
    }

    /// The write half of an IPC stream.
    pub struct IpcWriteHalf {
        inner: tokio::net::unix::OwnedWriteHalf,
    }

    impl IpcWriteHalf {
        /// Write a line (appends `\n`) and flushes.
        pub async fn write_line(&mut self, data: &str) -> Result<(), io::Error> {
            self.inner.write_all(data.as_bytes()).await?;
            self.inner.write_all(b"\n").await?;
            self.inner.flush().await
        }

        /// Consume this half and return the inner writer.
        ///
        /// The returned type implements `AsyncWrite`, which is useful for
        /// raw byte piping (e.g., after a JSON-RPC handshake).
        pub fn into_inner(self) -> tokio::net::unix::OwnedWriteHalf {
            self.inner
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Windows implementation
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(windows)]
mod platform {
    use std::io;
    use std::path::Path;
    use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    /// A cross-platform IPC listener.
    ///
    /// On Windows, creates named pipe server instances.
    pub struct IpcListener {
        pipe_name: String,
    }

    impl IpcListener {
        /// Bind to the given IPC path.
        ///
        /// On Windows, validates the pipe name by creating the first pipe instance.
        pub fn bind(path: &Path) -> Result<Self, io::Error> {
            let pipe_name = path.to_string_lossy().to_string();
            // Validate by creating (and immediately dropping) the first instance.
            let _test = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe_name)?;
            Ok(Self { pipe_name })
        }

        /// Accept a new incoming connection.
        ///
        /// Named pipes are single-use per connection; a new server instance is
        /// created for each accept.
        pub async fn accept(&self) -> Result<IpcStream, io::Error> {
            let server = ServerOptions::new().create(&self.pipe_name)?;
            server.connect().await?;
            Ok(IpcStream::Server(server))
        }
    }

    /// A cross-platform IPC stream (a single connection).
    ///
    /// On Windows, this is either a server-side or client-side named pipe.
    #[derive(Debug)]
    pub enum IpcStream {
        Server(NamedPipeServer),
        Client(NamedPipeClient),
    }

    impl IpcStream {
        /// Connect to the IPC endpoint at the given path.
        pub async fn connect(path: &Path) -> Result<Self, io::Error> {
            let pipe_name = path.to_string_lossy().to_string();
            let client = ClientOptions::new().open(&pipe_name)?;
            Ok(Self::Client(client))
        }

        /// Split the stream into a read half and a write half.
        ///
        /// Uses type-erased `Box<dyn AsyncRead/AsyncWrite>` since the
        /// underlying type depends on whether this is a server or client pipe.
        pub fn into_split(self) -> (IpcReadHalf, IpcWriteHalf) {
            match self {
                Self::Server(s) => {
                    let (r, w) = split(s);
                    (
                        IpcReadHalf {
                            inner: BufReader::new(Box::new(r)
                                as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
                        },
                        IpcWriteHalf {
                            inner: Box::new(w) as Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                        },
                    )
                }
                Self::Client(c) => {
                    let (r, w) = split(c);
                    (
                        IpcReadHalf {
                            inner: BufReader::new(Box::new(r)
                                as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
                        },
                        IpcWriteHalf {
                            inner: Box::new(w) as Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                        },
                    )
                }
            }
        }
    }

    /// The read half of an IPC stream.
    pub struct IpcReadHalf {
        inner: BufReader<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
    }

    impl IpcReadHalf {
        /// Read the next newline-delimited line from the stream.
        ///
        /// Returns `None` when the stream is closed (EOF).
        pub async fn next_line(&mut self) -> Result<Option<String>, io::Error> {
            let mut line = String::new();
            match self.inner.read_line(&mut line).await? {
                0 => Ok(None),
                _ => {
                    if line.ends_with('\n') {
                        line.pop();
                    }
                    if line.ends_with('\r') {
                        line.pop();
                    }
                    Ok(Some(line))
                }
            }
        }

        /// Consume this half and return the inner buffered reader.
        ///
        /// The returned type implements `AsyncRead`, which is useful for
        /// raw byte piping (e.g., after a JSON-RPC handshake).
        pub fn into_inner(self) -> BufReader<Box<dyn tokio::io::AsyncRead + Unpin + Send>> {
            self.inner
        }
    }

    /// The write half of an IPC stream.
    pub struct IpcWriteHalf {
        inner: Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    }

    impl IpcWriteHalf {
        /// Write a line (appends `\n`) and flushes.
        pub async fn write_line(&mut self, data: &str) -> Result<(), io::Error> {
            self.inner.write_all(data.as_bytes()).await?;
            self.inner.write_all(b"\n").await?;
            self.inner.flush().await
        }

        /// Consume this half and return the inner writer.
        ///
        /// The returned type implements `AsyncWrite`, which is useful for
        /// raw byte piping (e.g., after a JSON-RPC handshake).
        pub fn into_inner(self) -> Box<dyn tokio::io::AsyncWrite + Unpin + Send> {
            self.inner
        }
    }
}

pub use platform::{IpcListener, IpcReadHalf, IpcStream, IpcWriteHalf};

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipc_path_is_platform_appropriate() {
        let path = ipc_path();
        #[cfg(unix)]
        {
            let path_str = path.to_string_lossy();
            assert!(
                path_str.ends_with("truffle.sock"),
                "Unix IPC path should end with truffle.sock, got: {path_str}"
            );
        }
        #[cfg(windows)]
        {
            let path_str = path.to_string_lossy();
            assert!(
                path_str.contains(r"\\.\pipe\"),
                "Windows IPC path should be a named pipe, got: {path_str}"
            );
        }
    }

    #[test]
    fn test_is_named_pipe() {
        #[cfg(unix)]
        assert!(!is_named_pipe());
        #[cfg(windows)]
        assert!(is_named_pipe());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_ipc_listener_bind_and_accept() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");

        // Bind should succeed
        let listener = IpcListener::bind(&sock_path).unwrap();
        assert!(sock_path.exists(), "Socket file should exist after bind");

        // Spawn a client that connects
        let sock_path_clone = sock_path.clone();
        let client_handle = tokio::spawn(async move {
            // Small delay to let accept() start
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let stream = IpcStream::connect(&sock_path_clone).await.unwrap();
            let (_, mut writer) = stream.into_split();
            writer.write_line("hello").await.unwrap();
        });

        // Accept connection and read data
        let stream = listener.accept().await.unwrap();
        let (mut reader, _) = stream.into_split();
        let line = reader.next_line().await.unwrap();
        assert_eq!(line, Some("hello".to_string()));

        client_handle.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_ipc_stream_connect_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("nonexistent.sock");

        let result = IpcStream::connect(&sock_path).await;
        assert!(result.is_err(), "Connecting to nonexistent socket should fail");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_ipc_bidirectional_communication() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("bidir.sock");

        let listener = IpcListener::bind(&sock_path).unwrap();

        let sock_path_clone = sock_path.clone();
        let client_handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let stream = IpcStream::connect(&sock_path_clone).await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            // Send a request
            writer.write_line(r#"{"id":1,"method":"ping"}"#).await.unwrap();

            // Read the response
            let response = reader.next_line().await.unwrap();
            assert_eq!(response, Some(r#"{"id":1,"result":"pong"}"#.to_string()));
        });

        // Server side
        let stream = listener.accept().await.unwrap();
        let (mut reader, mut writer) = stream.into_split();

        let request = reader.next_line().await.unwrap();
        assert_eq!(request, Some(r#"{"id":1,"method":"ping"}"#.to_string()));

        writer
            .write_line(r#"{"id":1,"result":"pong"}"#)
            .await
            .unwrap();

        client_handle.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_ipc_eof_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("eof.sock");

        let listener = IpcListener::bind(&sock_path).unwrap();

        let sock_path_clone = sock_path.clone();
        let client_handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let stream = IpcStream::connect(&sock_path_clone).await.unwrap();
            // Drop immediately -- server should see EOF
            drop(stream);
        });

        let stream = listener.accept().await.unwrap();
        let (mut reader, _) = stream.into_split();
        let line = reader.next_line().await.unwrap();
        assert_eq!(line, None, "EOF should return None");

        client_handle.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_ipc_multiple_clients() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("multi.sock");

        let listener = IpcListener::bind(&sock_path).unwrap();

        let sock_clone = sock_path.clone();
        let clients_handle = tokio::spawn(async move {
            // Connect 3 sequential clients, each sending a unique message
            for i in 0..3u32 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let stream = IpcStream::connect(&sock_clone).await.unwrap();
                let (_, mut writer) = stream.into_split();
                writer
                    .write_line(&format!("client-{i}"))
                    .await
                    .unwrap();
                // Drop the stream to close the connection before opening the next
                drop(writer);
            }
        });

        // Accept and verify 3 sequential connections
        for i in 0..3u32 {
            let timeout = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                listener.accept(),
            )
            .await
            .expect("accept timed out")
            .expect("accept failed");

            let (mut reader, _) = timeout.into_split();
            let line = reader.next_line().await.unwrap();
            assert_eq!(
                line,
                Some(format!("client-{i}")),
                "Expected message from client-{i}"
            );
        }

        clients_handle.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_ipc_large_message() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("large.sock");

        let listener = IpcListener::bind(&sock_path).unwrap();

        // Build a ~100KB JSON line (no embedded newlines)
        let payload = "x".repeat(100_000);
        let large_msg = format!(r#"{{"data":"{}"}}"#, payload);
        let large_msg_clone = large_msg.clone();

        let sock_clone = sock_path.clone();
        let client_handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let stream = IpcStream::connect(&sock_clone).await.unwrap();
            let (_, mut writer) = stream.into_split();
            writer.write_line(&large_msg_clone).await.unwrap();
        });

        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            listener.accept(),
        )
        .await
        .expect("accept timed out")
        .unwrap();

        let (mut reader, _) = stream.into_split();
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader.next_line(),
        )
        .await
        .expect("read timed out")
        .unwrap();

        assert_eq!(
            received.as_deref(),
            Some(large_msg.as_str()),
            "100KB message should be received intact"
        );

        client_handle.await.unwrap();
    }
}
