//! WebSocket upgrade handler for the HTTP router (RFC 008 Phase 4).
//!
//! Validates WebSocket upgrade requests arriving on the HTTP router,
//! computes the `Sec-WebSocket-Accept` response, uses `hyper::upgrade::on()`
//! to obtain the raw I/O after sending the 101 response, then wraps the
//! upgraded connection in a `WebSocketStream` and hands it off to the
//! `ConnectionManager`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::WebSocketStream;

use super::router::{HttpHandler, PeerInfo};
use crate::bridge::header::Direction;
use crate::transport::connection::ConnectionManager;

// ═══════════════════════════════════════════════════════════════════════════
// WsUpgradeHandler
// ═══════════════════════════════════════════════════════════════════════════

/// HTTP handler that upgrades HTTP connections to WebSocket.
///
/// Registered at `/ws` on the HTTP router. Validates the WebSocket
/// upgrade headers, sends the 101 response, then passes the upgraded
/// stream to the `ConnectionManager` for mesh communication.
pub struct WsUpgradeHandler {
    connection_manager: Arc<ConnectionManager>,
}

impl WsUpgradeHandler {
    /// Create a new WebSocket upgrade handler.
    pub fn new(connection_manager: Arc<ConnectionManager>) -> Self {
        Self { connection_manager }
    }
}

impl HttpHandler for WsUpgradeHandler {
    fn handle(
        &self,
        req: Request<Incoming>,
        peer: PeerInfo,
    ) -> Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send + '_>> {
        Box::pin(async move {
            // Validate that this is actually a WebSocket upgrade request
            let is_upgrade = req
                .headers()
                .get("upgrade")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.eq_ignore_ascii_case("websocket"))
                .unwrap_or(false);

            if !is_upgrade {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Full::new(Bytes::from("Expected WebSocket upgrade")))
                    .unwrap();
            }

