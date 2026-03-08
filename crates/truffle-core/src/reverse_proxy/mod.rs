use std::collections::HashMap;
use std::sync::Arc;

use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use http_body_util::{BodyExt, Full};
use bytes::Bytes;
use tokio::io;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for a single reverse proxy.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub id: String,
    pub name: String,
    /// The tsnet port this proxy listens on (e.g., 8443).
    pub port: u16,
    /// The local target port to forward to (e.g., 5173).
    pub target_port: u16,
    /// Target scheme ("http" or "https"). Defaults to "http".
    pub target_scheme: String,
}

/// Information about an active proxy.
#[derive(Debug, Clone)]
pub struct ProxyInfo {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub target_port: u16,
    pub target_scheme: String,
    pub is_active: bool,
}

/// Events emitted by the ProxyManager.
#[derive(Debug, Clone)]
pub enum ProxyEvent {
    Started {
        id: String,
        port: u16,
        target_port: u16,
        url: String,
    },
    Stopped {
        id: String,
        reason: String,
    },
    Error {
        id: String,
        message: String,
        code: String,
    },
}

/// Error codes for proxy operations.
pub mod error_codes {
    pub const PROXY_EXISTS: &str = "PROXY_EXISTS";
    pub const PORT_IN_USE: &str = "PORT_IN_USE";
    pub const CONNECTION_REFUSED: &str = "CONNECTION_REFUSED";
    pub const SERVE_ERROR: &str = "SERVE_ERROR";
}

// ═══════════════════════════════════════════════════════════════════════════
// ProxyManager
// ═══════════════════════════════════════════════════════════════════════════

/// Manages multiple reverse proxy instances.
///
/// Ported from Go `ProxyManager` (reverseproxy.go, 382 LOC).
/// Each proxy forwards HTTP requests and WebSocket upgrades from
/// a tsnet port to a local target port (e.g., Vite dev server).
///
/// In the Rust rewrite, proxies run over bridged TCP connections
/// from the BridgeManager. The proxy handler is called per-connection
/// from the bridge accept loop's catch-all route for unknown ports.
pub struct ProxyManager {
    proxies: Arc<RwLock<HashMap<String, ActiveProxy>>>,
    event_tx: mpsc::Sender<ProxyEvent>,
}

struct ActiveProxy {
    config: ProxyConfig,
    cancel: CancellationToken,
}

impl ProxyManager {
    pub fn new(event_tx: mpsc::Sender<ProxyEvent>) -> Self {
        Self {
            proxies: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
        }
    }

    /// Add a new proxy configuration.
    pub async fn add(&self, config: ProxyConfig) -> Result<(), String> {
        let mut proxies = self.proxies.write().await;

        // Check duplicate ID
        if proxies.contains_key(&config.id) {
            let msg = format!("proxy with id {} already exists", config.id);
            self.emit(ProxyEvent::Error {
                id: config.id.clone(),
                message: msg.clone(),
                code: error_codes::PROXY_EXISTS.to_string(),
            }).await;
            return Err(msg);
        }

        // Check port conflict
        for (_, proxy) in proxies.iter() {
            if proxy.config.port == config.port {
                let msg = format!("port {} is already in use by proxy {}", config.port, proxy.config.id);
                self.emit(ProxyEvent::Error {
                    id: config.id.clone(),
                    message: msg.clone(),
                    code: error_codes::PORT_IN_USE.to_string(),
                }).await;
                return Err(msg);
            }
        }

        let cancel = CancellationToken::new();
        let id = config.id.clone();
        let port = config.port;
        let target_port = config.target_port;

        self.emit(ProxyEvent::Started {
            id: id.clone(),
            port,
            target_port,
            url: format!("https://[dns-name]:{port}"),
        }).await;

        proxies.insert(id, ActiveProxy { config, cancel });
        Ok(())
    }

    /// Remove and stop a proxy by ID.
    pub async fn remove(&self, id: &str) -> Result<(), String> {
        let mut proxies = self.proxies.write().await;
        let proxy = proxies.remove(id)
            .ok_or_else(|| format!("proxy {id} not found"))?;

        proxy.cancel.cancel();

        self.emit(ProxyEvent::Stopped {
            id: id.to_string(),
            reason: "removed".to_string(),
        }).await;

        Ok(())
    }

    /// List all active proxies.
    pub async fn list(&self) -> Vec<ProxyInfo> {
        let proxies = self.proxies.read().await;
        proxies.values().map(|p| ProxyInfo {
            id: p.config.id.clone(),
            name: p.config.name.clone(),
            port: p.config.port,
            target_port: p.config.target_port,
            target_scheme: p.config.target_scheme.clone(),
            is_active: true,
        }).collect()
    }

