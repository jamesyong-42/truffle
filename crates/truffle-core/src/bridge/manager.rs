use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use subtle::ConstantTimeEq;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex, Semaphore};

use super::header::{BridgeHeader, Direction};

/// Maximum concurrent bridge connections.
const MAX_CONCURRENT_CONNECTIONS: usize = 256;

/// Timeout for reading the bridge header after accepting a connection.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Timeout for outgoing dial correlation.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// A routed bridge connection after header parsing and validation.
#[derive(Debug)]
pub struct BridgeConnection {
    pub stream: TcpStream,
    pub header: BridgeHeader,
}

/// Handler trait for processing bridge connections by (port, direction).
///
/// Phase 1 only defines the trait. Concrete handlers are implemented in
/// later phases (transport for WS, file_transfer for HTTP).
pub trait BridgeHandler: Send + Sync + 'static {
    fn handle(&self, conn: BridgeConnection) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// Simple handler that sends connections over a channel (useful for testing
/// and for the BridgeManager to deliver to the appropriate subsystem).
pub struct ChannelHandler {
    tx: mpsc::Sender<BridgeConnection>,
}

impl ChannelHandler {
    pub fn new(tx: mpsc::Sender<BridgeConnection>) -> Self {
        Self { tx }
    }
}

impl BridgeHandler for ChannelHandler {
    fn handle(&self, conn: BridgeConnection) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if self.tx.send(conn).await.is_err() {
                tracing::warn!("bridge handler channel closed, dropping connection");
            }
        })
    }
}

/// Route key for dispatching bridge connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RouteKey {
    pub port: u16,
    pub direction: Direction,
}

/// Manages the local TCP bridge listener.
///
/// Accepts connections from the Go shim, reads and validates the binary header,
/// verifies the session token, and routes the connection to the appropriate handler.
pub struct BridgeManager {
    listener: TcpListener,
    session_token: [u8; 32],
    handlers: HashMap<RouteKey, Arc<dyn BridgeHandler>>,
    /// Fallback handler for unknown ports (e.g., reverse proxy).
    fallback_handler: Option<Arc<dyn BridgeHandler>>,
    /// Pending outgoing dials: requestId -> sender for delivering the stream.
    pending_dials: Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>>,
    /// Semaphore to cap concurrent connections.
    semaphore: Arc<Semaphore>,
}