            // Extract Sec-WebSocket-Key
            let ws_key = match req.headers().get("sec-websocket-key") {
                Some(key) => key.to_str().unwrap_or("").to_string(),
                None => {
                    return Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Full::new(Bytes::from("Missing Sec-WebSocket-Key")))
                        .unwrap();
                }
            };

            // Compute the accept key
            let accept_key = derive_accept_key(ws_key.as_bytes());

            // Use hyper's upgrade mechanism to get the raw I/O
            let on_upgrade = hyper::upgrade::on(req);

            let conn_mgr = self.connection_manager.clone();
            let peer_clone = peer.clone();

            // Spawn a task to handle the upgraded connection
            tokio::spawn(async move {
                match on_upgrade.await {
                    Ok(upgraded) => {
                        let io = TokioIo::new(upgraded);
                        // Wrap in WebSocketStream (server role)
                        let ws_stream = WebSocketStream::from_raw_socket(
                            io,
                            Role::Server,
                            None, // No WebSocket config override
                        )
                        .await;

                        // Hand off to the connection manager
                        conn_mgr
                            .handle_ws_stream(
                                ws_stream,
                                peer_clone.remote_addr,
                                peer_clone.remote_dns_name,
                                Direction::Incoming,
                            )
                            .await;
                    }
                    Err(e) => {
                        tracing::warn!("WebSocket upgrade failed: {e}");
                    }
                }
            });

            // Send the 101 Switching Protocols response
            Response::builder()
                .status(StatusCode::SWITCHING_PROTOCOLS)
                .header("upgrade", "websocket")
                .header("connection", "Upgrade")
                .header("sec-websocket-accept", accept_key)
                .body(Full::new(Bytes::new()))
                .unwrap_or_else(|_| {
                    let mut resp = Response::new(Full::new(Bytes::from("Internal Error")));
                    *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                    resp
                })
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::router::HttpRouter;
    use crate::transport::connection::TransportConfig;

    #[test]
    fn derive_accept_key_rfc6455() {
        // RFC 6455 Section 4.2.2 example
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let accept = derive_accept_key(key.as_bytes());
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn ws_upgrade_handler_creation() {
        let config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(config);
        let handler = WsUpgradeHandler::new(Arc::new(conn_mgr));
        // Just verify it constructs without panic
        let _ = handler;
    }

    #[tokio::test]
    async fn ws_handler_registered_on_router() {
        // Verify the WS handler can be registered on the router
        let config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(config);
        let conn_mgr = Arc::new(conn_mgr);

        let router = HttpRouter::new();
        let handler: Arc<dyn HttpHandler> = Arc::new(WsUpgradeHandler::new(conn_mgr));
        router
            .add_route("/ws", "WebSocket", "websocket", handler)
            .await
            .unwrap();

        let routes = router.list_routes().await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].prefix, "/ws");
        assert_eq!(routes[0].handler_type, "websocket");
    }

    /// Integration test: test the WS upgrade handler via a real TCP connection
    /// through the HTTP router.
    #[tokio::test]
    async fn ws_handler_rejects_non_upgrade_via_http() {
        use tokio::net::TcpListener;

        let config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(config);
        let conn_mgr = Arc::new(conn_mgr);

        let router = Arc::new(HttpRouter::new());
        let handler: Arc<dyn HttpHandler> = Arc::new(WsUpgradeHandler::new(conn_mgr));
        router
            .add_route("/ws", "WebSocket", "websocket", handler)
            .await
            .unwrap();

        // Set up a TCP listener to simulate the bridge
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let router_clone = router.clone();
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // Simulate a bridge connection by creating a BridgeConnection
            let conn = crate::bridge::manager::BridgeConnection {
                stream,
                header: crate::bridge::header::BridgeHeader {
                    session_token: [0u8; 32],
                    direction: Direction::Incoming,
                    service_port: 443,
                    request_id: String::new(),
                    remote_addr: "100.64.0.2:12345".to_string(),
                    remote_dns_name: "peer.ts.net".to_string(),
                },
            };
            router_clone.handle_connection(conn).await;
        });

        // Connect as client and send a non-upgrade request
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let io = hyper_util::rt::TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
        tokio::spawn(async move { let _ = conn.await; });

        let req = hyper::Request::builder()
            .uri("/ws")
            .header("host", addr.to_string())
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();

        let resp = sender.send_request(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        server_handle.abort();
    }

    /// Integration test: WS upgrade via the HTTP router returns 101.
    #[tokio::test]
    async fn ws_handler_upgrade_via_http() {
        use tokio::net::TcpListener;

        let config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(config);
        let conn_mgr = Arc::new(conn_mgr);

        let router = Arc::new(HttpRouter::new());
        let handler: Arc<dyn HttpHandler> = Arc::new(WsUpgradeHandler::new(conn_mgr));
        router
            .add_route("/ws", "WebSocket", "websocket", handler)
            .await
            .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let router_clone = router.clone();
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let conn = crate::bridge::manager::BridgeConnection {
                stream,
                header: crate::bridge::header::BridgeHeader {
                    session_token: [0u8; 32],
                    direction: Direction::Incoming,
                    service_port: 443,
                    request_id: String::new(),
                    remote_addr: "100.64.0.2:12345".to_string(),
                    remote_dns_name: "peer.ts.net".to_string(),
                },
            };
            router_clone.handle_connection(conn).await;
        });

        // Connect and send a WebSocket upgrade request
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let io = hyper_util::rt::TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
        tokio::spawn(async move { let _ = conn.await; });

        let req = hyper::Request::builder()
            .uri("/ws")
            .header("host", addr.to_string())
            .header("upgrade", "websocket")
            .header("connection", "Upgrade")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .header("sec-websocket-version", "13")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();

        let resp = sender.send_request(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            resp.headers().get("sec-websocket-accept").unwrap().to_str().unwrap(),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );

        server_handle.abort();
    }

    /// Integration test: non-/ws paths return 404 from the router.
    #[tokio::test]
    async fn router_returns_404_for_unregistered_path() {
        use tokio::net::TcpListener;

        let config = TransportConfig::default();
        let (conn_mgr, _rx) = ConnectionManager::new(config);
        let conn_mgr = Arc::new(conn_mgr);

        let router = Arc::new(HttpRouter::new());
        let handler: Arc<dyn HttpHandler> = Arc::new(WsUpgradeHandler::new(conn_mgr));
        router
            .add_route("/ws", "WebSocket", "websocket", handler)
            .await
            .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let router_clone = router.clone();
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let conn = crate::bridge::manager::BridgeConnection {
                stream,
                header: crate::bridge::header::BridgeHeader {
                    session_token: [0u8; 32],
                    direction: Direction::Incoming,
                    service_port: 443,
                    request_id: String::new(),
                    remote_addr: "100.64.0.2:12345".to_string(),
                    remote_dns_name: "peer.ts.net".to_string(),
                },
            };
            router_clone.handle_connection(conn).await;
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let io = hyper_util::rt::TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
        tokio::spawn(async move { let _ = conn.await; });

        let req = hyper::Request::builder()
            .uri("/unknown")
            .header("host", addr.to_string())
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();

        let resp = sender.send_request(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        server_handle.abort();
    }
}
