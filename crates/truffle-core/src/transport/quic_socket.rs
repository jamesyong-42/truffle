//! QUIC socket adapter for tsnet — bridges quinn's [`AsyncUdpSocket`] to
//! [`NetworkUdpSocket`](crate::network::NetworkUdpSocket).
//!
//! # Problem
//!
//! Quinn creates its own UDP socket bound to the host network. When running
//! over tsnet (Tailscale's userspace networking), that socket cannot reach
//! peers because traffic must flow through the Go sidecar's UDP relay.
//!
//! # Solution
//!
//! [`TsnetUdpSocket`] implements [`quinn::AsyncUdpSocket`] on top of our
//! [`NetworkUdpSocket`], which transparently routes datagrams through the
//! tsnet relay. The relay prepends/strips a 6-byte address header
//! (`[4-byte IPv4][2-byte port BE]`) on every datagram.
//!
//! On send, the adapter adds the address header before writing to the relay.
//! On recv, the adapter strips the header, extracts the remote address, and
//! populates quinn's [`RecvMeta`] with the source address.
//!
//! # Limitations
//!
//! - No GSO/GRO — each transmit/receive is a single datagram.
//! - No ECN — the relay does not preserve ECN bits.
//! - IPv4 only — the relay framing is 4-byte IPv4 + 2-byte port.
//! - `may_fragment()` returns `true` — MTUD is disabled.

use std::fmt::Debug;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};

use crate::network::NetworkUdpSocket;

/// Address header size: 4 bytes IPv4 + 2 bytes port (big-endian).
const ADDR_HEADER_SIZE: usize = 6;

// ---------------------------------------------------------------------------
// TsnetUdpSocket
// ---------------------------------------------------------------------------

/// A quinn-compatible UDP socket backed by [`NetworkUdpSocket`] (tsnet relay).
///
/// This adapter translates between quinn's [`AsyncUdpSocket`] trait and our
/// [`NetworkUdpSocket`] which transparently routes datagrams through the
/// Go sidecar's tsnet `ListenPacket` relay.
///
/// # Address framing
///
/// Every datagram through the relay carries a 6-byte header:
///
/// ```text
/// [4-byte IPv4 address][2-byte port big-endian][payload...]
/// ```
///
/// - **Outbound**: the adapter prepends the header with the quinn transmit's
///   destination address.
/// - **Inbound**: the adapter strips the header and reports the source address
///   in [`RecvMeta`].
pub struct TsnetUdpSocket {
    /// The underlying NetworkUdpSocket (connected to the local relay).
    inner: NetworkUdpSocket,
    /// The tsnet-visible local address (Tailscale IP + tsnet port).
    /// Used for quinn's `local_addr()`.
    tsnet_addr: SocketAddr,
}

impl TsnetUdpSocket {
    /// Create a new `TsnetUdpSocket` from a [`NetworkUdpSocket`].
    ///
    /// `tsnet_addr` should be the Tailscale IP + the tsnet-bound port,
    /// i.e. the address that remote peers will use to reach this endpoint.
    pub fn new(inner: NetworkUdpSocket, tsnet_addr: SocketAddr) -> Self {
        Self { inner, tsnet_addr }
    }

    /// Encode the address header for an outbound datagram.
    ///
    /// Returns `Err` if the destination is not IPv4.
    fn encode_header(dest: SocketAddr) -> io::Result<[u8; ADDR_HEADER_SIZE]> {
        let ip = match dest.ip() {
            IpAddr::V4(v4) => v4,
            IpAddr::V6(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "TsnetUdpSocket: IPv6 not supported in relay framing",
                ));
            }
        };
        let port = dest.port();
        let mut header = [0u8; ADDR_HEADER_SIZE];
        header[..4].copy_from_slice(&ip.octets());
        header[4..6].copy_from_slice(&port.to_be_bytes());
        Ok(header)
    }

    /// Decode the address header from an inbound datagram.
    ///
    /// Returns `(source_addr, header_size)`.
    fn decode_header(buf: &[u8]) -> io::Result<(SocketAddr, usize)> {
        if buf.len() < ADDR_HEADER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "TsnetUdpSocket: packet too short for address header ({} < {})",
                    buf.len(),
                    ADDR_HEADER_SIZE
                ),
            ));
        }
        let ip = Ipv4Addr::new(buf[0], buf[1], buf[2], buf[3]);
        let port = u16::from_be_bytes([buf[4], buf[5]]);
        Ok((SocketAddr::new(IpAddr::V4(ip), port), ADDR_HEADER_SIZE))
    }
}

impl Debug for TsnetUdpSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsnetUdpSocket")
            .field("tsnet_addr", &self.tsnet_addr)
            .field("relay_local_addr", &self.inner.inner().local_addr().ok())
            .finish()
    }
}

