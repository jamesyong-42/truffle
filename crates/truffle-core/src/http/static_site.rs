//! Static site handler -- serves files from disk or in-memory (RFC 008 Phase 4).
//!
//! Supports SPA fallback mode (serves index.html for unknown paths),
//! automatic MIME type detection, and content-hash-aware caching.
//! Implements `HttpHandler` so it can be registered with the `HttpRouter`.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use tokio::sync::RwLock;

use super::router::{HttpHandler, PeerInfo};

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

/// A static file to serve from memory.
#[derive(Debug, Clone)]
pub struct StaticFile {
    /// Relative path (e.g., "app.js", "css/style.css").
    pub path: String,
    /// File content.
    pub content: Bytes,
    /// MIME type (e.g., "application/javascript").
    pub mime: String,
}

/// Cache control configuration for a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheHeader {
    pub value: String,
}

impl CacheHeader {
    /// Default cache header for normal files.
    pub fn default_cache() -> Self {
        Self {
            value: "public, max-age=3600".to_string(),
        }
    }

    /// Immutable cache header for content-hashed files.
    pub fn immutable() -> Self {
        Self {
            value: "public, max-age=31536000, immutable".to_string(),
        }
    }
}

/// Result of resolving a static file request.
#[derive(Debug, Clone)]
pub struct ResolvedFile {
    /// The file content.
    pub content: Bytes,
    /// The MIME type.
    pub mime: String,
    /// The cache header to use.
    pub cache: CacheHeader,
}

// ═══════════════════════════════════════════════════════════════════════════
// StaticHandler
// ═══════════════════════════════════════════════════════════════════════════

/// Serves static files from a directory or in-memory file map.
///
/// Supports SPA fallback (returns index.html for unmatched paths),
/// automatic MIME detection, and content-hash-aware caching.
pub struct StaticHandler {
    /// Root directory for file serving (optional).
    root: Option<PathBuf>,
    /// In-memory file map: normalized_path -> (content, mime_type).
    memory_files: Arc<RwLock<HashMap<String, (Bytes, String)>>>,
    /// SPA mode: serve index.html for unmatched paths.
    spa_fallback: bool,
    /// Default cache-control header value.
    cache_control: String,
    /// Index file name (default: "index.html").
    index: String,
}

