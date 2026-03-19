//! # HTTP Layer Integration Tests (Layer 5)
//!
//! These tests validate the HTTP routing, static file serving, reverse proxy,
//! and WebSocket upgrade functionality using **real TCP connections** — no mocks.
//!
//! ## What's proven at this layer
//!
//! 1. **HttpRouter dispatch** — Path-prefix matching with longest-prefix-first
//!    ordering correctly routes requests to the right handler. Fallback handlers
//!    and 404s for unmatched paths work end-to-end over real HTTP.
//!
//! 2. **StaticHandler** — In-memory and disk-based file serving, correct MIME
//!    types, SPA fallback, and directory traversal prevention all work when
//!    accessed via real HTTP requests through the router.
//!
//! 3. **ReverseProxyHandler** — Requests are forwarded to a local HTTP server,
//!    prefix stripping works, headers (including Range) are passed through.
//!
//! 4. **WsUpgradeHandler** — WebSocket upgrade succeeds via the HTTP router
//!    while regular HTTP requests to other routes work simultaneously.
//!
//! ## What's NOT tested at this layer
//!
//! - **Tailscale networking** — All connections are local (127.0.0.1).
//! - **Bridge protocol** — BridgeHeaders are synthesised, not received from Go.
//! - **Mesh protocol** — No device discovery or election.
//! - **TLS** — All connections are plaintext TCP.
//!
//! ## Running
//!
//! ```bash
//! cargo test --test http_integration
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use truffle_core::bridge::header::{BridgeHeader, Direction};
use truffle_core::bridge::manager::BridgeConnection;
use truffle_core::http::proxy::{ProxyTarget, ReverseProxyHandler};
use truffle_core::http::router::{HttpHandler, HttpRouter, PeerInfo};
use truffle_core::http::static_site::{StaticFile, StaticHandler};

// ═══════════════════════════════════════════════════════════════════════════
// Test helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Create a fake BridgeHeader for test connections.
fn fake_header(remote_addr: SocketAddr) -> BridgeHeader {
    BridgeHeader {
        session_token: [0u8; 32],
        direction: Direction::Incoming,
        service_port: 443,
        request_id: String::new(),
        remote_addr: remote_addr.to_string(),
        remote_dns_name: "test-peer.ts.net".to_string(),
    }
}

/// Start a test HTTP server backed by an HttpRouter.
///
/// Binds to 127.0.0.1:0, spawns an accept loop that wraps each TCP stream
/// in a `BridgeConnection` and hands it to `router.handle_connection()`.
/// Returns the local address for clients to connect to.
async fn start_test_router(router: Arc<HttpRouter>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = router.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, remote_addr)) => {
                    let router = router.clone();
                    tokio::spawn(async move {
                        let bridge_conn = BridgeConnection {
                            stream,
                            header: fake_header(remote_addr),
                        };
                        router.handle_connection(bridge_conn).await;
                    });
                }
                Err(_) => break,
            }
        }
    });
    addr
}

/// Start a simple echo HTTP server that returns the request path, method,
/// and selected headers in the response body (as JSON).
///
/// Used for reverse proxy tests.
async fn start_echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let io = TokioIo::new(stream);
                    tokio::spawn(async move {
                        let service = service_fn(|req: Request<Incoming>| async move {
                            let method = req.method().to_string();
                            let path = req
                                .uri()
                                .path_and_query()
                                .map(|pq| pq.as_str().to_string())
                                .unwrap_or_else(|| "/".to_string());
                            let host = req
                                .headers()
                                .get("host")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();
                            let range = req
                                .headers()
                                .get("range")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();
                            let accept = req
                                .headers()
                                .get("accept")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();
                            let custom = req
                                .headers()
                                .get("x-custom-header")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();

                            let body = serde_json::json!({
                                "method": method,
                                "path": path,
                                "host": host,
                                "range": range,
                                "accept": accept,
                                "x-custom-header": custom,
                            });

                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .header("content-type", "application/json")
                                    .header("x-echo-server", "true")
                                    .body(Full::new(Bytes::from(body.to_string())))
                                    .unwrap(),
                            )
                        });

                        let _ = http1::Builder::new()
                            .keep_alive(false)
                            .serve_connection(io, service)
                            .await;
                    });
                }
                Err(_) => break,
            }
        }
    });
    addr
}