    /// Get the proxy config for a given tsnet port, if any.
    pub async fn config_for_port(&self, port: u16) -> Option<ProxyConfig> {
        let proxies = self.proxies.read().await;
        proxies.values()
            .find(|p| p.config.port == port)
            .map(|p| p.config.clone())
    }

    /// Close all proxies.
    pub async fn close_all(&self) {
        let mut proxies = self.proxies.write().await;
        let ids: Vec<String> = proxies.keys().cloned().collect();
        for id in &ids {
            if let Some(proxy) = proxies.remove(id) {
                proxy.cancel.cancel();
                let _ = self.event_tx.try_send(ProxyEvent::Stopped {
                    id: id.clone(),
                    reason: "shutdown".to_string(),
                });
            }
        }
    }

    /// Handle a bridged connection for a proxied port.
    ///
    /// Called from the BridgeManager accept loop when the port matches
    /// a configured proxy. Proxies the HTTP request (or WebSocket upgrade)
    /// to the local target.
    pub async fn handle_connection(
        &self,
        stream: TcpStream,
        port: u16,
    ) {
        let config = match self.config_for_port(port).await {
            Some(c) => c,
            None => {
                tracing::warn!("No proxy configured for port {port}");
                return;
            }
        };

        let target_port = config.target_port;
        let proxy_id = config.id.clone();
        let event_tx = self.event_tx.clone();

        let io = TokioIo::new(stream);

        let service = service_fn(move |req: Request<Incoming>| {
            let proxy_id = proxy_id.clone();
            let event_tx = event_tx.clone();
            async move {
                handle_proxy_request(req, target_port, &proxy_id, &event_tx).await
            }
        });

        if let Err(e) = http1::Builder::new()
            .keep_alive(true)
            .serve_connection(io, service)
            .with_upgrades()
            .await
        {
            tracing::debug!("Proxy connection ended: {e}");
        }
    }

    async fn emit(&self, event: ProxyEvent) {
        let _ = self.event_tx.try_send(event);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Request handling
// ═══════════════════════════════════════════════════════════════════════════

/// Handle a single proxied HTTP request.
///
/// For regular HTTP: forward to localhost:target_port, return response.
/// For WebSocket upgrade: connect to target, relay upgrade response,
/// then bidirectional copy via `tokio::io::copy_bidirectional`.
async fn handle_proxy_request(
    req: Request<Incoming>,
    target_port: u16,
    proxy_id: &str,
    event_tx: &mpsc::Sender<ProxyEvent>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    if is_websocket_upgrade(&req) {
        tracing::debug!("[Proxy {proxy_id}] WebSocket upgrade detected for {}", req.uri().path());
        match handle_ws_upgrade(req, target_port).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                tracing::warn!("[Proxy {proxy_id}] WebSocket proxy error: {e}");
                Ok(bad_gateway())
            }
        }
    } else {
        tracing::debug!(
            "[Proxy {proxy_id}] HTTP {} {}",
            req.method(),
            req.uri().path()
        );
        match forward_http(req, target_port).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!("[Proxy {proxy_id}] HTTP proxy error: {msg}");
                if msg.contains("Connection refused") || msg.contains("connection refused") {
                    let _ = event_tx.try_send(ProxyEvent::Error {
                        id: proxy_id.to_string(),
                        message: format!("target localhost:{target_port} not reachable"),
                        code: error_codes::CONNECTION_REFUSED.to_string(),
                    });
                }
                Ok(bad_gateway())
            }
        }
    }
}

/// Check if a request is a WebSocket upgrade.
fn is_websocket_upgrade<B>(req: &Request<B>) -> bool {
    let connection = req.headers()
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let upgrade = req.headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    connection.contains("upgrade") && upgrade == "websocket"
}

/// Forward a regular HTTP request to the local target.
async fn forward_http(
    mut req: Request<Incoming>,
    target_port: u16,
) -> Result<Response<Full<Bytes>>, Box<dyn std::error::Error + Send + Sync>> {
    // Connect to target
    let target_addr = format!("127.0.0.1:{target_port}");
    let target_stream = TcpStream::connect(&target_addr).await?;
    let target_io = TokioIo::new(target_stream);

    // Rewrite Host header to localhost so dev servers (Vite, etc.) accept it
    let host_val = format!("localhost:{target_port}");
    if let Ok(hv) = host_val.parse() {
        req.headers_mut().insert("host", hv);
    }

    // Build the URI with the target
    let path = req.uri().path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let uri: hyper::Uri = format!("http://{target_addr}{path}").parse()?;
    *req.uri_mut() = uri;

    // Send via hyper client
    let (mut sender, conn) = hyper::client::conn::http1::handshake(target_io).await?;

    // Spawn connection driver
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!("Forward connection ended: {e}");
        }
    });

    let resp = sender.send_request(req).await?;

    // Collect response body
    let (parts, body) = resp.into_parts();
    let body_bytes = body.collect().await?.to_bytes();
    Ok(Response::from_parts(parts, Full::new(body_bytes)))
}

