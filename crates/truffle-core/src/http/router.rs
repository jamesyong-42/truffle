//! HTTP Router -- path-prefix-based request routing (RFC 008 Phase 4).
//!
//! Routes incoming HTTP connections to the appropriate handler based on
//! longest-prefix-first matching. Sits between the bridge and specialized
//! handlers (WebSocket, reverse proxy, static files, PWA).
//!
//! ## Architecture
//!
//! `HttpRouter` holds a sorted list of routes (longest-prefix-first).
//! Each route maps a path prefix to an `Arc<dyn HttpHandler>`. When a
//! bridge connection arrives, `handle_connection` uses hyper's `http1::Builder`
//! to parse the HTTP request, then dispatches to the matching handler.
//! A fallback handler catches unmatched paths (defaults to 404).

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::sync::RwLock;

use crate::bridge::manager::BridgeConnection;

// ═══════════════════════════════════════════════════════════════════════════
// PeerInfo
// ═══════════════════════════════════════════════════════════════════════════

/// Information about the remote peer, extracted from the bridge header.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub remote_addr: String,
    pub remote_dns_name: String,
}

impl From<&crate::bridge::header::BridgeHeader> for PeerInfo {
    fn from(header: &crate::bridge::header::BridgeHeader) -> Self {
        Self {
            remote_addr: header.remote_addr.clone(),
            remote_dns_name: header.remote_dns_name.clone(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// HttpHandler trait
// ═══════════════════════════════════════════════════════════════════════════

/// Trait for HTTP request handlers registered with the router.
///
/// Implementors handle a single HTTP request and return a response.
/// The `PeerInfo` carries the identity of the remote Tailscale peer.
pub trait HttpHandler: Send + Sync + 'static {
    fn handle(
        &self,
        req: Request<Incoming>,
        peer: PeerInfo,
    ) -> Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send + '_>>;
}

// ═══════════════════════════════════════════════════════════════════════════
// Route types
// ═══════════════════════════════════════════════════════════════════════════

/// A registered route entry (internal, holds the handler).
struct RouteEntry {
    /// Normalized path prefix (e.g., "/api/v2").
    prefix: String,
    /// Human-readable name for the route.
    name: String,
    /// The handler type label (for display/debugging).
    handler_type: String,
    /// The actual handler.
    handler: Arc<dyn HttpHandler>,
}

/// Public snapshot of a registered route (no handler reference).
#[derive(Debug, Clone)]
pub struct Route {
    /// Normalized path prefix (e.g., "/api/v2").
    pub prefix: String,
    /// Human-readable name for the route.
    pub name: String,
    /// The handler type (for display/debugging).
    pub handler_type: String,
}

/// Errors returned by router operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterError {
    /// A route with the same prefix already exists.
    DuplicatePrefix(String),
    /// The specified route was not found.
    NotFound(String),
    /// The prefix is invalid.
    InvalidPrefix(String),
}

impl fmt::Display for RouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouterError::DuplicatePrefix(prefix) => {
                write!(f, "route prefix \"{}\" is already registered", prefix)
            }
            RouterError::NotFound(prefix) => {
                write!(f, "no route found for prefix \"{}\"", prefix)
            }
            RouterError::InvalidPrefix(reason) => {
                write!(f, "invalid prefix: {}", reason)
            }
        }
    }
}

impl std::error::Error for RouterError {}

// ═══════════════════════════════════════════════════════════════════════════
// HttpRouter
// ═══════════════════════════════════════════════════════════════════════════

/// Routes incoming HTTP connections to the appropriate handler.
///
/// Matches requests using longest-prefix-first ordering. When multiple
/// prefixes could match a request, the longest (most specific) prefix wins.
/// For example, `/api/v2` takes priority over `/api` for `/api/v2/users`.
///
/// Duplicate prefix registration returns a `DuplicatePrefix` error.
pub struct HttpRouter {
    /// Registered routes, sorted by prefix length (longest first) for matching.
    routes: Arc<RwLock<Vec<RouteEntry>>>,
    /// Fallback handler for unmatched paths. Defaults to 404 if None.
    fallback: RwLock<Option<Arc<dyn HttpHandler>>>,
}