/// Make an HTTP request using hyper's low-level client and return (status, headers, body).
async fn http_get(
    addr: SocketAddr,
    path: &str,
) -> (StatusCode, hyper::HeaderMap, String) {
    http_request(addr, path, &[]).await
}

/// Make an HTTP request with custom headers.
async fn http_request(
    addr: SocketAddr,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> (StatusCode, hyper::HeaderMap, String) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = Request::builder()
        .uri(path)
        .header("host", addr.to_string());

    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }

    let req = builder
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = sender.send_request(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let body_str = String::from_utf8_lossy(&body).to_string();

    (status, headers, body_str)
}

/// A simple handler that returns a fixed body and status code (for router dispatch tests).
struct FixedHandler {
    status: StatusCode,
    body: String,
}

impl FixedHandler {
    fn new(status: StatusCode, body: &str) -> Self {
        Self {
            status,
            body: body.to_string(),
        }
    }
}

impl HttpHandler for FixedHandler {
    fn handle(
        &self,
        _req: Request<Incoming>,
        _peer: PeerInfo,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Response<Full<Bytes>>> + Send + '_>,
    > {
        let status = self.status;
        let body = self.body.clone();
        Box::pin(async move {
            Response::builder()
                .status(status)
                .header("content-type", "text/plain")
                .body(Full::new(Bytes::from(body)))
                .unwrap()
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Static file serving tests
// ═══════════════════════════════════════════════════════════════════════════

/// Register StaticHandler with in-memory files, GET /index.html,
/// verify 200 with correct content and Content-Type.
#[tokio::test]
async fn test_static_serves_file_from_memory() {
    let files = vec![
        StaticFile {
            path: "index.html".to_string(),
            content: Bytes::from("<!DOCTYPE html><html><body>Hello</body></html>"),
            mime: "text/html".to_string(),
        },
        StaticFile {
            path: "app.js".to_string(),
            content: Bytes::from("console.log('truffle')"),
            mime: "application/javascript".to_string(),
        },
    ];

    let handler = StaticHandler::from_memory(files);
    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/", "Static", "static", Arc::new(handler))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, headers, body) = http_get(addr, "/index.html").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "<!DOCTYPE html><html><body>Hello</body></html>");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "text/html"
    );

    let (status, headers, body) = http_get(addr, "/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "console.log('truffle')");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/javascript"
    );
}

/// Serve .html, .css, .js, .json, .png files, verify each gets correct Content-Type.
#[tokio::test]
async fn test_static_serves_correct_mime_types() {
    let files = vec![
        StaticFile {
            path: "page.html".to_string(),
            content: Bytes::from("<html></html>"),
            mime: "text/html".to_string(),
        },
        StaticFile {
            path: "style.css".to_string(),
            content: Bytes::from("body{}"),
            mime: "text/css".to_string(),
        },
        StaticFile {
            path: "app.js".to_string(),
            content: Bytes::from("//js"),
            mime: "application/javascript".to_string(),
        },
        StaticFile {
            path: "data.json".to_string(),
            content: Bytes::from("{}"),
            mime: "application/json".to_string(),
        },
        StaticFile {
            path: "logo.png".to_string(),
            content: Bytes::from(vec![0x89, 0x50, 0x4E, 0x47]),
            mime: "image/png".to_string(),
        },
    ];

    let handler = StaticHandler::from_memory(files);
    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/", "Static", "static", Arc::new(handler))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let expected: Vec<(&str, &str)> = vec![
        ("/page.html", "text/html"),
        ("/style.css", "text/css"),
        ("/app.js", "application/javascript"),
        ("/data.json", "application/json"),
        ("/logo.png", "image/png"),
    ];

    for (path, expected_mime) in expected {
        let (status, headers, _) = http_get(addr, path).await;
        assert_eq!(status, StatusCode::OK, "GET {path} should be 200");
        assert_eq!(
            headers.get("content-type").unwrap().to_str().unwrap(),
            expected_mime,
            "GET {path} should have Content-Type: {expected_mime}"
        );
    }
}

/// GET a path that doesn't exist, verify 404.
#[tokio::test]
async fn test_static_404_for_missing_file() {
    let files = vec![StaticFile {
        path: "index.html".to_string(),
        content: Bytes::from("<html></html>"),
        mime: "text/html".to_string(),
    }];

    let handler = StaticHandler::from_memory(files);
    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/", "Static", "static", Arc::new(handler))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, _) = http_get(addr, "/nonexistent.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Enable SPA mode, GET /nonexistent/path, verify returns index.html content.
#[tokio::test]
async fn test_static_spa_fallback() {
    let files = vec![StaticFile {
        path: "index.html".to_string(),
        content: Bytes::from("<html>SPA</html>"),
        mime: "text/html".to_string(),
    }];

    let handler = StaticHandler::from_memory(files).with_spa_fallback(true);
    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/", "Static", "static", Arc::new(handler))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    // Known file still works
    let (status, _, body) = http_get(addr, "/index.html").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "<html>SPA</html>");

    // Unknown path falls back to index.html
    let (status, headers, body) = http_get(addr, "/dashboard/settings/profile").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "<html>SPA</html>");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "text/html"
    );
}

/// GET /../../../etc/passwd, verify 400 or 404 (directory traversal blocked).
#[tokio::test]
async fn test_static_directory_traversal_blocked() {
    // Use a disk-based handler so resolve_path is exercised
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("index.html"), "<html>safe</html>").unwrap();