/// Handle WebSocket upgrade by connecting to target, relaying the upgrade,
/// then doing bidirectional TCP copy.
///
/// This mirrors the Go `handleWebSocketProxy` which hijacks the HTTP
/// connection and does `io.Copy` in both directions.
async fn handle_ws_upgrade(
    req: Request<Incoming>,
    target_port: u16,
) -> Result<Response<Full<Bytes>>, Box<dyn std::error::Error + Send + Sync>> {
    // Connect to target
    let target_addr = format!("127.0.0.1:{target_port}");
    let mut target_stream = TcpStream::connect(&target_addr).await?;

    // Write the HTTP upgrade request to the target
    let host_val = format!("localhost:{target_port}");
    let path = req.uri().path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    // Build raw HTTP request for the target
    let mut raw_request = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\n",
        req.method(),
        path,
        host_val,
    );
    for (name, value) in req.headers() {
        if name.as_str().eq_ignore_ascii_case("host") {
            continue; // Already set
        }
        if let Ok(v) = value.to_str() {
            raw_request.push_str(&format!("{}: {}\r\n", name, v));
        }
    }
    raw_request.push_str("\r\n");

    use tokio::io::AsyncWriteExt;
    target_stream.write_all(raw_request.as_bytes()).await?;

    // Use hyper's upgrade mechanism to get the client's underlying TCP stream.
    // We respond with 101 Switching Protocols to initiate the upgrade on the
    // client side, then do bidirectional copy between client and target.

    // Read the target's response (101 Switching Protocols expected)
    use tokio::io::AsyncBufReadExt;
    let mut target_reader = tokio::io::BufReader::new(&mut target_stream);
    let mut response_headers = String::new();
    loop {
        let mut line = String::new();
        let n = target_reader.read_line(&mut line).await?;
        if n == 0 {
            return Err("target closed connection before completing upgrade".into());
        }
        response_headers.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    drop(target_reader);

    // Parse the status line
    let status_line = response_headers.lines().next().unwrap_or("");
    if !status_line.contains("101") {
        return Err(format!("target did not upgrade: {status_line}").into());
    }

    // Now we have a successfully upgraded WebSocket on the target side.
    // We need to upgrade the client connection too.
    // Use hyper's on_upgrade to get the client stream, then copy bidirectionally.
    let on_upgrade = hyper::upgrade::on(req);

    // Build the 101 response to send back to the client
    let mut resp_builder = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);

    // Forward relevant headers from target's response
    for line in response_headers.lines().skip(1) {
        if line.is_empty() || line == "\r" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if let (Ok(n), Ok(v)) = (name.parse::<hyper::header::HeaderName>(), value.parse::<hyper::header::HeaderValue>()) {
                resp_builder = resp_builder.header(n, v);
            }
        }
    }

    // Spawn the bidirectional copy task
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let mut client_stream = TokioIo::new(upgraded);
                match io::copy_bidirectional(&mut client_stream, &mut target_stream).await {
                    Ok((c2t, t2c)) => {
                        tracing::debug!("WebSocket proxy ended: client->target={c2t}B, target->client={t2c}B");
                    }
                    Err(e) => {
                        tracing::debug!("WebSocket proxy copy error: {e}");
                    }
                }
            }
            Err(e) => {
                tracing::warn!("WebSocket upgrade failed: {e}");
            }
        }
    });

    let resp = resp_builder.body(Full::new(Bytes::new()))?;
    Ok(resp)
}

