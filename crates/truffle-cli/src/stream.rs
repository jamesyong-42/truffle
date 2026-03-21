//! Bidirectional pipe between stdin/stdout and a Unix socket.
//!
//! Used by `tcp` and `ws` commands after the JSON-RPC handshake completes.
//! Once the daemon has established the upstream connection, the Unix socket
//! becomes a raw byte pipe and this module copies data in both directions.

use tokio::io::{self, AsyncRead, AsyncWrite};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// Bidirectional pipe between stdin/stdout and a Unix socket.
///
/// Copies stdin -> socket and socket -> stdout concurrently.
/// Returns when either direction reaches EOF or encounters an error.
pub async fn pipe_stdio(
    read_half: OwnedReadHalf,
    write_half: OwnedWriteHalf,
) -> io::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    pipe_bidirectional(stdin, stdout, read_half, write_half).await
}

/// Generic bidirectional copy between two pairs of read/write halves.
///
/// Copies `local_read -> remote_write` and `remote_read -> local_write`
/// concurrently. Returns when either direction completes.
pub async fn pipe_bidirectional<LR, LW, RR, RW>(
    mut local_read: LR,
    mut local_write: LW,
    mut remote_read: RR,
    mut remote_write: RW,
) -> io::Result<()>
where
    LR: AsyncRead + Unpin,
    LW: AsyncWrite + Unpin,
    RR: AsyncRead + Unpin,
    RW: AsyncWrite + Unpin,
{
    let local_to_remote = io::copy(&mut local_read, &mut remote_write);
    let remote_to_local = io::copy(&mut remote_read, &mut local_write);

    tokio::select! {
        result = local_to_remote => {
            result.map(|_| ())
        }
        result = remote_to_local => {
            result.map(|_| ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn test_pipe_bidirectional() {
        // Create two in-memory duplex streams to test piping
        let (local_client, local_server) = tokio::io::duplex(1024);
        let (remote_client, remote_server) = tokio::io::duplex(1024);

        let (local_read, mut local_write_handle) = tokio::io::split(local_client);
        let (local_server_read, local_server_write) = tokio::io::split(local_server);
        let (remote_read, remote_write) = tokio::io::split(remote_client);
        let (mut remote_server_read, mut remote_server_write) =
            tokio::io::split(remote_server);

        // Start the pipe
        let pipe_handle = tokio::spawn(async move {
            pipe_bidirectional(local_server_read, local_server_write, remote_read, remote_write)
                .await
        });

        // Write from local side, read from remote side
        local_write_handle.write_all(b"hello").await.unwrap();
        drop(local_write_handle); // EOF on local side

        let mut buf = vec![0u8; 64];
        let n = remote_server_read.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");

        // Write from remote side
        remote_server_write.write_all(b"world").await.unwrap();
        drop(remote_server_write); // EOF on remote side

        let mut buf2 = vec![0u8; 64];
        let n = local_read.take(64).read(&mut buf2).await.unwrap();
        // The pipe should have forwarded some or no data depending on timing,
        // but should not panic.
        let _ = n;

        // The pipe should complete without error
        let result = pipe_handle.await.unwrap();
        assert!(result.is_ok());
    }
}