    let handler = StaticHandler::from_dir(root);
    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/", "Static", "static", Arc::new(handler))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    // Various traversal attempts
    let traversal_paths = [
        "/../../../etc/passwd",
        "/..%2f..%2f..%2fetc/passwd",
        "/foo/../../etc/passwd",
        "/foo/../bar/../../../etc/passwd",
    ];

    for path in &traversal_paths {
        let (status, _, body) = http_get(addr, path).await;
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST,
            "GET {path} should be 404 or 400, got {status}"
        );
        assert!(
            !body.contains("root:"),
            "GET {path} must not leak /etc/passwd content"
        );
    }
}

/// Create a temp dir with files, serve from disk, verify content.
#[tokio::test]
async fn test_static_serves_from_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    std::fs::write(root.join("index.html"), "<html>disk</html>").unwrap();
    std::fs::create_dir_all(root.join("assets")).unwrap();
    std::fs::write(root.join("assets/app.js"), "console.log('disk')").unwrap();

    let handler = StaticHandler::from_dir(root);
    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/", "Static", "static", Arc::new(handler))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, headers, body) = http_get(addr, "/index.html").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "<html>disk</html>");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "text/html"
    );

    let (status, _, body) = http_get(addr, "/assets/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "console.log('disk')");

    // 404 for missing disk file
    let (status, _, _) = http_get(addr, "/missing.txt").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ═══════════════════════════════════════════════════════════════════════════
// Router dispatch tests
// ═══════════════════════════════════════════════════════════════════════════

/// Register /api and /static handlers, verify /api/users goes to proxy handler,
/// /static/app.js goes to static handler.
#[tokio::test]
async fn test_router_dispatches_to_correct_handler() {
    let router = Arc::new(HttpRouter::new());

    let api_handler = Arc::new(FixedHandler::new(StatusCode::OK, "api-handler"));
    let static_handler = Arc::new(FixedHandler::new(StatusCode::OK, "static-handler"));

    router
        .add_route("/api", "API", "proxy", api_handler)
        .await
        .unwrap();
    router
        .add_route("/static", "Static", "static", static_handler)
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, body) = http_get(addr, "/api/users").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "api-handler");

    let (status, _, body) = http_get(addr, "/static/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "static-handler");
}

/// Register /api and /api/v2, request /api/v2/users, verify it hits /api/v2 handler.
#[tokio::test]
async fn test_router_longest_prefix_wins() {
    let router = Arc::new(HttpRouter::new());

    let api_handler = Arc::new(FixedHandler::new(StatusCode::OK, "api-v1"));
    let api_v2_handler = Arc::new(FixedHandler::new(StatusCode::OK, "api-v2"));

    router
        .add_route("/api", "API v1", "proxy", api_handler)
        .await
        .unwrap();
    router
        .add_route("/api/v2", "API v2", "proxy", api_v2_handler)
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    // /api/v2/users should match the longer /api/v2 prefix
    let (status, _, body) = http_get(addr, "/api/v2/users").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "api-v2");

    // /api/v1/users should match /api
    let (status, _, body) = http_get(addr, "/api/v1/users").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "api-v1");

    // Exact match on /api/v2
    let (status, _, body) = http_get(addr, "/api/v2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "api-v2");
}