impl HttpRouter {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self {
            routes: Arc::new(RwLock::new(Vec::new())),
            fallback: RwLock::new(None),
        }
    }

    /// Normalize a path prefix: ensure leading `/`, strip trailing `/`.
    pub fn normalize_prefix(prefix: &str) -> String {
        normalize_prefix(prefix)
    }

    /// Add a route with a handler. Returns `DuplicatePrefix` if the prefix is already registered.
    pub async fn add_route(
        &self,
        prefix: &str,
        name: &str,
        handler_type: &str,
        handler: Arc<dyn HttpHandler>,
    ) -> Result<(), RouterError> {
        let normalized = normalize_prefix(prefix);
        let mut routes = self.routes.write().await;

        // Check for duplicate prefix
        if routes.iter().any(|r| r.prefix == normalized) {
            return Err(RouterError::DuplicatePrefix(normalized));
        }

        routes.push(RouteEntry {
            prefix: normalized,
            name: name.to_string(),
            handler_type: handler_type.to_string(),
            handler,
        });

        // Sort by prefix length descending (longest first) for matching
        routes.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));

        Ok(())
    }

    /// Remove a route by prefix. Returns `NotFound` if the prefix isn't registered.
    pub async fn remove_route(&self, prefix: &str) -> Result<(), RouterError> {
        let normalized = normalize_prefix(prefix);
        let mut routes = self.routes.write().await;

        let len_before = routes.len();
        routes.retain(|r| r.prefix != normalized);

        if routes.len() == len_before {
            return Err(RouterError::NotFound(normalized));
        }

        Ok(())
    }

    /// List all registered routes (public snapshots without handler references).
    pub async fn list_routes(&self) -> Vec<Route> {
        let routes = self.routes.read().await;
        routes
            .iter()
            .map(|r| Route {
                prefix: r.prefix.clone(),
                name: r.name.clone(),
                handler_type: r.handler_type.clone(),
            })
            .collect()
    }

    /// Match a request path to the best route (longest prefix wins).
    ///
    /// Returns the matched `Route` snapshot if found.
    pub async fn match_route(&self, path: &str) -> Option<Route> {
        let routes = self.routes.read().await;

        // Routes are sorted longest-first, so first match is the best match
        for route in routes.iter() {
            if path_matches(path, &route.prefix) {
                return Some(Route {
                    prefix: route.prefix.clone(),
                    name: route.name.clone(),
                    handler_type: route.handler_type.clone(),
                });
            }
        }

        None
    }

    /// Set a fallback handler for unmatched paths.
    pub async fn set_fallback(&self, handler: Arc<dyn HttpHandler>) {
        let mut fallback = self.fallback.write().await;
        *fallback = Some(handler);
    }

    /// Handle a bridge connection: parse HTTP via hyper, dispatch by path.
    ///
    /// This is the main entry point for incoming connections on port 443.
    /// Uses hyper's `http1::Builder` to parse the HTTP request, then
    /// dispatches to the matching handler based on path prefix.
    pub async fn handle_connection(&self, conn: BridgeConnection) {
        let peer = PeerInfo::from(&conn.header);
        let routes = self.routes.clone();
        let fallback = &self.fallback;

        // Read the fallback once
        let fallback_handler = {
            let fb = fallback.read().await;
            fb.clone()
        };

        let io = TokioIo::new(conn.stream);

        // Clone the routes Arc for use in the service closure
        let routes_ref = routes.clone();

        let service = service_fn(move |req: Request<Incoming>| {
            let peer = peer.clone();
            let routes_ref = routes_ref.clone();
            let fallback_handler = fallback_handler.clone();
            async move {
                let resp = dispatch(&routes_ref, fallback_handler.as_deref(), req, peer).await;
                Ok::<_, hyper::Error>(resp)
            }
        });

        if let Err(e) = http1::Builder::new()
            .keep_alive(true)
            .serve_connection(io, service)
            .with_upgrades()
            .await
        {
            tracing::debug!("HTTP router connection ended: {e}");
        }
    }
}

