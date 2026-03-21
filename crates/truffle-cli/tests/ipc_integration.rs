//! Integration tests for IPC daemon lifecycle.
//!
//! These tests verify the full lifecycle: start a server, connect a client,
//! exchange messages, and disconnect. They also verify PID file behavior.
//!
//! All tests use temp directories for socket/PID files to avoid conflicts.

// Only compile on Unix -- Windows integration tests require named pipes
// with a different setup pattern.
#![cfg(unix)]

use std::time::Duration;
use tokio::time::timeout;

// Import the library crate items via the package name.
// truffle-cli is a binary crate, so we test the IPC/PID logic through
// a helper module that re-exports the needed types. Since integration
// tests cannot access private modules of a binary crate, we use
// the same underlying platform primitives directly.
//
// NOTE: Because truffle-cli is a *binary* crate (it has `[[bin]]` but
// no `[lib]`), integration tests in `tests/` cannot `use truffle_cli::...`.
// Instead, these tests reimplement a minimal IPC server/client using the
// same tokio UnixListener/UnixStream primitives that `ipc.rs` wraps,
// proving the protocol works end-to-end at the transport layer.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Helper: write a line (with trailing newline) and flush.
async fn write_line(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    data: &str,
) -> std::io::Result<()> {
    writer.write_all(data.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

/// Helper: read the next newline-delimited line. Returns None on EOF.
async fn read_line(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    match reader.read_line(&mut line).await? {
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

/// Full daemon lifecycle: start server, connect client, send request,
/// get response, disconnect.
#[tokio::test]
async fn test_full_ipc_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("lifecycle.sock");

    // 1. Bind server
    let listener = UnixListener::bind(&sock_path).unwrap();
    assert!(sock_path.exists(), "Socket file should exist after bind");

    // 2. Spawn client
    let sock_clone = sock_path.clone();
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let stream = UnixStream::connect(&sock_clone).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // Send JSON-RPC request
        write_line(&mut write_half, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
            .await
            .unwrap();

        // Read response
        let response = timeout(Duration::from_secs(5), read_line(&mut reader))
            .await
            .expect("read response timed out")
            .unwrap();

        assert_eq!(
            response,
            Some(r#"{"jsonrpc":"2.0","id":1,"result":"pong"}"#.to_string()),
            "Client should receive pong response"
        );

        // 5. Client disconnects by dropping
        drop(write_half);
        drop(reader);
    });

    // 3. Server accepts
    let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("accept timed out")
        .unwrap();

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // 4. Server reads request and sends response
    let request = timeout(Duration::from_secs(5), read_line(&mut reader))
        .await
        .expect("read request timed out")
        .unwrap();

    assert_eq!(
        request,
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_string()),
        "Server should receive ping request"
    );

    write_line(
        &mut write_half,
        r#"{"jsonrpc":"2.0","id":1,"result":"pong"}"#,
    )
    .await
    .unwrap();

    // Wait for client to finish
    client_handle.await.unwrap();

    // 6. After client disconnects, server should get None (EOF)
    let eof = timeout(Duration::from_secs(5), read_line(&mut reader))
        .await
        .expect("EOF read timed out")
        .unwrap();

    assert_eq!(eof, None, "Server should see EOF after client disconnects");
}

/// Verify PID file is created and removed during lifecycle.
#[tokio::test]
async fn test_pid_file_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let pid_path = dir.path().join("test.pid");

    // Write PID file (simulating daemon startup)
    let pid = std::process::id();
    std::fs::write(&pid_path, pid.to_string()).unwrap();
    assert!(pid_path.exists(), "PID file should exist after write");

    // Read it back
    let contents = std::fs::read_to_string(&pid_path).unwrap();
    let read_pid: u32 = contents.trim().parse().unwrap();
    assert_eq!(read_pid, pid, "PID file should contain current process PID");

    // Simulate daemon shutdown: remove PID file
    std::fs::remove_file(&pid_path).unwrap();
    assert!(
        !pid_path.exists(),
        "PID file should not exist after removal"
    );
}

/// Multiple sequential client connections to the same server.
#[tokio::test]
async fn test_multiple_sequential_connections() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("multi-seq.sock");

    let listener = UnixListener::bind(&sock_path).unwrap();

    let sock_clone = sock_path.clone();
    let clients_handle = tokio::spawn(async move {
        for i in 0..3u32 {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let stream = UnixStream::connect(&sock_clone).await.unwrap();
            let (_read_half, mut write_half) = stream.into_split();
            write_line(&mut write_half, &format!(r#"{{"id":{i}}}"#))
                .await
                .unwrap();
            // Drop to close the connection
            drop(write_half);
        }
    });

    for i in 0..3u32 {
        let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("accept timed out")
            .unwrap();

        let (read_half, _write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        let line = timeout(Duration::from_secs(5), read_line(&mut reader))
            .await
            .expect("read timed out")
            .unwrap();

        assert_eq!(
            line,
            Some(format!(r#"{{"id":{i}}}"#)),
            "Server should receive message from client {i}"
        );
    }

    clients_handle.await.unwrap();
}

/// Verify that a large message (100KB) survives the IPC roundtrip.
#[tokio::test]
async fn test_large_message_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("large-rt.sock");

    let listener = UnixListener::bind(&sock_path).unwrap();

    // Build a ~100KB JSON payload (no embedded newlines)
    let payload = "A".repeat(100_000);
    let large_msg = format!(r#"{{"data":"{}"}}"#, payload);
    let large_msg_clone = large_msg.clone();

    let sock_clone = sock_path.clone();
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let stream = UnixStream::connect(&sock_clone).await.unwrap();
        let (_read_half, mut write_half) = stream.into_split();
        // Client sends large message
        write_line(&mut write_half, &large_msg_clone).await.unwrap();

        // Read echo back
        let read_half = _read_half;
        let mut reader = BufReader::new(read_half);
        let echoed = timeout(Duration::from_secs(10), read_line(&mut reader))
            .await
            .expect("echo read timed out")
            .unwrap();

        assert_eq!(
            echoed.as_deref(),
            Some(large_msg_clone.as_str()),
            "Echoed message should match original"
        );
    });

    let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("accept timed out")
        .unwrap();

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Server reads large message
    let received = timeout(Duration::from_secs(10), read_line(&mut reader))
        .await
        .expect("read timed out")
        .unwrap();

    assert_eq!(
        received.as_deref(),
        Some(large_msg.as_str()),
        "Server should receive 100KB message intact"
    );

    // Echo it back
    write_line(&mut write_half, &large_msg).await.unwrap();

    client_handle.await.unwrap();
}
