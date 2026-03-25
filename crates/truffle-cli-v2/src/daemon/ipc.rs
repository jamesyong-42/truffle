//! Cross-platform IPC transport for daemon <-> CLI communication.
//!
//! - Unix (macOS/Linux): Unix Domain Sockets
//! - Windows: Named Pipes

use std::path::PathBuf;

/// Get the IPC endpoint path for this platform.
pub fn ipc_path() -> PathBuf {
    crate::config::TruffleConfig::socket_path()
}

// ==========================================================================
// Unix implementation (macOS / Linux / BSDs)
// ==========================================================================

#[cfg(unix)]
mod platform {
    use std::io;
    use std::path::Path;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    pub struct IpcListener {
        inner: UnixListener,
    }

    impl IpcListener {
        pub fn bind(path: &Path) -> Result<Self, io::Error> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Ok(Self {
                inner: UnixListener::bind(path)?,
            })
        }

        pub async fn accept(&self) -> Result<IpcStream, io::Error> {
            let (stream, _) = self.inner.accept().await?;
            Ok(IpcStream { inner: stream })
        }
    }

    #[derive(Debug)]
    pub struct IpcStream {
        inner: UnixStream,
    }

    impl IpcStream {
        pub async fn connect(path: &Path) -> Result<Self, io::Error> {
            Ok(Self {
                inner: UnixStream::connect(path).await?,
            })
        }

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

    pub struct IpcReadHalf {
        inner: BufReader<tokio::net::unix::OwnedReadHalf>,
    }

    impl IpcReadHalf {
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
    }

    pub struct IpcWriteHalf {
        inner: tokio::net::unix::OwnedWriteHalf,
    }

    impl IpcWriteHalf {
        pub async fn write_line(&mut self, data: &str) -> Result<(), io::Error> {
            self.inner.write_all(data.as_bytes()).await?;
            self.inner.write_all(b"\n").await?;
            self.inner.flush().await
        }
    }
}

// ==========================================================================
// Windows implementation
// ==========================================================================

#[cfg(windows)]
mod platform {
    use std::io;
    use std::path::Path;
    use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

    pub struct IpcListener {
        pipe_name: String,
    }

    impl IpcListener {
        pub fn bind(path: &Path) -> Result<Self, io::Error> {
            let pipe_name = path.to_string_lossy().to_string();
            let _test = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe_name)?;
            Ok(Self { pipe_name })
        }

        pub async fn accept(&self) -> Result<IpcStream, io::Error> {
            let server = ServerOptions::new().create(&self.pipe_name)?;
            server.connect().await?;
            Ok(IpcStream::Server(server))
        }
    }

    #[derive(Debug)]
    pub enum IpcStream {
        Server(tokio::net::windows::named_pipe::NamedPipeServer),
        Client(tokio::net::windows::named_pipe::NamedPipeClient),
    }

    impl IpcStream {
        pub async fn connect(path: &Path) -> Result<Self, io::Error> {
            let pipe_name = path.to_string_lossy().to_string();
            let client = ClientOptions::new().open(&pipe_name)?;
            Ok(Self::Client(client))
        }

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

    pub struct IpcReadHalf {
        inner: BufReader<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
    }

    impl IpcReadHalf {
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
    }

    pub struct IpcWriteHalf {
        inner: Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    }

    impl IpcWriteHalf {
        pub async fn write_line(&mut self, data: &str) -> Result<(), io::Error> {
            use tokio::io::AsyncWriteExt;
            self.inner.write_all(data.as_bytes()).await?;
            self.inner.write_all(b"\n").await?;
            self.inner.flush().await
        }
    }
}

pub use platform::{IpcListener, IpcReadHalf, IpcStream, IpcWriteHalf};