impl Default for HttpRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// HttpRouterBridgeHandler
// ═══════════════════════════════════════════════════════════════════════════

/// `BridgeHandler` implementation that routes connections through the `HttpRouter`.
///
/// Register this as the handler for port 443 (incoming) on the BridgeManager.
pub struct HttpRouterBridgeHandler {
    router: Arc<HttpRouter>,
}

impl HttpRouterBridgeHandler {
    pub fn new(router: Arc<HttpRouter>) -> Self {
        Self { router }
    }
}

impl crate::bridge::manager::BridgeHandler for HttpRouterBridgeHandler {
    fn handle(
        &self,
        conn: BridgeConnection,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.router.handle_connection(conn).await;
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Normalize a path prefix: ensure leading `/`, strip trailing `/`.
pub fn normalize_prefix(prefix: &str) -> String {
    let mut p = prefix.to_string();

    // Ensure leading slash
    if !p.starts_with('/') {
        p = format!("/{p}");
    }

    // Strip trailing slash (unless it's just "/")
    if p.len() > 1 && p.ends_with('/') {
        p.pop();
    }

    p
}

/// Check if a request path matches a route prefix.
///
/// A path matches if it equals the prefix, or starts with prefix followed
/// by '/'. The root prefix "/" matches everything.
fn path_matches(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// Dispatch a request to the matching handler (internal).
async fn dispatch(
    routes: &RwLock<Vec<RouteEntry>>,
    fallback: Option<&dyn HttpHandler>,
    req: Request<Incoming>,
    peer: PeerInfo,
) -> Response<Full<Bytes>> {
    let path = req.uri().path().to_string();

    // Find the longest matching prefix
    let handler = {
        let routes = routes.read().await;
        routes
            .iter()
            .find(|r| path_matches(&path, &r.prefix))
            .map(|r| r.handler.clone())
    };

    if let Some(handler) = handler {
        return handler.handle(req, peer).await;
    }

    // Try fallback
    if let Some(fb) = fallback {
        return fb.handle(req, peer).await;
    }

    // No route matched -- return 404
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Full::new(Bytes::from("Not Found")))
        .unwrap_or_else(|_| {
            let mut resp = Response::new(Full::new(Bytes::from("Not Found")));
            *resp.status_mut() = StatusCode::NOT_FOUND;
            resp
        })
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// A simple test handler that returns a fixed status code.
    struct StatusHandler(StatusCode);

    impl HttpHandler for StatusHandler {
        fn handle(
            &self,
            _req: Request<Incoming>,
            _peer: PeerInfo,
        ) -> Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send + '_>> {
            let status = self.0;
            Box::pin(async move {
                Response::builder()
                    .status(status)
                    .body(Full::new(Bytes::new()))
                    .unwrap()
            })
        }
    }

    fn test_handler() -> Arc<dyn HttpHandler> {
        Arc::new(StatusHandler(StatusCode::OK))
    }

    #[tokio::test]
    async fn test_add_and_list_routes() {
        let router = HttpRouter::new();

        router.add_route("/api", "API Proxy", "proxy", test_handler()).await.unwrap();
        router.add_route("/static", "Static Files", "static", test_handler()).await.unwrap();
        router.add_route("/ws", "WebSocket", "websocket", test_handler()).await.unwrap();

        let routes = router.list_routes().await;
        assert_eq!(routes.len(), 3);

        // Verify all routes are present (order is by prefix length, but these are same length)
        let prefixes: Vec<&str> = routes.iter().map(|r| r.prefix.as_str()).collect();
        assert!(prefixes.contains(&"/api"));
        assert!(prefixes.contains(&"/static"));
        assert!(prefixes.contains(&"/ws"));
    }

    #[tokio::test]
    async fn test_duplicate_prefix_rejected() {
        let router = HttpRouter::new();

        router.add_route("/api", "API Proxy", "proxy", test_handler()).await.unwrap();

        let result = router.add_route("/api", "Another API", "proxy", test_handler()).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            RouterError::DuplicatePrefix(prefix) => {
                assert_eq!(prefix, "/api");
            }
            other => panic!("expected DuplicatePrefix, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_longest_prefix_wins() {
        let router = HttpRouter::new();

        router.add_route("/api", "General API", "proxy", test_handler()).await.unwrap();
        router
            .add_route("/api/v2", "API v2", "proxy", Arc::new(StatusHandler(StatusCode::ACCEPTED)))
            .await
            .unwrap();

        // /api/v2/users should match /api/v2 (longer prefix wins)
        let matched = router.match_route("/api/v2/users").await;
        assert!(matched.is_some());
        let route = matched.unwrap();
        assert_eq!(route.prefix, "/api/v2");
        assert_eq!(route.name, "API v2");

        // /api/v1/users should match /api (shorter prefix)
        let matched = router.match_route("/api/v1/users").await;
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().prefix, "/api");

        // Exact match on /api/v2
        let matched = router.match_route("/api/v2").await;
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().prefix, "/api/v2");
    }

