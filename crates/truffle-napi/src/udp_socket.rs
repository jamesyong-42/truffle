//! Raw UDP datagram bindings (RFC 021 Phase 3).
//!
//! Wraps `DatagramSocket` (relayed through tsnet with boundaries
//! preserved). Pull-model receives; `close()` unblocks any in-flight
//! `recv()` via a notify so sockets never leak a hung read.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use tokio::sync::Notify;

use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::transport::DatagramSocket;
use truffle_core::Node;

/// Receive buffer size: one datagram can be at most 64 KiB.
const RECV_BUF_BYTES: usize = 64 * 1024;

/// A datagram received from the mesh.
#[napi(object)]
pub struct NapiDatagram {
    pub data: Buffer,
    /// Sender's tailnet IP (`100.x.x.x`) — WireGuard-authenticated.
    pub address: String,
    /// Sender's source port.
    pub port: u16,
}

/// A UDP socket on the mesh.
///
/// Datagram boundaries are preserved end-to-end. Keep payloads ≤ ~1,200
/// bytes to stay under the tailnet MTU (larger datagrams fragment in the
/// userspace netstack and become loss-prone). IPv4 (`100.x`) peers only.
#[napi]
pub struct NapiUdpSocket {
    socket: Arc<DatagramSocket>,
    node: Arc<Node<TailscaleProvider>>,
    closed: Arc<AtomicBool>,
    close_notify: Arc<Notify>,
    port: u16,
}

impl NapiUdpSocket {
    pub(crate) fn new(
        socket: DatagramSocket,
        node: Arc<Node<TailscaleProvider>>,
        port: u16,
    ) -> Self {
        Self {
            socket: Arc::new(socket),
            node,
            closed: Arc::new(AtomicBool::new(false)),
            close_notify: Arc::new(Notify::new()),
            port,
        }
    }
}

#[napi]
impl NapiUdpSocket {
    /// Send a datagram to `host:port`.
    ///
    /// `host` accepts a peer's device id (or unique ≥4-char prefix),
    /// device name, Tailscale hostname, or Tailscale IP. Resolves with
    /// the number of payload bytes sent.
    #[napi]
    pub async fn send(&self, data: Buffer, host: String, port: u16) -> Result<u32> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::from_reason("udp send: socket closed"));
        }

        // A literal IP is used verbatim; anything else resolves as a peer
        // reference.
        let ip = match host.parse::<std::net::IpAddr>() {
            Ok(ip) => ip,
            Err(_) => self
                .node
                .resolve_peer_ip(&host)
                .await
                .map_err(|e| Error::from_reason(e.to_string()))?,
        };

        let n = self
            .socket
            .send_to(data.as_ref(), &format!("{ip}:{port}"))
            .await
            .map_err(|e| Error::from_reason(format!("udp send: {e}")))?;
        Ok(n as u32)
    }

    /// Receive the next datagram.
    ///
    /// Resolves `null` once the socket has been closed. Note UDP is
    /// unreliable: datagrams may be dropped or reordered.
    #[napi]
    pub async fn recv(&self) -> Result<Option<NapiDatagram>> {
        // Register for the close signal BEFORE checking the flag, so a
        // concurrent close() can never slip between check and select.
        let notified = self.close_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.closed.load(Ordering::Acquire) {
            return Ok(None);
        }

        let mut buf = vec![0u8; RECV_BUF_BYTES];
        tokio::select! {
            biased;
            _ = &mut notified => Ok(None),
            result = self.socket.recv_from(&mut buf) => {
                let (n, addr) = result
                    .map_err(|e| Error::from_reason(format!("udp recv: {e}")))?;
                buf.truncate(n);
                // DatagramSocket reports the sender as an "ip:port" string.
                let (address, port) = match addr.parse::<std::net::SocketAddr>() {
                    Ok(sock_addr) => (sock_addr.ip().to_string(), sock_addr.port()),
                    Err(_) => (addr, 0),
                };
                Ok(Some(NapiDatagram {
                    data: buf.into(),
                    address,
                    port,
                }))
            }
        }
    }

    /// The mesh port this socket is bound to (the tsnet port over the
    /// relay; 0 for ephemeral client-style sockets whose tsnet port is
    /// unknown — see RFC 021).
    #[napi]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Close the socket. Any in-flight `recv()` resolves with `null`.
    /// Idempotent.
    #[napi]
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.close_notify.notify_waiters();
    }
}