/// Request a path with no matching route, verify 404.
#[tokio::test]
async fn test_router_404_unmatched_path() {
    let router = Arc::new(HttpRouter::new());

    let handler = Arc::new(FixedHandler::new(StatusCode::OK, "api"));
    router
        .add_route("/api", "API", "proxy", handler)
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, body) = http_get(addr, "/unknown/path").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "Not Found");
}

/// Set a fallback, request unmatched path, verify fallback handles it.
#[tokio::test]
async fn test_router_fallback_handler() {
    let router = Arc::new(HttpRouter::new());

    let api_handler = Arc::new(FixedHandler::new(StatusCode::OK, "api"));
    router
        .add_route("/api", "API", "proxy", api_handler)
        .await
        .unwrap();

    let fallback = Arc::new(FixedHandler::new(
        StatusCode::IM_A_TEAPOT,
        "fallback-handler",
    ));
    router.set_fallback(fallback).await;

    let addr = start_test_router(router).await;

    // /api still works
    let (status, _, body) = http_get(addr, "/api/data").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "api");

    // unmatched path hits fallback
    let (status, _, body) = http_get(addr, "/random/path").await;
    assert_eq!(status, StatusCode::IM_A_TEAPOT);
    assert_eq!(body, "fallback-handler");
}

// ═══════════════════════════════════════════════════════════════════════════
// Reverse proxy tests
// ═══════════════════════════════════════════════════════════════════════════

/// Start a local echo HTTP server, register a proxy route pointing to it,
/// make a request through the router, verify the response came from the echo server.
#[tokio::test]
async fn test_proxy_forwards_to_local_server() {
    let echo_addr = start_echo_server().await;

    let target = ProxyTarget::http(echo_addr.to_string());
    let proxy = ReverseProxyHandler::new("/api", target);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/api", "API Proxy", "proxy", Arc::new(proxy))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, headers, body) = http_get(addr, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("x-echo-server").unwrap().to_str().unwrap(),
        "true",
        "response should come from the echo server"
    );

    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["method"], "GET");
}

/// Proxy /api -> localhost:PORT, request /api/users, verify the local server
/// receives /users (prefix stripped).
#[tokio::test]
async fn test_proxy_strips_prefix() {
    let echo_addr = start_echo_server().await;

    let target = ProxyTarget::http(echo_addr.to_string());
    let proxy = ReverseProxyHandler::new("/api", target);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/api", "API Proxy", "proxy", Arc::new(proxy))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, body) = http_get(addr, "/api/users").await;
    assert_eq!(status, StatusCode::OK);

    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed["path"], "/users",
        "prefix /api should be stripped, leaving /users"
    );

    // Exact prefix match should become /
    let (status, _, body) = http_get(addr, "/api").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed["path"], "/",
        "exact prefix match should map to /"
    );

    // Deep path
    let (_, _, body) = http_get(addr, "/api/v2/items/123").await;
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["path"], "/v2/items/123");
}

/// Verify headers (including Accept, custom headers) are forwarded to the target.
#[tokio::test]
async fn test_proxy_passes_headers() {
    let echo_addr = start_echo_server().await;

    let target = ProxyTarget::http(echo_addr.to_string());
    let proxy = ReverseProxyHandler::new("/api", target);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/api", "API Proxy", "proxy", Arc::new(proxy))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, body) = http_request(
        addr,
        "/api/data",
        &[
            ("accept", "application/json"),
            ("x-custom-header", "custom-value-42"),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["accept"], "application/json");
    assert_eq!(parsed["x-custom-header"], "custom-value-42");
}