impl StaticHandler {
    /// Create a handler serving from a directory.
    pub fn from_dir(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Some(root.into()),
            memory_files: Arc::new(RwLock::new(HashMap::new())),
            spa_fallback: false,
            cache_control: "public, max-age=3600".to_string(),
            index: "index.html".to_string(),
        }
    }

    /// Create a handler serving from in-memory files.
    pub fn from_memory(files: Vec<StaticFile>) -> Self {
        let mut map = HashMap::new();
        for file in files {
            let normalized = normalize_file_path(&file.path);
            map.insert(normalized, (file.content, file.mime));
        }

        Self {
            root: None,
            memory_files: Arc::new(RwLock::new(map)),
            spa_fallback: false,
            cache_control: "public, max-age=3600".to_string(),
            index: "index.html".to_string(),
        }
    }

    /// Enable or disable SPA fallback mode.
    pub fn with_spa_fallback(mut self, enabled: bool) -> Self {
        self.spa_fallback = enabled;
        self
    }

    /// Set the default cache-control header.
    pub fn with_cache_control(mut self, value: &str) -> Self {
        self.cache_control = value.to_string();
        self
    }

    /// Set the index file name (default: "index.html").
    pub fn with_index(mut self, name: &str) -> Self {
        self.index = name.to_string();
        self
    }

    /// Resolve a request path to a file (in-memory lookup).
    ///
    /// Returns `Some(ResolvedFile)` if the file is found, or `None` for 404.
    /// In SPA mode, returns index.html for unmatched paths.
    pub async fn serve_from_memory(&self, request_path: &str) -> Option<ResolvedFile> {
        let files = self.memory_files.read().await;

        let normalized = normalize_file_path(request_path);

        // Try exact match
        if let Some((content, mime)) = files.get(&normalized) {
            return Some(ResolvedFile {
                content: content.clone(),
                mime: mime.clone(),
                cache: cache_header_for_path(&normalized, &self.cache_control),
            });
        }

        // Try index file (e.g., "/" -> "/index.html")
        let index_path = if normalized == "/" || normalized.is_empty() {
            format!("/{}", self.index)
        } else {
            format!("{}/{}", normalized, self.index)
        };

        if let Some((content, mime)) = files.get(&index_path) {
            return Some(ResolvedFile {
                content: content.clone(),
                mime: mime.clone(),
                cache: cache_header_for_path(&index_path, &self.cache_control),
            });
        }

        // SPA fallback: return index.html for unmatched paths
        if self.spa_fallback {
            let index_key = format!("/{}", self.index);
            if let Some((content, mime)) = files.get(&index_key) {
                return Some(ResolvedFile {
                    content: content.clone(),
                    mime: mime.clone(),
                    cache: CacheHeader {
                        value: "no-cache".to_string(),
                    },
                });
            }
        }

        None
    }

    /// Resolve a file path for directory-based serving.
    ///
    /// Returns `None` for paths containing directory traversal (`..`).
    pub fn resolve_path(&self, request_path: &str) -> Option<PathBuf> {
        let root = self.root.as_ref()?;

        // Block directory traversal
        if request_path.contains("..") {
            return None;
        }

        let clean = request_path.trim_start_matches('/');
        let full_path = root.join(clean);

        // Verify the resolved path is within the root
        if !full_path.starts_with(root) {
            return None;
        }

        Some(full_path)
    }

    /// Serve a file from disk (async, uses `tokio::fs`).
    ///
    /// Reads the file at the resolved path. Returns `None` if the file
    /// doesn't exist or is a directory. In SPA mode, falls back to
    /// `{root}/index.html` for unmatched paths.
    pub async fn serve_from_disk(&self, request_path: &str) -> Option<ResolvedFile> {
        let root = self.root.as_ref()?;

        // Resolve the path (blocks traversal)
        let full_path = self.resolve_path(request_path)?;

        // Try the exact file
        if let Ok(content) = tokio::fs::read(&full_path).await {
            let mime = mime_for_extension(full_path.to_str().unwrap_or("")).to_string();
            let path_str = full_path.to_str().unwrap_or("");
            return Some(ResolvedFile {
                content: Bytes::from(content),
                mime,
                cache: cache_header_for_path(path_str, &self.cache_control),
            });
        }

        // Try index file in directory
        let index_path = full_path.join(&self.index);
        if let Ok(content) = tokio::fs::read(&index_path).await {
            let mime = mime_for_extension(index_path.to_str().unwrap_or("")).to_string();
            return Some(ResolvedFile {
                content: Bytes::from(content),
                mime,
                cache: cache_header_for_path(
                    index_path.to_str().unwrap_or(""),
                    &self.cache_control,
                ),
            });
        }

        // SPA fallback: try root/index.html
        if self.spa_fallback {
            let spa_path = root.join(&self.index);
            if let Ok(content) = tokio::fs::read(&spa_path).await {
                let mime = mime_for_extension(spa_path.to_str().unwrap_or("")).to_string();
                return Some(ResolvedFile {
                    content: Bytes::from(content),
                    mime,
                    cache: CacheHeader {
                        value: "no-cache".to_string(),
                    },
                });
            }
        }

        None
    }

    /// Serve a request, trying in-memory first, then disk.
    async fn serve(&self, request_path: &str) -> Option<ResolvedFile> {
        // Try in-memory first
        if let Some(file) = self.serve_from_memory(request_path).await {
            return Some(file);
        }

        // Try disk
        if self.root.is_some() {
            return self.serve_from_disk(request_path).await;
        }

        None
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// HttpHandler implementation
// ═══════════════════════════════════════════════════════════════════════════

impl HttpHandler for StaticHandler {
    fn handle(
        &self,
        req: Request<Incoming>,
        _peer: PeerInfo,
    ) -> Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send + '_>> {
        let path = req.uri().path().to_string();
        Box::pin(async move {
            match self.serve(&path).await {
                Some(file) => Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", &file.mime)
                    .header("cache-control", &file.cache.value)
                    .body(Full::new(file.content))
                    .unwrap_or_else(|_| {
                        let mut resp = Response::new(Full::new(Bytes::from("Internal Error")));
                        *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                        resp
                    }),
                None => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(Bytes::from("Not Found")))
                    .unwrap_or_else(|_| {
                        let mut resp = Response::new(Full::new(Bytes::from("Not Found")));
                        *resp.status_mut() = StatusCode::NOT_FOUND;
                        resp
                    }),
            }
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Normalize a file path: ensure leading `/`, strip trailing `/`.
fn normalize_file_path(path: &str) -> String {
    let mut p = path.to_string();

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

/// Detect MIME type from file extension.
pub fn mime_for_extension(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match ext {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "application/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "xml" => "application/xml",
        "txt" => "text/plain",
        "pdf" => "application/pdf",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Determine if a filename contains a content hash (e.g., app.a1b2c3d4.js).
///
/// Heuristic: at least 8 hex characters in the second-to-last segment
/// when split by `.`.
fn is_content_hashed(path: &str) -> bool {
    let filename = Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");

    let parts: Vec<&str> = filename.split('.').collect();
    if parts.len() >= 3 {
        // Check the second-to-last part (e.g., "a1b2c3d4" in "app.a1b2c3d4.js")
        let candidate = parts[parts.len() - 2];
        candidate.len() >= 8 && candidate.chars().all(|c| c.is_ascii_hexdigit())
        } else {
        false
    }
}

/// Get the appropriate cache header for a file path.
fn cache_header_for_path(path: &str, default: &str) -> CacheHeader {
    if is_content_hashed(path) {
        CacheHeader::immutable()
    } else {
        CacheHeader {
            value: default.to_string(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn test_memory_files() -> Vec<StaticFile> {
        vec![
            StaticFile {
                path: "index.html".to_string(),
                content: Bytes::from("<html>hello</html>"),
                mime: "text/html".to_string(),
            },
            StaticFile {
                path: "app.js".to_string(),
                content: Bytes::from("console.log('hello')"),
                mime: "application/javascript".to_string(),
            },
            StaticFile {
                path: "css/style.css".to_string(),
                content: Bytes::from("body { color: red; }"),
                mime: "text/css".to_string(),
            },
            StaticFile {
                path: "app.a1b2c3d4.js".to_string(),
                content: Bytes::from("hashed content"),
                mime: "application/javascript".to_string(),
            },
        ]
    }

    #[tokio::test]
    async fn test_serve_from_memory() {
        let handler = StaticHandler::from_memory(test_memory_files());

        // Serve a known file
        let result = handler.serve_from_memory("/app.js").await;
        assert!(result.is_some());
        let file = result.unwrap();
        assert_eq!(file.content, Bytes::from("console.log('hello')"));
        assert_eq!(file.mime, "application/javascript");

        // Serve nested file
        let result = handler.serve_from_memory("/css/style.css").await;
        assert!(result.is_some());
        let file = result.unwrap();
        assert_eq!(file.content, Bytes::from("body { color: red; }"));
        assert_eq!(file.mime, "text/css");
    }

    #[tokio::test]
    async fn test_serve_from_memory_404() {
        let handler = StaticHandler::from_memory(test_memory_files());

        // Unknown path returns None (404)
        let result = handler.serve_from_memory("/nonexistent.js").await;
        assert!(result.is_none());

        // Another unknown path
        let result = handler.serve_from_memory("/deep/nested/path.html").await;
        assert!(result.is_none());
    }

    #[test]
    fn test_directory_traversal_blocked() {
        let handler = StaticHandler::from_dir("/var/www");

        // Paths with .. should be blocked
        assert!(handler.resolve_path("/../etc/passwd").is_none());
        assert!(handler.resolve_path("/foo/../../etc/passwd").is_none());
        assert!(handler.resolve_path("..").is_none());
        assert!(handler.resolve_path("/foo/../bar").is_none());

        // Normal paths should work
        assert!(handler.resolve_path("/index.html").is_some());
        assert!(handler.resolve_path("/css/style.css").is_some());
        assert!(handler.resolve_path("/").is_some());
    }

    #[tokio::test]
    async fn test_spa_fallback_serves_index() {
        let handler = StaticHandler::from_memory(test_memory_files())
            .with_spa_fallback(true);

        // Known file still works
        let result = handler.serve_from_memory("/app.js").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().mime, "application/javascript");

        // Unknown path returns index.html (SPA fallback)
        let result = handler.serve_from_memory("/dashboard/settings").await;
        assert!(result.is_some());
        let file = result.unwrap();
        assert_eq!(file.content, Bytes::from("<html>hello</html>"));
        assert_eq!(file.mime, "text/html");
        // SPA fallback uses no-cache
        assert_eq!(file.cache.value, "no-cache");
    }

    #[tokio::test]
    async fn test_spa_fallback_disabled_returns_none() {
        let handler = StaticHandler::from_memory(test_memory_files());
        // spa_fallback is false by default

        let result = handler.serve_from_memory("/dashboard/settings").await;
        assert!(result.is_none());
    }

    #[test]
    fn test_mime_types() {
        assert_eq!(mime_for_extension("index.html"), "text/html");
        assert_eq!(mime_for_extension("page.htm"), "text/html");
        assert_eq!(mime_for_extension("style.css"), "text/css");
        assert_eq!(mime_for_extension("app.js"), "application/javascript");
        assert_eq!(mime_for_extension("module.mjs"), "application/javascript");
        assert_eq!(mime_for_extension("data.json"), "application/json");
        assert_eq!(mime_for_extension("image.png"), "image/png");
        assert_eq!(mime_for_extension("photo.jpg"), "image/jpeg");
        assert_eq!(mime_for_extension("photo.jpeg"), "image/jpeg");
        assert_eq!(mime_for_extension("anim.gif"), "image/gif");
        assert_eq!(mime_for_extension("logo.svg"), "image/svg+xml");
        assert_eq!(mime_for_extension("favicon.ico"), "image/x-icon");
        assert_eq!(mime_for_extension("font.woff"), "font/woff");
        assert_eq!(mime_for_extension("font.woff2"), "font/woff2");
        assert_eq!(mime_for_extension("font.ttf"), "font/ttf");
        assert_eq!(mime_for_extension("font.otf"), "font/otf");
        assert_eq!(mime_for_extension("module.wasm"), "application/wasm");
        assert_eq!(mime_for_extension("image.webp"), "image/webp");
        assert_eq!(mime_for_extension("image.avif"), "image/avif");
        assert_eq!(mime_for_extension("video.mp4"), "video/mp4");
        assert_eq!(mime_for_extension("video.webm"), "video/webm");
        assert_eq!(mime_for_extension("data.xml"), "application/xml");
        assert_eq!(mime_for_extension("readme.txt"), "text/plain");
        assert_eq!(mime_for_extension("doc.pdf"), "application/pdf");
        // Unknown extension
        assert_eq!(mime_for_extension("data.xyz"), "application/octet-stream");
        // No extension
        assert_eq!(mime_for_extension("Makefile"), "application/octet-stream");
    }

    #[tokio::test]
    async fn test_cache_header_hashed_file() {
        let handler = StaticHandler::from_memory(test_memory_files());

        let result = handler.serve_from_memory("/app.a1b2c3d4.js").await;
        assert!(result.is_some());
        let file = result.unwrap();
        assert_eq!(file.cache, CacheHeader::immutable());
        assert_eq!(file.cache.value, "public, max-age=31536000, immutable");
    }

    #[tokio::test]
    async fn test_cache_header_normal_file() {
        let handler = StaticHandler::from_memory(test_memory_files());

        let result = handler.serve_from_memory("/app.js").await;
        assert!(result.is_some());
        let file = result.unwrap();
        assert_eq!(file.cache, CacheHeader::default_cache());
        assert_eq!(file.cache.value, "public, max-age=3600");
    }

    #[test]
    fn test_is_content_hashed() {
        // Hashed files (>= 8 hex chars)
        assert!(is_content_hashed("app.a1b2c3d4.js"));
        assert!(is_content_hashed("vendor.abcdef01.css"));
        assert!(is_content_hashed("/assets/main.deadbeef.wasm"));

        // Not hashed (too short)
        assert!(!is_content_hashed("app.js"));
        assert!(!is_content_hashed("app.min.js")); // "min" is not hex
        assert!(!is_content_hashed("style.css"));

        // Not hashed (non-hex chars)
        assert!(!is_content_hashed("app.notahash.js"));

        // Not hashed (only 2 parts)
        assert!(!is_content_hashed("app.js"));
    }

    #[tokio::test]
    async fn test_index_fallback_for_root() {
        let handler = StaticHandler::from_memory(test_memory_files());

        // "/" should find "/index.html"
        let result = handler.serve_from_memory("/").await;
        assert!(result.is_some());
        let file = result.unwrap();
        assert_eq!(file.content, Bytes::from("<html>hello</html>"));
        assert_eq!(file.mime, "text/html");
    }

    #[tokio::test]
    async fn test_custom_cache_control() {
        let files = vec![StaticFile {
            path: "data.json".to_string(),
            content: Bytes::from("{}"),
            mime: "application/json".to_string(),
        }];

        let handler = StaticHandler::from_memory(files)
            .with_cache_control("private, no-cache");

        let result = handler.serve_from_memory("/data.json").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().cache.value, "private, no-cache");
    }

    #[test]
    fn test_resolve_path_within_root() {
        let handler = StaticHandler::from_dir("/srv/www");

        let resolved = handler.resolve_path("/css/style.css");
        assert!(resolved.is_some());
        assert_eq!(
            resolved.unwrap(),
            PathBuf::from("/srv/www/css/style.css")
        );
    }

    #[test]
    fn test_resolve_path_no_root() {
        // Handler created from memory has no root
        let handler = StaticHandler::from_memory(vec![]);
        let resolved = handler.resolve_path("/anything");
        assert!(resolved.is_none());
    }

    #[tokio::test]
    async fn test_from_memory_normalizes_paths() {
        // Paths without leading slash should still work
        let files = vec![StaticFile {
            path: "no-slash.txt".to_string(),
            content: Bytes::from("content"),
            mime: "text/plain".to_string(),
        }];

        let handler = StaticHandler::from_memory(files);
        let result = handler.serve_from_memory("/no-slash.txt").await;
        assert!(result.is_some());
    }
}