fn bad_gateway() -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .body(Full::new(Bytes::from("Bad Gateway")))
        .unwrap_or_else(|_| {
            let mut resp = Response::new(Full::new(Bytes::from("Bad Gateway")));
            *resp.status_mut() = StatusCode::BAD_GATEWAY;
            resp
        })
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn add_and_list_proxies() {
        let (tx, _rx) = mpsc::channel(16);
        let manager = ProxyManager::new(tx);

        let config = ProxyConfig {
            id: "proxy-1".to_string(),
            name: "Dev Server".to_string(),
            port: 8443,
            target_port: 5173,
            target_scheme: "http".to_string(),
        };
        manager.add(config).await.unwrap();

        let list = manager.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "proxy-1");
        assert_eq!(list[0].port, 8443);
        assert_eq!(list[0].target_port, 5173);
        assert!(list[0].is_active);
    }

    #[tokio::test]
    async fn reject_duplicate_id() {
        let (tx, _rx) = mpsc::channel(16);
        let manager = ProxyManager::new(tx);

        let config = ProxyConfig {
            id: "proxy-1".to_string(),
            name: "A".to_string(),
            port: 8443,
            target_port: 5173,
            target_scheme: "http".to_string(),
        };
        manager.add(config.clone()).await.unwrap();

        let result = manager.add(ProxyConfig {
            id: "proxy-1".to_string(),
            name: "B".to_string(),
            port: 9443,
            target_port: 3000,
            target_scheme: "http".to_string(),
        }).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[tokio::test]
    async fn reject_duplicate_port() {
        let (tx, _rx) = mpsc::channel(16);
        let manager = ProxyManager::new(tx);

        manager.add(ProxyConfig {
            id: "proxy-1".to_string(),
            name: "A".to_string(),
            port: 8443,
            target_port: 5173,
            target_scheme: "http".to_string(),
        }).await.unwrap();

        let result = manager.add(ProxyConfig {
            id: "proxy-2".to_string(),
            name: "B".to_string(),
            port: 8443,
            target_port: 3000,
            target_scheme: "http".to_string(),
        }).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already in use"));
    }

    #[tokio::test]
    async fn remove_proxy() {
        let (tx, mut rx) = mpsc::channel(16);
        let manager = ProxyManager::new(tx);

        manager.add(ProxyConfig {
            id: "proxy-1".to_string(),
            name: "A".to_string(),
            port: 8443,
            target_port: 5173,
            target_scheme: "http".to_string(),
        }).await.unwrap();

        // Drain the Started event
        let _ = rx.recv().await;

        manager.remove("proxy-1").await.unwrap();
        assert!(manager.list().await.is_empty());

        // Should have received Stopped event
        let event = rx.recv().await.unwrap();
        match event {
            ProxyEvent::Stopped { id, reason } => {
                assert_eq!(id, "proxy-1");
                assert_eq!(reason, "removed");
            }
            _ => panic!("expected Stopped event"),
        }
    }

    #[tokio::test]
    async fn remove_nonexistent() {
        let (tx, _rx) = mpsc::channel(16);
        let manager = ProxyManager::new(tx);
        let result = manager.remove("nope").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn close_all() {
        let (tx, _rx) = mpsc::channel(16);
        let manager = ProxyManager::new(tx);

        for i in 0..3 {
            manager.add(ProxyConfig {
                id: format!("proxy-{i}"),
                name: format!("P{i}"),
                port: 8443 + i as u16,
                target_port: 5173 + i as u16,
                target_scheme: "http".to_string(),
            }).await.unwrap();
        }

        assert_eq!(manager.list().await.len(), 3);
        manager.close_all().await;
        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn config_for_port() {
        let (tx, _rx) = mpsc::channel(16);
        let manager = ProxyManager::new(tx);

        manager.add(ProxyConfig {
            id: "proxy-1".to_string(),
            name: "A".to_string(),
            port: 8443,
            target_port: 5173,
            target_scheme: "http".to_string(),
        }).await.unwrap();

        let config = manager.config_for_port(8443).await;
        assert!(config.is_some());
        assert_eq!(config.unwrap().target_port, 5173);

        assert!(manager.config_for_port(9999).await.is_none());
    }

    #[test]
    fn is_websocket_upgrade_detection() {
        // Build a request with WebSocket upgrade headers
        let req = Request::builder()
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .body(())
            .unwrap();
        assert!(is_websocket_upgrade(&req));
    }

    #[test]
    fn is_websocket_upgrade_case_insensitive() {
        let req = Request::builder()
            .header("connection", "upgrade")
            .header("upgrade", "WebSocket")
            .body(())
            .unwrap();
        assert!(is_websocket_upgrade(&req));
    }

    #[test]
    fn not_websocket_without_headers() {
        let req = Request::builder()
            .body(())
            .unwrap();
        assert!(!is_websocket_upgrade(&req));
    }

    #[tokio::test]
    async fn events_emitted_on_add() {
        let (tx, mut rx) = mpsc::channel(16);
        let manager = ProxyManager::new(tx);

        manager.add(ProxyConfig {
            id: "proxy-1".to_string(),
            name: "Dev".to_string(),
            port: 8443,
            target_port: 5173,
            target_scheme: "http".to_string(),
        }).await.unwrap();

        let event = rx.recv().await.unwrap();
        match event {
            ProxyEvent::Started { id, port, target_port, .. } => {
                assert_eq!(id, "proxy-1");
                assert_eq!(port, 8443);
                assert_eq!(target_port, 5173);
            }
            _ => panic!("expected Started event"),
        }
    }
}