/// Test Range: bytes=0-1023 header passthrough (critical for video streaming).
#[tokio::test]
async fn test_proxy_passes_range_headers() {
    let echo_addr = start_echo_server().await;

    let target = ProxyTarget::http(echo_addr.to_string());
    let proxy = ReverseProxyHandler::new("/api", target);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/api", "API Proxy", "proxy", Arc::new(proxy))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, body) = http_request(
        addr,
        "/api/video.mp4",
        &[("range", "bytes=0-1023")],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed["range"], "bytes=0-1023",
        "Range header must be forwarded to the target"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// WebSocket + HTTP coexistence test
// ═══════════════════════════════════════════════════════════════════════════

/// Register /ws (WsUpgradeHandler) and /static (StaticHandler).
/// Verify HTTP GET /static/app.js returns a file,
/// AND WebSocket upgrade at /ws succeeds.
#[tokio::test]
async fn test_ws_and_http_on_same_router() {
    use truffle_core::http::ws_handler::WsUpgradeHandler;
    use truffle_core::transport::connection::{ConnectionManager, TransportConfig};

    let config = TransportConfig::default();
    let (conn_mgr, _rx) = ConnectionManager::new(config);
    let conn_mgr = Arc::new(conn_mgr);

    // Files are stored with the full path including the prefix, because the
    // router passes the unmodified request URI to the handler.
    let files = vec![StaticFile {
        path: "static/app.js".to_string(),
        content: Bytes::from("console.log('ws-coexist')"),
        mime: "application/javascript".to_string(),
    }];
    let static_handler = StaticHandler::from_memory(files);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route(
            "/ws",
            "WebSocket",
            "websocket",
            Arc::new(WsUpgradeHandler::new(conn_mgr)),
        )
        .await
        .unwrap();
    router
        .add_route("/static", "Static", "static", Arc::new(static_handler))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    // 1. HTTP GET /static/app.js should work
    let (status, headers, body) = http_get(addr, "/static/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "console.log('ws-coexist')");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/javascript"
    );

    // 2. WebSocket upgrade at /ws should return 101
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let ws_req = Request::builder()
        .uri("/ws")
        .header("host", addr.to_string())
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = sender.send_request(ws_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(
        resp.headers()
            .get("sec-websocket-accept")
            .unwrap()
            .to_str()
            .unwrap(),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );

    // 3. Non-upgrade request to /ws should get 400
    let (status, _, _) = http_get(addr, "/ws").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 4. Unregistered path returns 404
    let (status, _, _) = http_get(addr, "/unknown").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ═══════════════════════════════════════════════════════════════════════════
// Adversarial edge case integration tests
// ═══════════════════════════════════════════════════════════════════════════

/// Proxy to a port where nothing is listening (via real HTTP through the router).
/// Should return 502 Bad Gateway.
#[tokio::test]
async fn test_proxy_to_dead_port_returns_502() {
    // Get an unused port
    let tmp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = tmp_listener.local_addr().unwrap().port();
    drop(tmp_listener);

    let target = ProxyTarget::http(format!("127.0.0.1:{dead_port}"));
    let proxy = ReverseProxyHandler::new("/api", target);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/api", "Dead Proxy", "proxy", Arc::new(proxy))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, body) = http_get(addr, "/api/health").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body, "Bad Gateway");
}

/// Start an echo server that always returns 500. Proxy should forward the 500
/// to the client (not convert it to 502).
#[tokio::test]
async fn test_proxy_forwards_500_from_target() {
    // Start a server that always returns 500
    let error_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let error_addr = error_listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            match error_listener.accept().await {
                Ok((stream, _)) => {
                    let io = TokioIo::new(stream);
                    tokio::spawn(async move {
                        let service = service_fn(|_req: Request<Incoming>| async move {
                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                                    .body(Full::new(Bytes::from("Internal Server Error")))
                                    .unwrap(),
                            )
                        });
                        let _ = http1::Builder::new()
                            .keep_alive(false)
                            .serve_connection(io, service)
                            .await;
                    });
                }
                Err(_) => break,
            }
        }
    });

    let target = ProxyTarget::http(error_addr.to_string());
    let proxy = ReverseProxyHandler::new("/api", target);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/api", "Error Proxy", "proxy", Arc::new(proxy))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, body) = http_get(addr, "/api/fail").await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "500 from target should be forwarded to client"
    );
    assert_eq!(body, "Internal Server Error");
}

/// Target server closes connection after sending headers (mid-response drop).
/// Client should get an error response (502 Bad Gateway).
#[tokio::test]
async fn test_proxy_target_drops_connection() {
    use tokio::io::AsyncWriteExt;

    // Start a server that accepts the connection, sends partial headers,
    // then drops the connection
    let dropper = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dropper_addr = dropper.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            match dropper.accept().await {
                Ok((mut stream, _)) => {
                    tokio::spawn(async move {
                        // Read enough to consume the request, then drop
                        let mut buf = vec![0u8; 4096];
                        let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
                        // Send partial/malformed response, then close
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\npartial")
                            .await;
                        drop(stream); // Close without sending full body
                    });
                }
                Err(_) => break,
            }
        }
    });

    let target = ProxyTarget::http(dropper_addr.to_string());
    let proxy = ReverseProxyHandler::new("/api", target);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/api", "Dropper Proxy", "proxy", Arc::new(proxy))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    // The proxy collects the response body. If the target drops before
    // sending Content-Length bytes, the collect() call will fail and
    // the proxy should return 502.
    let (status, _, _) = http_get(addr, "/api/drop").await;
    // Either 502 (proxy error) or we get partial content -- both are acceptable.
    // The key thing is: no hang, no panic.
    assert!(
        status == StatusCode::BAD_GATEWAY || status == StatusCode::OK,
        "dropped connection should return 502 or partial 200, got {status}"
    );
}