    #[tokio::test]
    async fn test_normalize_prefix() {
        // Leading slash added
        assert_eq!(normalize_prefix("api"), "/api");

        // Trailing slash stripped
        assert_eq!(normalize_prefix("/api/"), "/api");

        // Both: leading added, trailing stripped
        assert_eq!(normalize_prefix("api/"), "/api");

        // Already normalized
        assert_eq!(normalize_prefix("/api"), "/api");

        // Root "/" stays as is
        assert_eq!(normalize_prefix("/"), "/");

        // Nested paths
        assert_eq!(normalize_prefix("api/v2/"), "/api/v2");

        // Deeply nested
        assert_eq!(normalize_prefix("/a/b/c/d/"), "/a/b/c/d");
    }

    #[tokio::test]
    async fn test_remove_route() {
        let router = HttpRouter::new();

        router.add_route("/api", "API", "proxy", test_handler()).await.unwrap();
        router.add_route("/static", "Static", "static", test_handler()).await.unwrap();

        assert_eq!(router.list_routes().await.len(), 2);

        // Remove succeeds
        router.remove_route("/api").await.unwrap();
        let routes = router.list_routes().await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].prefix, "/static");
    }

    #[tokio::test]
    async fn test_remove_route_not_found() {
        let router = HttpRouter::new();

        router.add_route("/api", "API", "proxy", test_handler()).await.unwrap();

        let result = router.remove_route("/unknown").await;
        assert!(result.is_err());

        match result.unwrap_err() {
            RouterError::NotFound(prefix) => {
                assert_eq!(prefix, "/unknown");
            }
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_router_error_display() {
        let dup = RouterError::DuplicatePrefix("/api".to_string());
        assert_eq!(
            dup.to_string(),
            "route prefix \"/api\" is already registered"
        );

        let not_found = RouterError::NotFound("/unknown".to_string());
        assert_eq!(
            not_found.to_string(),
            "no route found for prefix \"/unknown\""
        );

        let invalid = RouterError::InvalidPrefix("empty".to_string());
        assert_eq!(invalid.to_string(), "invalid prefix: empty");
    }

    #[tokio::test]
    async fn test_match_route_root_fallback() {
        let router = HttpRouter::new();

        router.add_route("/", "Root", "static", test_handler()).await.unwrap();
        router.add_route("/api", "API", "proxy", test_handler()).await.unwrap();

        // /api/foo should match /api (more specific than /)
        let matched = router.match_route("/api/foo").await;
        assert_eq!(matched.unwrap().prefix, "/api");

        // /other should match / (root fallback)
        let matched = router.match_route("/other").await;
        assert_eq!(matched.unwrap().prefix, "/");
    }

    #[tokio::test]
    async fn test_match_route_no_match() {
        let router = HttpRouter::new();

        router.add_route("/api", "API", "proxy", test_handler()).await.unwrap();

        // Path that doesn't match any prefix
        let matched = router.match_route("/other/path").await;
        assert!(matched.is_none());
    }

    #[tokio::test]
    async fn test_routes_sorted_by_length() {
        let router = HttpRouter::new();

        // Add in reverse order of specificity
        router.add_route("/a", "Short", "proxy", test_handler()).await.unwrap();
        router.add_route("/a/b/c", "Long", "proxy", test_handler()).await.unwrap();
        router.add_route("/a/b", "Medium", "proxy", test_handler()).await.unwrap();

        let routes = router.list_routes().await;
        // Should be sorted longest-first
        assert_eq!(routes[0].prefix, "/a/b/c");
        assert_eq!(routes[1].prefix, "/a/b");
        assert_eq!(routes[2].prefix, "/a");
    }

    #[tokio::test]
    async fn test_add_route_after_remove_same_prefix() {
        let router = HttpRouter::new();

        router.add_route("/api", "Old API", "proxy", test_handler()).await.unwrap();
        router.remove_route("/api").await.unwrap();

        // Should be able to re-add the same prefix
        router.add_route("/api", "New API", "proxy", test_handler()).await.unwrap();
        let routes = router.list_routes().await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].name, "New API");
    }

    #[tokio::test]
    async fn test_normalize_prefix_used_for_dedup() {
        let router = HttpRouter::new();

        router.add_route("api/", "API A", "proxy", test_handler()).await.unwrap();

        // These should all be normalized to "/api" and thus conflict
        let result = router.add_route("/api", "API B", "proxy", test_handler()).await;
        assert!(matches!(result, Err(RouterError::DuplicatePrefix(_))));

        let result = router.add_route("/api/", "API C", "proxy", test_handler()).await;
        assert!(matches!(result, Err(RouterError::DuplicatePrefix(_))));

        let result = router.add_route("api", "API D", "proxy", test_handler()).await;
        assert!(matches!(result, Err(RouterError::DuplicatePrefix(_))));
    }

    #[tokio::test]
    async fn test_empty_router_match_returns_none() {
        let router = HttpRouter::new();
        let matched = router.match_route("/anything").await;
        assert!(matched.is_none());
    }

    #[test]
    fn test_path_matching() {
        // Root matches everything
        assert!(path_matches("/", "/"));
        assert!(path_matches("/anything", "/"));
        assert!(path_matches("/a/b/c", "/"));

        // Exact match
        assert!(path_matches("/ws", "/ws"));
        assert!(path_matches("/api", "/api"));

        // Prefix match with /
        assert!(path_matches("/api/v1", "/api"));
        assert!(path_matches("/api/v1/users", "/api"));

        // No partial prefix match (path boundary)
        assert!(!path_matches("/apis", "/api"));
        assert!(!path_matches("/ws2", "/ws"));

        // No match at all
        assert!(!path_matches("/other", "/ws"));
    }

    #[tokio::test]
    async fn test_peer_info_from_bridge_header() {
        let header = crate::bridge::header::BridgeHeader {
            session_token: [0u8; 32],
            direction: crate::bridge::header::Direction::Incoming,
            service_port: 443,
            request_id: String::new(),
            remote_addr: "100.64.0.2:12345".to_string(),
            remote_dns_name: "peer.tail.ts.net".to_string(),
        };

        let peer = PeerInfo::from(&header);
        assert_eq!(peer.remote_addr, "100.64.0.2:12345");
        assert_eq!(peer.remote_dns_name, "peer.tail.ts.net");
    }

    #[tokio::test]
    async fn test_set_fallback() {
        let router = HttpRouter::new();
        router
            .set_fallback(Arc::new(StatusHandler(StatusCode::IM_A_TEAPOT)))
            .await;

        // No routes, so any path should hit the fallback
        let matched = router.match_route("/anything").await;
        // match_route doesn't use fallback, only dispatch does
        assert!(matched.is_none());
    }

    #[tokio::test]
    async fn test_router_default() {
        let router = HttpRouter::default();
        let routes = router.list_routes().await;
        assert!(routes.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════
    // Adversarial edge cases
    // ═══════════════════════════════════════════════════════════════════

    /// Register "/api/", request "/api/users". Trailing slash should be
    /// stripped during normalization so the route matches sub-paths.
    #[tokio::test]
    async fn test_trailing_slash_normalization_match() {
        let router = HttpRouter::new();
        router
            .add_route("/api/", "API", "proxy", test_handler())
            .await
            .unwrap();

        // Normalized to "/api", should match /api/users
        let matched = router.match_route("/api/users").await;
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().prefix, "/api");

        // Also matches /api exactly
        let matched = router.match_route("/api").await;
        assert!(matched.is_some());
    }

    /// Register "api" (no leading slash). Should be normalized to "/api".
    #[tokio::test]
    async fn test_no_leading_slash_normalized() {
        let router = HttpRouter::new();
        router
            .add_route("api", "API", "proxy", test_handler())
            .await
            .unwrap();

        let routes = router.list_routes().await;
        assert_eq!(routes[0].prefix, "/api");

        let matched = router.match_route("/api/foo").await;
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().prefix, "/api");
    }

    /// Register "" (empty string). Should normalize to "/" and act as catch-all.
    #[tokio::test]
    async fn test_empty_prefix_becomes_root() {
        let router = HttpRouter::new();
        router
            .add_route("", "Root", "static", test_handler())
            .await
            .unwrap();

        let routes = router.list_routes().await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].prefix, "/");

        // Root "/" matches everything
        let matched = router.match_route("/anything/at/all").await;
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().prefix, "/");
    }

    /// Remove a route while holding a snapshot of the handler Arc.
    /// The snapshot should remain valid (Arc keeps the handler alive)
    /// even after the route is removed from the router. This is the
    /// same pattern used in `dispatch()` -- it clones the handler Arc
    /// before releasing the read lock.
    #[tokio::test]
    async fn test_route_removal_snapshot_semantics() {
        let router = HttpRouter::new();
        let handler = test_handler();
        let handler_weak = Arc::downgrade(&handler);

        router
            .add_route("/api", "API", "proxy", handler)
            .await
            .unwrap();

        // Simulate what dispatch() does: grab the handler Arc (snapshot)
        let handler_snapshot = {
            let routes = router.routes.read().await;
            routes
                .iter()
                .find(|r| r.prefix == "/api")
                .map(|r| r.handler.clone())
        };

        assert!(handler_snapshot.is_some());
        let snapshot = handler_snapshot.unwrap();

        // Remove the route while we hold a snapshot
        router.remove_route("/api").await.unwrap();
        assert!(router.list_routes().await.is_empty());

        // The route table no longer references the handler, but our
        // snapshot Arc keeps it alive
        assert!(
            Arc::strong_count(&snapshot) >= 1,
            "snapshot should keep handler alive"
        );

        // The weak reference from before registration is still upgradable
        // only if our snapshot is alive
        assert!(
            handler_weak.upgrade().is_some(),
            "weak ref should still be upgradable via snapshot"
        );

        // Drop the snapshot -- now the handler should be deallocated
        drop(snapshot);
        assert!(
            handler_weak.upgrade().is_none(),
            "after dropping snapshot, handler should be deallocated"
        );
    }

    /// Register 100 different prefixes, verify all match correctly.
    #[tokio::test]
    async fn test_100_routes_registered() {
        let router = HttpRouter::new();

        for i in 0..100 {
            let prefix = format!("/route{i:03}");
            router
                .add_route(&prefix, &format!("Route {i}"), "handler", test_handler())
                .await
                .unwrap();
        }

        assert_eq!(router.list_routes().await.len(), 100);

        // Verify each route matches
        for i in 0..100 {
            let path = format!("/route{i:03}/sub");
            let matched = router.match_route(&path).await;
            assert!(matched.is_some(), "route{i:03} should match {path}");
            assert_eq!(matched.unwrap().prefix, format!("/route{i:03}"));
        }

        // Verify a non-matching path returns None
        let matched = router.match_route("/nomatch").await;
        assert!(matched.is_none());
    }

    /// Request "/api/users?page=2". Query string should not affect routing.
    #[test]
    fn test_path_with_query_string() {
        // path_matches works on path only; the URI parser strips query strings
        // from `.path()`. Verify the function works on raw paths.
        assert!(path_matches("/api/users", "/api"));
        assert!(path_matches("/api", "/api"));

        // Simulate what hyper does: URI.path() returns just the path portion
        let uri: hyper::Uri = "/api/users?page=2".parse().unwrap();
        assert_eq!(uri.path(), "/api/users");
        assert!(path_matches(uri.path(), "/api"));
    }

    /// Request "/api/users#section". Fragment should not affect routing.
    #[test]
    fn test_path_with_fragment() {
        // Fragments are stripped by URI parsers before reaching the server.
        // Hyper's URI parser also strips them. Verify the path still matches.
        let uri: hyper::Uri = "/api/users".parse().unwrap();
        assert!(path_matches(uri.path(), "/api"));

        // Note: HTTP spec says fragments are never sent to the server.
        // Hyper strips them during parsing. This test documents that behavior.
    }

    /// Request "//api//users" (double slashes). Should not cause a panic.
    #[test]
    fn test_double_slash_in_path() {
        // Double slashes should not crash path_matches
        assert!(!path_matches("//api//users", "/api"));  // won't match /api prefix
        assert!(path_matches("//api//users", "/"));       // root catches all
        assert!(path_matches("//api//users", "//api"));   // exact prefix matches

        // Normalization doesn't collapse double slashes (intentionally)
        let normalized = normalize_prefix("//api");
        assert_eq!(normalized, "//api");
    }

    /// Request "/api/users%2Fadmin" (encoded slash). Verify it doesn't
    /// bypass prefix matching by injecting path separators.
    #[test]
    fn test_encoded_path_segments() {
        // %2F is an encoded `/`. In the raw path, it's a literal string.
        // path_matches works on the raw path, so %2F won't be treated as `/`.
        let path = "/api/users%2Fadmin";

        // This matches /api because the path starts with "/api/"
        assert!(path_matches(path, "/api"));

        // But critically, it should NOT match a deeper prefix like "/api/users/admin"
        // because %2F is not a real path separator
        assert!(!path_matches(path, "/api/users/admin"));

        // The encoded slash stays encoded in the prefix match
        assert!(path_matches(path, "/api/users%2Fadmin"));
    }

    /// Path matching with prefix that is a substring of a longer segment
    /// should NOT match (e.g., /api should NOT match /apis).
    #[test]
    fn test_path_boundary_enforcement() {
        assert!(!path_matches("/apis", "/api"));
        assert!(!path_matches("/apis/v1", "/api"));
        assert!(!path_matches("/apiary", "/api"));
        assert!(!path_matches("/api-v2", "/api"));

        // But these should match
        assert!(path_matches("/api/v2", "/api"));
        assert!(path_matches("/api", "/api"));
    }
}