impl AsyncUdpSocket for TsnetUdpSocket {
    /// Create a poller that wakes when the underlying relay socket is writable.
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(TsnetUdpPoller {
            socket: self,
            fut: None,
        })
    }

    /// Try to send a single datagram through the relay.
    ///
    /// The transmit's `destination` is encoded into the 6-byte address header
    /// and prepended to the payload before sending to the relay socket.
    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        let header = Self::encode_header(transmit.destination)?;

        // Build framed packet: [header][payload]
        let payload = transmit.contents;
        let mut framed = Vec::with_capacity(ADDR_HEADER_SIZE + payload.len());
        framed.extend_from_slice(&header);
        framed.extend_from_slice(payload);

        // Use try_send on the inner tokio UdpSocket (non-blocking).
        // The inner socket is `connect()`ed to the relay, so `try_send` works.
        self.inner.inner().try_send(&framed)?;

        Ok(())
    }

    /// Poll for incoming datagrams from the relay.
    ///
    /// Each datagram from the relay has a 6-byte address header prepended.
    /// We strip it, extract the source address, and fill in [`RecvMeta`].
    ///
    /// Quinn passes `bufs` and `meta` arrays for batch receive. Since our
    /// relay does not support GRO, we fill at most one buffer per call.
    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        // We need a temporary buffer large enough for the header + max payload.
        // Quinn's bufs[0] is the target buffer; we read into a slightly larger
        // temp buffer, then strip the header and copy into bufs[0].
        debug_assert!(!bufs.is_empty());
        debug_assert!(!meta.is_empty());

        let buf_len = bufs[0].len();
        // Allocate temp on the stack if reasonable, otherwise heap.
        // Most QUIC datagrams are <= 1500 bytes. Allow some headroom.
        let tmp_size = ADDR_HEADER_SIZE + buf_len;
        let mut tmp = vec![0u8; tmp_size];

        loop {
            ready!(self.inner.inner().poll_recv_ready(cx))?;

            match self.inner.inner().try_recv(&mut tmp) {
                Ok(n) => {
                    if n < ADDR_HEADER_SIZE {
                        // Runt packet — skip it and try again
                        tracing::warn!(
                            bytes = n,
                            "TsnetUdpSocket: runt packet (< {} bytes), skipping",
                            ADDR_HEADER_SIZE
                        );
                        continue;
                    }

                    let (src_addr, hdr_len) = Self::decode_header(&tmp[..n])?;
                    let payload_len = n - hdr_len;

                    // Copy payload into quinn's buffer
                    let copy_len = payload_len.min(buf_len);
                    bufs[0][..copy_len].copy_from_slice(&tmp[hdr_len..hdr_len + copy_len]);

                    meta[0] = RecvMeta {
                        addr: src_addr,
                        len: copy_len,
                        stride: copy_len,
                        ecn: None, // Relay does not preserve ECN
                        dst_ip: Some(self.tsnet_addr.ip()),
                    };

                    return Poll::Ready(Ok(1));
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // Spurious wakeup — re-register and loop
                    continue;
                }
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }

    /// Return the tsnet-visible local address (Tailscale IP + port).
    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.tsnet_addr)
    }

    /// No GSO — each transmit is a single datagram.
    fn max_transmit_segments(&self) -> usize {
        1
    }

    /// No GRO — each receive is a single datagram.
    fn max_receive_segments(&self) -> usize {
        1
    }

    /// Datagrams may be fragmented (relay handles individual datagrams).
    ///
    /// Returning `true` disables MTUD probing in quinn, which is appropriate
    /// since we go through a local relay and don't control the path MTU.
    fn may_fragment(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// TsnetUdpPoller — write-readiness poller for the relay socket
// ---------------------------------------------------------------------------

/// A [`UdpPoller`] backed by tokio's writable notification on the relay socket.
struct TsnetUdpPoller {
    socket: Arc<TsnetUdpSocket>,
    /// Cached future for the writable() call. We must re-create it after each
    /// Ready, because tokio futures are single-use.
    fut: Option<Pin<Box<dyn std::future::Future<Output = io::Result<()>> + Send + Sync>>>,
}

impl UdpPoller for TsnetUdpPoller {
    fn poll_writable(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        // If we don't have a future yet, create one.
        if self.fut.is_none() {
            let socket = self.socket.clone();
            self.fut = Some(Box::pin(async move {
                socket.inner.inner().writable().await
            }));
        }

        // Poll the future
        let result = self.fut.as_mut().unwrap().as_mut().poll(cx);
        if result.is_ready() {
            // Reset for next use
            self.fut = None;
        }
        result
    }
}

impl Debug for TsnetUdpPoller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsnetUdpPoller").finish_non_exhaustive()
    }
}