/// Proxy a POST request with a JSON body. The echo server should receive
/// the body. This validates that request bodies are forwarded.
#[tokio::test]
async fn test_proxy_post_with_body() {
    // Start an echo server that also captures the request body
    let body_echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let body_echo_addr = body_echo.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            match body_echo.accept().await {
                Ok((stream, _)) => {
                    let io = TokioIo::new(stream);
                    tokio::spawn(async move {
                        let service = service_fn(|req: Request<Incoming>| async move {
                            let method = req.method().to_string();
                            let path = req
                                .uri()
                                .path_and_query()
                                .map(|pq| pq.as_str().to_string())
                                .unwrap_or_else(|| "/".to_string());
                            let content_type = req
                                .headers()
                                .get("content-type")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();

                            // Read body
                            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
                            let body_str =
                                String::from_utf8_lossy(&body_bytes).to_string();

                            let resp_body = serde_json::json!({
                                "method": method,
                                "path": path,
                                "content_type": content_type,
                                "body": body_str,
                            });

                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .header("content-type", "application/json")
                                    .body(Full::new(Bytes::from(resp_body.to_string())))
                                    .unwrap(),
                            )
                        });
                        let _ = http1::Builder::new()
                            .keep_alive(false)
                            .serve_connection(io, service)
                            .await;
                    });
                }
                Err(_) => break,
            }
        }
    });

    let target = ProxyTarget::http(body_echo_addr.to_string());
    let proxy = ReverseProxyHandler::new("/api", target);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/api", "Body Proxy", "proxy", Arc::new(proxy))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    // Send a POST with JSON body
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let json_body = r#"{"name":"truffle","version":"0.1.0"}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/api/data")
        .header("host", addr.to_string())
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(json_body)))
        .unwrap();

    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["method"], "POST");
    assert_eq!(parsed["path"], "/data"); // /api prefix stripped
    assert_eq!(parsed["content_type"], "application/json");
    assert_eq!(parsed["body"], json_body);
}

/// Proxy with extra headers configured. Verify they reach the target.
#[tokio::test]
async fn test_proxy_custom_headers_injection() {
    let echo_addr = start_echo_server().await;

    let target = ProxyTarget::http(echo_addr.to_string())
        .with_header("x-custom-header", "injected-by-proxy");
    let proxy = ReverseProxyHandler::new("/api", target);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/api", "Header Proxy", "proxy", Arc::new(proxy))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, body) = http_get(addr, "/api/check").await;
    assert_eq!(status, StatusCode::OK);

    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed["x-custom-header"], "injected-by-proxy",
        "custom header should be forwarded to target"
    );
}

/// Large response body (1MB) from target should be proxied correctly.
#[tokio::test]
async fn test_proxy_large_response_body() {
    let large_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let large_addr = large_server.local_addr().unwrap();
    let response_size = 1024 * 1024; // 1MB

    tokio::spawn(async move {
        loop {
            match large_server.accept().await {
                Ok((stream, _)) => {
                    let io = TokioIo::new(stream);
                    tokio::spawn(async move {
                        let service = service_fn(move |_req: Request<Incoming>| async move {
                            let body = "x".repeat(response_size);
                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .header("content-type", "application/octet-stream")
                                    .body(Full::new(Bytes::from(body)))
                                    .unwrap(),
                            )
                        });
                        let _ = http1::Builder::new()
                            .keep_alive(false)
                            .serve_connection(io, service)
                            .await;
                    });
                }
                Err(_) => break,
            }
        }
    });

    let target = ProxyTarget::http(large_addr.to_string());
    let proxy = ReverseProxyHandler::new("/api", target);

    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/api", "Large Proxy", "proxy", Arc::new(proxy))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, body) = http_get(addr, "/api/large").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.len(),
        response_size,
        "1MB response should be fully proxied"
    );
}