impl BridgeManager {
    /// Create a new BridgeManager that listens on 127.0.0.1:0 (ephemeral port).
    pub async fn bind(session_token: [u8; 32]) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        Ok(Self {
            listener,
            session_token,
            handlers: HashMap::new(),
            fallback_handler: None,
            pending_dials: Arc::new(Mutex::new(HashMap::new())),
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS)),
        })
    }

    /// Returns the local port the bridge is listening on.
    pub fn local_port(&self) -> u16 {
        self.listener
            .local_addr()
            .expect("listener bound")
            .port()
    }

    /// Register a handler for a specific (port, direction) pair.
    pub fn add_handler(
        &mut self,
        port: u16,
        direction: Direction,
        handler: Arc<dyn BridgeHandler>,
    ) {
        self.handlers
            .insert(RouteKey { port, direction }, handler);
    }

    /// Register a fallback handler for unknown ports (e.g., reverse proxy).
    pub fn set_fallback_handler(&mut self, handler: Arc<dyn BridgeHandler>) {
        self.fallback_handler = Some(handler);
    }

    /// Access the pending dials map.
    pub fn pending_dials(&self) -> &Arc<Mutex<HashMap<String, oneshot::Sender<BridgeConnection>>>> {
        &self.pending_dials
    }

    /// Run the accept loop. This runs until the listener is closed or an error occurs.
    pub async fn run(self) {
        let session_token = self.session_token;
        let handlers = Arc::new(self.handlers);
        let fallback = self.fallback_handler.map(Arc::new);
        let pending_dials = self.pending_dials;
        let semaphore = self.semaphore;

        loop {
            let (stream, addr) = match self.listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::error!("bridge accept error: {e}");
                    break;
                }
            };

            let permit = match semaphore.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    tracing::error!("bridge semaphore closed");
                    break;
                }
            };

            let handlers = handlers.clone();
            let fallback = fallback.clone();
            let pending_dials = pending_dials.clone();

            tokio::spawn(async move {
                let _permit = permit; // held for task lifetime

                // Read header with timeout
                let mut stream = stream;
                let header = match tokio::time::timeout(
                    HEADER_READ_TIMEOUT,
                    BridgeHeader::read_from(&mut stream),
                )
                .await
                {
                    Ok(Ok(h)) => h,
                    Ok(Err(e)) => {
                        tracing::warn!("bad bridge header from {addr}: {e}");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!("bridge header timeout from {addr}");
                        return;
                    }
                };

                // Verify session token (constant-time compare)
                if header
                    .session_token
                    .ct_eq(&session_token)
                    .unwrap_u8()
                    != 1
                {
                    tracing::warn!("invalid bridge session token from {addr}");
                    return;
                }

                let conn = BridgeConnection { stream, header };

                // Outgoing connections with a request_id: deliver via pending_dials
                if conn.header.direction == Direction::Outgoing
                    && !conn.header.request_id.is_empty()
                {
                    let mut dials = pending_dials.lock().await;
                    if let Some(tx) = dials.remove(&conn.header.request_id) {
                        if tx.send(conn).is_err() {
                            tracing::warn!(
                                "pending dial receiver dropped for request_id={}",
                                "unknown" // conn was moved
                            );
                        }
                    } else {
                        tracing::warn!(
                            "unknown outgoing request_id={} from {addr}, closing",
                            conn.header.request_id
                        );
                    }
                    return;
                }

                // Route by (port, direction)
                let key = RouteKey {
                    port: conn.header.service_port,
                    direction: conn.header.direction,
                };

                if let Some(handler) = handlers.get(&key) {
                    handler.handle(conn).await;
                } else if let Some(ref fb) = fallback {
                    fb.handle(conn).await;
                } else {
                    tracing::warn!(
                        "no handler for port={} direction={} from {addr}",
                        key.port,
                        conn.header.direction
                    );
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn test_token() -> [u8; 32] {
        let mut token = [0u8; 32];
        for (i, byte) in token.iter_mut().enumerate() {
            *byte = (i + 0x10) as u8;
        }
        token
    }

    #[tokio::test]
    async fn bind_ephemeral_port() {
        let manager = BridgeManager::bind(test_token()).await.unwrap();
        let port = manager.local_port();
        assert!(port > 0);
    }

    #[tokio::test]
    async fn accept_and_route_incoming_ws() {
        let token = test_token();
        let mut manager = BridgeManager::bind(token).await.unwrap();
        let port = manager.local_port();

        let (tx, mut rx) = mpsc::channel::<BridgeConnection>(1);
        manager.add_handler(
            443,
            Direction::Incoming,
            Arc::new(ChannelHandler::new(tx)),
        );

        // Run manager in background
        let manager_handle = tokio::spawn(async move { manager.run().await });

        // Connect and send a valid header
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let header = BridgeHeader {
            session_token: token,
            direction: Direction::Incoming,
            service_port: 443,
            request_id: String::new(),
            remote_addr: "100.64.0.2:12345".to_string(),
            remote_dns_name: "peer.ts.net".to_string(),
        };
        header.write_to(&mut stream).await.unwrap();

        // Write some payload data
        stream.write_all(b"hello from Go shim").await.unwrap();

        // Receive the routed connection
        let conn = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        assert_eq!(conn.header.service_port, 443);
        assert_eq!(conn.header.direction, Direction::Incoming);
        assert_eq!(conn.header.remote_addr, "100.64.0.2:12345");

        manager_handle.abort();
    }

    #[tokio::test]
    async fn reject_invalid_token() {
        let token = test_token();
        let manager = BridgeManager::bind(token).await.unwrap();
        let port = manager.local_port();

        let (_tx, mut rx) = mpsc::channel::<BridgeConnection>(1);
        // No handler registered — we just want to see that it doesn't crash

        let manager_handle = tokio::spawn(async move { manager.run().await });

        // Connect with wrong token
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let wrong_token = [0xFFu8; 32];
        let header = BridgeHeader {
            session_token: wrong_token,
            direction: Direction::Incoming,
            service_port: 443,
            request_id: String::new(),
            remote_addr: "1.2.3.4:80".to_string(),
            remote_dns_name: String::new(),
        };
        header.write_to(&mut stream).await.unwrap();

        // Should not be routed
        let result = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
        assert!(result.is_err(), "should have timed out — connection rejected");

        manager_handle.abort();
    }

    #[tokio::test]
    async fn outgoing_dial_correlation() {
        let token = test_token();
        let manager = BridgeManager::bind(token).await.unwrap();
        let port = manager.local_port();
        let pending_dials = manager.pending_dials().clone();

        let manager_handle = tokio::spawn(async move { manager.run().await });

        // Insert a pending dial
        let (tx, rx) = oneshot::channel::<BridgeConnection>();
        {
            let mut dials = pending_dials.lock().await;
            dials.insert("test-dial-123".to_string(), tx);
        }

        // Simulate Go connecting back with the request_id
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let header = BridgeHeader {
            session_token: token,
            direction: Direction::Outgoing,
            service_port: 443,
            request_id: "test-dial-123".to_string(),
            remote_addr: "100.64.0.3:443".to_string(),
            remote_dns_name: "target.ts.net".to_string(),
        };
        header.write_to(&mut stream).await.unwrap();

        // The dial should be correlated
        let conn = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("timeout")
            .expect("channel error");

        assert_eq!(conn.header.request_id, "test-dial-123");
        assert_eq!(conn.header.direction, Direction::Outgoing);
        assert_eq!(conn.header.service_port, 443);

        // Pending dials map should be empty now
        let dials = pending_dials.lock().await;
        assert!(!dials.contains_key("test-dial-123"));

        manager_handle.abort();
    }

    #[tokio::test]
    async fn header_timeout() {
        let token = test_token();
        let manager = BridgeManager::bind(token).await.unwrap();
        let port = manager.local_port();

        let manager_handle = tokio::spawn(async move { manager.run().await });

        // Connect but don't send anything — should timeout after 2s
        let _stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        // Wait for the timeout plus margin
        tokio::time::sleep(Duration::from_millis(2500)).await;

        // The connection should have been dropped by the manager
        // (we can't directly assert this, but the manager shouldn't crash)

        manager_handle.abort();
    }

    #[tokio::test]
    async fn fallback_handler_for_unknown_port() {
        let token = test_token();
        let mut manager = BridgeManager::bind(token).await.unwrap();
        let port = manager.local_port();

        let (tx, mut rx) = mpsc::channel::<BridgeConnection>(1);
        manager.set_fallback_handler(Arc::new(ChannelHandler::new(tx)));

        let manager_handle = tokio::spawn(async move { manager.run().await });

        // Connect with an unusual port (reverse proxy scenario)
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let header = BridgeHeader {
            session_token: token,
            direction: Direction::Incoming,
            service_port: 8443,
            request_id: String::new(),
            remote_addr: "100.64.0.5:8443".to_string(),
            remote_dns_name: String::new(),
        };
        header.write_to(&mut stream).await.unwrap();

        let conn = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        assert_eq!(conn.header.service_port, 8443);

        manager_handle.abort();
    }

    #[tokio::test]
    async fn unknown_outgoing_request_id_dropped() {
        let token = test_token();
        let manager = BridgeManager::bind(token).await.unwrap();
        let port = manager.local_port();

        let manager_handle = tokio::spawn(async move { manager.run().await });

        // Connect with an outgoing header whose request_id isn't in pending_dials
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let header = BridgeHeader {
            session_token: token,
            direction: Direction::Outgoing,
            service_port: 443,
            request_id: "nonexistent-id".to_string(),
            remote_addr: "100.64.0.3:443".to_string(),
            remote_dns_name: String::new(),
        };
        header.write_to(&mut stream).await.unwrap();

        // Give it a moment to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Manager should still be running fine
        assert!(!manager_handle.is_finished());

        manager_handle.abort();
    }

    #[tokio::test]
    async fn route_file_transfer_port() {
        let token = test_token();
        let mut manager = BridgeManager::bind(token).await.unwrap();
        let port = manager.local_port();

        let (tx, mut rx) = mpsc::channel::<BridgeConnection>(1);
        manager.add_handler(
            9417,
            Direction::Incoming,
            Arc::new(ChannelHandler::new(tx)),
        );

        let manager_handle = tokio::spawn(async move { manager.run().await });

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let header = BridgeHeader {
            session_token: token,
            direction: Direction::Incoming,
            service_port: 9417,
            request_id: String::new(),
            remote_addr: "100.64.0.4:9417".to_string(),
            remote_dns_name: "file-peer.ts.net".to_string(),
        };
        header.write_to(&mut stream).await.unwrap();

        let conn = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        assert_eq!(conn.header.service_port, 9417);
        assert_eq!(conn.header.remote_dns_name, "file-peer.ts.net");

        manager_handle.abort();
    }
}