// SAFETY: TsnetUdpPoller is Send+Sync because:
// - Arc<TsnetUdpSocket> is Send+Sync
// - The boxed future is Send+Sync (bound in the type)
// The trait requires Send+Sync+Debug+'static which we satisfy.
unsafe impl Send for TsnetUdpPoller {}
unsafe impl Sync for TsnetUdpPoller {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};

    #[test]
    fn encode_header_ipv4() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 1, 2)), 9420);
        let header = TsnetUdpSocket::encode_header(addr).unwrap();
        assert_eq!(header[0], 100);
        assert_eq!(header[1], 64);
        assert_eq!(header[2], 1);
        assert_eq!(header[3], 2);
        assert_eq!(u16::from_be_bytes([header[4], header[5]]), 9420);
    }

    #[test]
    fn encode_header_ipv6_rejected() {
        let addr = SocketAddr::new(IpAddr::V6("::1".parse().unwrap()), 9420);
        let result = TsnetUdpSocket::encode_header(addr);
        assert!(result.is_err());
    }

    #[test]
    fn decode_header_roundtrip() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 100, 50, 3)), 12345);
        let header = TsnetUdpSocket::encode_header(addr).unwrap();
        let (decoded_addr, hdr_len) = TsnetUdpSocket::decode_header(&header).unwrap();
        assert_eq!(decoded_addr, addr);
        assert_eq!(hdr_len, ADDR_HEADER_SIZE);
    }

    #[test]
    fn decode_header_too_short() {
        let result = TsnetUdpSocket::decode_header(&[0, 1, 2]);
        assert!(result.is_err());
    }

    #[test]
    fn decode_header_with_payload() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&[10, 0, 0, 1]); // 10.0.0.1
        buf.extend_from_slice(&8080u16.to_be_bytes()); // port 8080
        buf.extend_from_slice(b"hello"); // payload
        let (addr, hdr_len) = TsnetUdpSocket::decode_header(&buf).unwrap();
        assert_eq!(addr, SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 8080));
        assert_eq!(hdr_len, 6);
        assert_eq!(&buf[hdr_len..], b"hello");
    }

    #[tokio::test]
    async fn tsnet_socket_send_recv_via_loopback_relay() {
        // Simulate a relay: a UDP socket pair where one side acts as the relay
        // and the other is the "inner" socket of NetworkUdpSocket.
        let relay = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay.local_addr().unwrap();

        let inner = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        inner.connect(relay_addr).await.unwrap();
        let inner_addr = inner.local_addr().unwrap();

        // The relay needs to know where to send back — "connect" to inner
        relay.connect(inner_addr).await.unwrap();

        let tsnet_port = 9420;
        let net_socket = NetworkUdpSocket::new(inner, tsnet_port);
        let tsnet_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 1, 1)), tsnet_port);
        let tsnet_socket = Arc::new(TsnetUdpSocket::new(net_socket, tsnet_addr));

        // Simulate sending: the adapter should prepend the address header.
        // We need to wait for the socket to be writable before try_send works
        // (tokio sockets start not-ready). Use poll_fn to drive writability.
        let dest = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 1, 2)), 9420);
        let payload = b"test quic datagram";

        // Wait for writable, then send
        tsnet_socket.inner.inner().writable().await.unwrap();
        let transmit = Transmit {
            destination: dest,
            ecn: None,
            contents: payload,
            segment_size: None,
            src_ip: None,
        };
        tsnet_socket.try_send(&transmit).unwrap();

        // Relay receives the framed datagram
        let mut buf = [0u8; 2048];
        let n = relay.recv(&mut buf).await.unwrap();
        assert_eq!(n, ADDR_HEADER_SIZE + payload.len());

        // Verify header
        assert_eq!(buf[0], 100);
        assert_eq!(buf[1], 64);
        assert_eq!(buf[2], 1);
        assert_eq!(buf[3], 2);
        assert_eq!(u16::from_be_bytes([buf[4], buf[5]]), 9420);
        assert_eq!(&buf[ADDR_HEADER_SIZE..n], payload);

        // Now simulate the relay sending back a framed datagram
        let reply_src = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 1, 2)), 9420);
        let reply_payload = b"reply from peer";
        let mut framed = Vec::new();
        if let IpAddr::V4(v4) = reply_src.ip() {
            framed.extend_from_slice(&v4.octets());
        }
        framed.extend_from_slice(&reply_src.port().to_be_bytes());
        framed.extend_from_slice(reply_payload);

        relay.send(&framed).await.unwrap();

        // The adapter should strip the header and return the payload
        let mut recv_buf = [0u8; 2048];
        let mut ioslice = [IoSliceMut::new(&mut recv_buf)];
        let mut recv_meta = [RecvMeta::default()];

        // We need to poll — use a simple tokio helper
        let count = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            poll_recv_helper(&tsnet_socket, &mut ioslice, &mut recv_meta),
        )
        .await
        .expect("recv timed out");

        assert_eq!(count, 1);
        assert_eq!(recv_meta[0].addr, reply_src);
        assert_eq!(recv_meta[0].len, reply_payload.len());
        assert_eq!(&recv_buf[..recv_meta[0].len], reply_payload);
    }

    /// Helper to drive poll_recv to completion.
    async fn poll_recv_helper(
        socket: &Arc<TsnetUdpSocket>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> usize {
        std::future::poll_fn(|cx| socket.poll_recv(cx, bufs, meta)).await.unwrap()
    }
}