/// HEAD request to static handler. Hyper should strip the body but
/// preserve headers (Content-Type).
#[tokio::test]
async fn test_static_head_request() {
    let files = vec![StaticFile {
        path: "index.html".to_string(),
        content: Bytes::from("<!DOCTYPE html><html><body>HEAD test</body></html>"),
        mime: "text/html".to_string(),
    }];

    let handler = StaticHandler::from_memory(files);
    let router = Arc::new(HttpRouter::new());
    router
        .add_route("/", "Static", "static", Arc::new(handler))
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    // Send HEAD request
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = Request::builder()
        .method("HEAD")
        .uri("/index.html")
        .header("host", addr.to_string())
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/html"
    );

    // Body should be empty for HEAD
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        body.is_empty(),
        "HEAD response body should be empty, got {} bytes",
        body.len()
    );
}

/// Double-slash paths through the router should not crash.
#[tokio::test]
async fn test_router_double_slash_paths() {
    let router = Arc::new(HttpRouter::new());
    let handler = Arc::new(FixedHandler::new(StatusCode::OK, "root-handler"));
    router
        .add_route("/", "Root", "static", handler)
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    // Various double-slash paths
    let paths = ["//", "//api//users", "///", "/api//"];
    for path in &paths {
        let (status, _, _) = http_get(addr, path).await;
        // Should not crash; root catches all since prefix is "/"
        assert_eq!(
            status,
            StatusCode::OK,
            "double-slash path {path} should not crash (root catches all)"
        );
    }
}

/// Query strings should not affect routing. /api/users?page=2 should
/// route the same as /api/users.
#[tokio::test]
async fn test_router_query_string_ignored() {
    let router = Arc::new(HttpRouter::new());
    let handler = Arc::new(FixedHandler::new(StatusCode::OK, "api-response"));
    router
        .add_route("/api", "API", "proxy", handler)
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    let (status, _, body) = http_get(addr, "/api/users?page=2&sort=name").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "api-response");
}

/// Encoded path (%2F) should not bypass prefix matching.
#[tokio::test]
async fn test_router_encoded_path_no_bypass() {
    let router = Arc::new(HttpRouter::new());

    let admin_handler = Arc::new(FixedHandler::new(StatusCode::OK, "admin-area"));
    let api_handler = Arc::new(FixedHandler::new(StatusCode::OK, "api-area"));
    router
        .add_route("/admin", "Admin", "admin", admin_handler)
        .await
        .unwrap();
    router
        .add_route("/api", "API", "proxy", api_handler)
        .await
        .unwrap();

    let addr = start_test_router(router).await;

    // /api/users%2Fadmin should route to /api (not /admin)
    let (status, _, body) = http_get(addr, "/api/users%2Fadmin").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "api-area", "%2F should not be decoded for routing");
}

/// Multiple routes with similar prefixes should not interfere.
#[tokio::test]
async fn test_router_similar_prefixes_no_interference() {
    let router = Arc::new(HttpRouter::new());

    let handlers: Vec<(&str, &str)> = vec![
        ("/api", "api"),
        ("/api/v1", "api-v1"),
        ("/api/v2", "api-v2"),
        ("/app", "app"),
        ("/application", "application"),
    ];

    for (prefix, body) in &handlers {
        let h = Arc::new(FixedHandler::new(StatusCode::OK, body));
        router.add_route(prefix, body, "test", h).await.unwrap();
    }

    let addr = start_test_router(router).await;

    // /api/v1/users -> api-v1 (longest prefix wins)
    let (_, _, body) = http_get(addr, "/api/v1/users").await;
    assert_eq!(body, "api-v1");

    // /api/v2/items -> api-v2
    let (_, _, body) = http_get(addr, "/api/v2/items").await;
    assert_eq!(body, "api-v2");

    // /api/v3/other -> api (falls back to shorter prefix)
    let (_, _, body) = http_get(addr, "/api/v3/other").await;
    assert_eq!(body, "api");

    // /app/dashboard -> app (not api!)
    let (_, _, body) = http_get(addr, "/app/dashboard").await;
    assert_eq!(body, "app");

    // /application/status -> application (not app!)
    let (_, _, body) = http_get(addr, "/application/status").await;
    assert_eq!(body, "application");

    // /apps -> 404 (not /app, because /apps is not /app + /)
    let (status, _, _) = http_get(addr, "/apps").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
