//! Raw TCP socket bindings (RFC 021 Phase 2).
//!
//! Pull-model handles: byte data crosses the NAPI boundary only as the
//! resolution of a JS-initiated `read()` / `accept()` promise, so JS
//! awaiting each call is the backpressure — no ThreadsafeFunction queues.
//! The `@vibecook/truffle` TS layer wraps these in `stream.Duplex` /
//! `net`-style classes.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Mutex;

use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::transport::RawListener;
use truffle_core::Node;

/// Default read size when the caller does not specify one (64 KiB —
/// matches the file-transfer chunk size).
const DEFAULT_READ_BYTES: u32 = 64 * 1024;

/// Upper bound on a single read allocation (4 MiB).
const MAX_READ_BYTES: u32 = 4 * 1024 * 1024;

/// A raw TCP connection to a peer over the mesh.
///
/// Reads are pull-model: `read()` resolves with the next chunk, `null` on
/// clean EOF. Writes resolve once the bytes are handed to the transport
/// (respecting backpressure). `end()` half-closes the write side (FIN);
/// `close()` tears down both directions. Dropping the JS object without
/// closing also closes the socket (no background tasks are held).
#[napi]
pub struct NapiTcpSocket {
    read: Arc<Mutex<Option<OwnedReadHalf>>>,
    write: Arc<Mutex<Option<OwnedWriteHalf>>>,
    remote_address: String,
    remote_peer_id: Option<String>,
    remote_peer_name: Option<String>,
}

impl NapiTcpSocket {
    /// Wrap a connected `TcpStream` (bridge loopback stream) in a handle.
    pub(crate) fn from_stream(
        stream: tokio::net::TcpStream,
        remote_address: String,
        remote_peer_id: Option<String>,
        remote_peer_name: Option<String>,
    ) -> Self {
        let (read, write) = stream.into_split();
        Self {
            read: Arc::new(Mutex::new(Some(read))),
            write: Arc::new(Mutex::new(Some(write))),
            remote_address,
            remote_peer_id,
            remote_peer_name,
        }
    }
}

#[napi]
impl NapiTcpSocket {
    /// Read up to `maxBytes` (default 64 KiB) from the socket.
    ///
    /// Resolves with the next chunk of data, or `null` on clean EOF (the
    /// peer finished writing) and after `close()`.
    #[napi]
    pub async fn read(&self, max_bytes: Option<u32>) -> Result<Option<Buffer>> {
        let cap = max_bytes
            .unwrap_or(DEFAULT_READ_BYTES)
            .clamp(1, MAX_READ_BYTES) as usize;

        let mut guard = self.read.lock().await;
        let half = match guard.as_mut() {
            Some(half) => half,
            None => return Ok(None), // closed locally
        };

        let mut buf = vec![0u8; cap];
        let n = half
            .read(&mut buf)
            .await
            .map_err(|e| Error::from_reason(format!("tcp read: {e}")))?;
        if n == 0 {
            return Ok(None); // clean EOF
        }
        buf.truncate(n);
        Ok(Some(buf.into()))
    }

    /// Write all of `data` to the socket.
    #[napi]
    pub async fn write(&self, data: Buffer) -> Result<()> {
        let mut guard = self.write.lock().await;
        let half = guard
            .as_mut()
            .ok_or_else(|| Error::from_reason("tcp write: socket closed"))?;
        half.write_all(data.as_ref())
            .await
            .map_err(|e| Error::from_reason(format!("tcp write: {e}")))
    }

    /// Half-close the write side (send FIN), like `socket.end()`.
    ///
    /// Reading remains possible until the peer closes its side. Idempotent.
    #[napi]
    pub async fn end(&self) -> Result<()> {
        let mut guard = self.write.lock().await;
        if let Some(mut half) = guard.take() {
            let _ = half.shutdown().await;
        }
        Ok(())
    }

    /// Fully close the socket (both directions). Idempotent.
    #[napi]
    pub async fn close(&self) -> Result<()> {
        {
            let mut guard = self.write.lock().await;
            if let Some(mut half) = guard.take() {
                let _ = half.shutdown().await;
            }
        }
        {
            let mut guard = self.read.lock().await;
            let _ = guard.take();
        }
        Ok(())
    }

    /// The logical remote address (`host:port` for outbound connections,
    /// the peer's tailnet address for inbound ones).
    #[napi]
    pub fn remote_address(&self) -> String {
        self.remote_address.clone()
    }

    /// The peer's stable id when known — the resolved device id for
    /// outbound connections, the WhoIs node id for inbound ones. `null`
    /// means "anonymous but tailnet-authenticated" (never gate on it).
    #[napi]
    pub fn remote_peer_id(&self) -> Option<String> {
        self.remote_peer_id.clone()
    }

    /// Human-readable peer name from the WhoIs identity (inbound only).
    #[napi]
    pub fn remote_peer_name(&self) -> Option<String> {
        self.remote_peer_name.clone()
    }
}

/// A listener for raw TCP connections on a mesh port.
#[napi]
pub struct NapiTcpListener {
    listener: Arc<Mutex<Option<RawListener>>>,
    node: Arc<Node<TailscaleProvider>>,
    port: u16,
}

impl NapiTcpListener {
    pub(crate) fn new(listener: RawListener, node: Arc<Node<TailscaleProvider>>) -> Self {
        let port = listener.port;
        Self {
            listener: Arc::new(Mutex::new(Some(listener))),
            node,
            port,
        }
    }
}

#[napi]
impl NapiTcpListener {
    /// Accept the next incoming connection.
    ///
    /// Resolves with `null` once the listener has been closed.
    #[napi]
    pub async fn accept(&self) -> Result<Option<NapiTcpSocket>> {
        let mut guard = self.listener.lock().await;
        let listener = match guard.as_mut() {
            Some(listener) => listener,
            None => return Ok(None),
        };

        match listener.accept().await {
            Some(incoming) => {
                let peer_id = incoming
                    .remote_identity
                    .as_ref()
                    .and_then(|i| i.node_id.clone());
                let peer_name = incoming.remote_identity.as_ref().and_then(|i| {
                    i.display_name
                        .clone()
                        .or_else(|| i.login_name.clone())
                        .or_else(|| i.dns_name.clone())
                });
                Ok(Some(NapiTcpSocket::from_stream(
                    incoming.stream,
                    incoming.remote_addr,
                    peer_id,
                    peer_name,
                )))
            }
            None => Ok(None),
        }
    }

    /// The port this listener is bound to (resolved when 0 was requested).
    #[napi]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Close the listener and release the tsnet port in the sidecar.
    /// Pending `accept()` calls resolve with `null`. Idempotent.
    #[napi]
    pub async fn close(&self) -> Result<()> {
        let had_listener = self.listener.lock().await.take().is_some();
        if had_listener {
            if let Err(e) = self.node.unlisten_tcp(self.port).await {
                tracing::debug!(port = self.port, "unlisten_tcp on close: {e}");
            }
        }
        Ok(())
    }
}
