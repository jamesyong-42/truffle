//! PWA handler -- serves Progressive Web App assets (RFC 008 Phase 5).
//!
//! Serves the PWA manifest, service worker, and icon files at a configurable
//! prefix (default: `/pwa`). Implements `HttpHandler` for registration with
//! the `HttpRouter`.
//!
//! ## Architecture
//!
//! `PwaHandler` pre-serializes the manifest and service worker at construction
//! time. Icon files are stored as `(Bytes, mime_type)` pairs. All assets are
//! served from memory -- no disk I/O at request time.
//!
//! The service worker is a minimal caching shell that caches the app shell
//! and passes through API/WebSocket requests. Applications can provide their
//! own service worker for more sophisticated caching strategies.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use super::router::{HttpHandler, PeerInfo};

// ═══════════════════════════════════════════════════════════════════════════
// Config types
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for PWA manifest generation.
#[derive(Debug, Clone)]
pub struct PwaConfig {
    /// Application name (displayed on install prompt and splash screen).
    pub name: String,
    /// Short name (displayed on home screen / app launcher).
    pub short_name: String,
    /// Start URL when the app is launched (default: "/").
    pub start_url: String,
    /// Display mode: "standalone", "fullscreen", "minimal-ui", or "browser".
    pub display: String,
    /// Theme color (CSS color, e.g., "#1a1a2e").
    pub theme_color: String,
    /// Background color for the splash screen.
    pub background_color: String,
    /// PWA icons to include in the manifest.
    pub icons: Vec<PwaIcon>,
    /// Optional custom service worker JavaScript source.
    /// If `None`, a default caching service worker is generated.
    pub custom_service_worker: Option<String>,
}

impl Default for PwaConfig {
    fn default() -> Self {
        Self {
            name: "Truffle App".to_string(),
            short_name: "Truffle".to_string(),
            start_url: "/".to_string(),
            display: "standalone".to_string(),
            theme_color: "#1a1a2e".to_string(),
            background_color: "#1a1a2e".to_string(),
            icons: Vec::new(),
            custom_service_worker: None,
        }
    }
}

/// A PWA icon entry for the manifest.
#[derive(Debug, Clone)]
pub struct PwaIcon {
    /// URL path relative to the PWA prefix (e.g., "icons/icon-192.png").
    pub src: String,
    /// Icon dimensions (e.g., "192x192").
    pub sizes: String,
    /// MIME type (e.g., "image/png").
    pub mime_type: String,
    /// The icon file content.
    pub data: Bytes,
}

// ═══════════════════════════════════════════════════════════════════════════
// PwaHandler
// ═══════════════════════════════════════════════════════════════════════════

/// Serves PWA assets: manifest, service worker, and icons.
///
/// Register with the router at a prefix (e.g., "/pwa") to serve:
/// - `GET {prefix}/manifest.json` -- the web app manifest
/// - `GET {prefix}/sw.js` -- service worker script
/// - `GET {prefix}/icons/{name}` -- icon files
///
/// All assets are served from memory with appropriate caching headers.
pub struct PwaHandler {
    /// Pre-serialized manifest JSON.
    manifest_json: Bytes,
    /// Service worker JavaScript source.
    service_worker_js: Bytes,
    /// Icon files: relative_path -> (content, mime_type).
    /// Paths are normalized (no leading slash).
    icons: HashMap<String, (Bytes, String)>,
    /// The route prefix this handler is mounted at (e.g., "/pwa").
    prefix: String,
}

impl PwaHandler {
    /// Create a new PWA handler from a configuration.
    ///
    /// The manifest is pre-serialized to JSON. Icons are stored in memory.
    /// The service worker is either the custom one from config or a default
    /// caching shell.
    pub fn new(config: &PwaConfig, prefix: &str) -> Self {
        let normalized_prefix = normalize_pwa_prefix(prefix);

        let manifest_json = build_manifest(config, &normalized_prefix);
        let service_worker_js = config
            .custom_service_worker
            .clone()
            .unwrap_or_else(|| build_default_service_worker(&normalized_prefix));

        let mut icons = HashMap::new();
        for icon in &config.icons {
            let normalized = icon.src.trim_start_matches('/').to_string();
            icons.insert(normalized, (icon.data.clone(), icon.mime_type.clone()));
        }

        Self {
            manifest_json: Bytes::from(manifest_json),
            service_worker_js: Bytes::from(service_worker_js),
            icons,
            prefix: normalized_prefix,
        }
    }

    /// Resolve a request path to a PWA asset.
    ///
    /// Returns `(content, mime_type)` if the path matches a known asset,
    /// or `None` for a 404.
    fn resolve(&self, path: &str) -> Option<(Bytes, &str)> {
        // Strip the prefix to get the relative path
        let relative = strip_prefix(path, &self.prefix);

        match relative {
            "/manifest.json" | "manifest.json" => {
                Some((self.manifest_json.clone(), "application/manifest+json"))
            }
            "/sw.js" | "sw.js" => {
                Some((self.service_worker_js.clone(), "application/javascript"))
            }
            other => {
                // Try icons: strip leading "icons/" or "/icons/"
                let icon_path = other
                    .trim_start_matches('/')
                    .trim_start_matches("icons/");
                // Also try the full relative path (without leading slash)
                let relative_clean = other.trim_start_matches('/');

                if let Some((content, mime)) = self.icons.get(icon_path) {
                    Some((content.clone(), mime.as_str()))
                } else if let Some((content, mime)) = self.icons.get(relative_clean) {
                    Some((content.clone(), mime.as_str()))
                } else {
                    None
                }
            }
        }
    }
}

impl HttpHandler for PwaHandler {
    fn handle(
        &self,
        req: Request<Incoming>,
        _peer: PeerInfo,
    ) -> Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send + '_>> {
        let path = req.uri().path().to_string();
        Box::pin(async move {
            match self.resolve(&path) {
                Some((content, mime)) => {
                    // Service worker must not be cached aggressively
                    let cache = if mime == "application/javascript" && path.contains("sw.js") {
                        "no-cache"
                    } else if mime == "application/manifest+json" {
                        "public, max-age=3600"
                    } else {
                        "public, max-age=86400"
                    };

                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", mime)
                        .header("cache-control", cache)
                        .body(Full::new(content))
                        .unwrap_or_else(|_| {
                            let mut resp =
                                Response::new(Full::new(Bytes::from("Internal Error")));
                            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                            resp
                        })
                }
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

/// Normalize a PWA prefix: ensure leading `/`, strip trailing `/`.
fn normalize_pwa_prefix(prefix: &str) -> String {
    let mut p = prefix.to_string();
    if !p.starts_with('/') {
        p = format!("/{p}");
    }
    if p.len() > 1 && p.ends_with('/') {
        p.pop();
    }
    p
}

/// Strip a prefix from a path, returning the remainder.
fn strip_prefix<'a>(path: &'a str, prefix: &str) -> &'a str {
    if prefix == "/" {
        return path;
    }
    path.strip_prefix(prefix).unwrap_or(path)
}

/// Build the PWA manifest JSON from config.
fn build_manifest(config: &PwaConfig, prefix: &str) -> String {
    let icons: Vec<serde_json::Value> = config
        .icons
        .iter()
        .map(|icon| {
            serde_json::json!({
                "src": format!("{}/icons/{}", prefix, icon.src.trim_start_matches('/')),
                "sizes": icon.sizes,
                "type": icon.mime_type,
            })
        })
        .collect();

    let manifest = serde_json::json!({
        "name": config.name,
        "short_name": config.short_name,
        "start_url": config.start_url,
        "display": config.display,
        "theme_color": config.theme_color,
        "background_color": config.background_color,
        "icons": icons,
    });

    serde_json::to_string_pretty(&manifest).unwrap_or_else(|_| "{}".to_string())
}

/// Build the default service worker JavaScript.
///
/// This is a minimal caching shell that:
/// - Caches the app shell (manifest, icons) on install
/// - Serves cached assets when offline
/// - Passes through API and WebSocket requests
/// - Listens for push events and displays notifications
fn build_default_service_worker(prefix: &str) -> String {
    format!(
        r#"// Truffle PWA Service Worker (auto-generated)
const CACHE_NAME = 'truffle-pwa-v1';
const SHELL_URLS = [
  '{prefix}/manifest.json',
  '/',
];

// Install: cache the app shell
self.addEventListener('install', (event) => {{
  event.waitUntil(
    caches.open(CACHE_NAME).then((cache) => cache.addAll(SHELL_URLS))
  );
  self.skipWaiting();
}});

// Activate: clean old caches
self.addEventListener('activate', (event) => {{
  event.waitUntil(
    caches.keys().then((names) =>
      Promise.all(
        names
          .filter((name) => name !== CACHE_NAME)
          .map((name) => caches.delete(name))
      )
    )
  );
  self.clients.claim();
}});

// Fetch: network-first with cache fallback
self.addEventListener('fetch', (event) => {{
  const url = new URL(event.request.url);

  // Skip caching for WebSocket upgrades and API calls
  if (url.pathname.startsWith('/ws') || url.pathname.startsWith('/api/')) {{
    return;
  }}

  event.respondWith(
    fetch(event.request)
      .then((response) => {{
        // Cache successful GET responses
        if (event.request.method === 'GET' && response.status === 200) {{
          const clone = response.clone();
          caches.open(CACHE_NAME).then((cache) => cache.put(event.request, clone));
        }}
        return response;
      }})
      .catch(() => caches.match(event.request))
  );
}});

// Push: display notification
self.addEventListener('push', (event) => {{
  let data = {{}};
  try {{
    data = event.data ? event.data.json() : {{}};
  }} catch (e) {{
    data = {{ title: 'Notification', body: event.data ? event.data.text() : '' }};
  }}

  const title = data.title || 'Truffle';
  const options = {{
    body: data.body || '',
    icon: data.icon || '{prefix}/icons/icon-192.png',
    badge: data.badge,
    data: data.data,
    tag: data.tag,
    requireInteraction: data.requireInteraction || false,
  }};

  event.waitUntil(self.registration.showNotification(title, options));
}});

// Notification click: focus or open the app
self.addEventListener('notificationclick', (event) => {{
  event.notification.close();
  event.waitUntil(
    self.clients.matchAll({{ type: 'window' }}).then((clients) => {{
      if (clients.length > 0) {{
        return clients[0].focus();
      }}
      return self.clients.openWindow('/');
    }})
  );
}});
"#
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PwaConfig {
        PwaConfig {
            name: "Test App".to_string(),
            short_name: "Test".to_string(),
            start_url: "/".to_string(),
            display: "standalone".to_string(),
            theme_color: "#000000".to_string(),
            background_color: "#ffffff".to_string(),
            icons: vec![
                PwaIcon {
                    src: "icon-192.png".to_string(),
                    sizes: "192x192".to_string(),
                    mime_type: "image/png".to_string(),
                    data: Bytes::from(vec![0x89, 0x50, 0x4E, 0x47]), // PNG magic bytes
                },
                PwaIcon {
                    src: "icon-512.png".to_string(),
                    sizes: "512x512".to_string(),
                    mime_type: "image/png".to_string(),
                    data: Bytes::from(vec![0x89, 0x50, 0x4E, 0x47]),
                },
            ],
            custom_service_worker: None,
        }
    }

    #[test]
    fn test_manifest_generation() {
        let config = test_config();
        let manifest = build_manifest(&config, "/pwa");
        let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();

        assert_eq!(parsed["name"], "Test App");
        assert_eq!(parsed["short_name"], "Test");
        assert_eq!(parsed["start_url"], "/");
        assert_eq!(parsed["display"], "standalone");
        assert_eq!(parsed["theme_color"], "#000000");
        assert_eq!(parsed["background_color"], "#ffffff");

        let icons = parsed["icons"].as_array().unwrap();
        assert_eq!(icons.len(), 2);
        assert_eq!(icons[0]["src"], "/pwa/icons/icon-192.png");
        assert_eq!(icons[0]["sizes"], "192x192");
        assert_eq!(icons[0]["type"], "image/png");
        assert_eq!(icons[1]["src"], "/pwa/icons/icon-512.png");
        assert_eq!(icons[1]["sizes"], "512x512");
    }

    #[test]
    fn test_default_service_worker_contains_push_handler() {
        let sw = build_default_service_worker("/pwa");
        assert!(sw.contains("addEventListener('push'"));
        assert!(sw.contains("addEventListener('install'"));
        assert!(sw.contains("addEventListener('fetch'"));
        assert!(sw.contains("addEventListener('notificationclick'"));
        assert!(sw.contains("/pwa/manifest.json"));
    }

    #[test]
    fn test_pwa_handler_serves_manifest() {
        let config = test_config();
        let handler = PwaHandler::new(&config, "/pwa");

        let result = handler.resolve("/pwa/manifest.json");
        assert!(result.is_some());
        let (content, mime) = result.unwrap();
        assert_eq!(mime, "application/manifest+json");

        let parsed: serde_json::Value = serde_json::from_slice(&content).unwrap();
        assert_eq!(parsed["name"], "Test App");
    }

    #[test]
    fn test_pwa_handler_serves_sw() {
        let config = test_config();
        let handler = PwaHandler::new(&config, "/pwa");

        let result = handler.resolve("/pwa/sw.js");
        assert!(result.is_some());
        let (content, mime) = result.unwrap();
        assert_eq!(mime, "application/javascript");
        let sw_text = String::from_utf8(content.to_vec()).unwrap();
        assert!(sw_text.contains("addEventListener"));
    }

    #[test]
    fn test_pwa_handler_serves_icons() {
        let config = test_config();
        let handler = PwaHandler::new(&config, "/pwa");

        // Icon via /pwa/icons/icon-192.png
        let result = handler.resolve("/pwa/icons/icon-192.png");
        assert!(result.is_some());
        let (content, mime) = result.unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(content.len(), 4); // PNG magic bytes

        // Second icon
        let result = handler.resolve("/pwa/icons/icon-512.png");
        assert!(result.is_some());
    }

    #[test]
    fn test_pwa_handler_404_for_unknown() {
        let config = test_config();
        let handler = PwaHandler::new(&config, "/pwa");

        let result = handler.resolve("/pwa/unknown.txt");
        assert!(result.is_none());

        let result = handler.resolve("/other/path");
        assert!(result.is_none());
    }

    #[test]
    fn test_pwa_handler_custom_service_worker() {
        let config = PwaConfig {
            custom_service_worker: Some("// my custom sw".to_string()),
            ..test_config()
        };
        let handler = PwaHandler::new(&config, "/pwa");

        let result = handler.resolve("/pwa/sw.js");
        assert!(result.is_some());
        let (content, _) = result.unwrap();
        assert_eq!(content, Bytes::from("// my custom sw"));
    }

    #[test]
    fn test_normalize_pwa_prefix() {
        assert_eq!(normalize_pwa_prefix("pwa"), "/pwa");
        assert_eq!(normalize_pwa_prefix("/pwa"), "/pwa");
        assert_eq!(normalize_pwa_prefix("/pwa/"), "/pwa");
        assert_eq!(normalize_pwa_prefix("/"), "/");
    }

    #[test]
    fn test_strip_prefix() {
        assert_eq!(strip_prefix("/pwa/manifest.json", "/pwa"), "/manifest.json");
        assert_eq!(strip_prefix("/pwa/sw.js", "/pwa"), "/sw.js");
        assert_eq!(strip_prefix("/pwa/icons/icon.png", "/pwa"), "/icons/icon.png");
        // Root prefix returns full path
        assert_eq!(strip_prefix("/manifest.json", "/"), "/manifest.json");
    }

    #[test]
    fn test_default_config() {
        let config = PwaConfig::default();
        assert_eq!(config.name, "Truffle App");
        assert_eq!(config.short_name, "Truffle");
        assert_eq!(config.display, "standalone");
        assert!(config.icons.is_empty());
        assert!(config.custom_service_worker.is_none());
    }

    #[test]
    fn test_manifest_no_icons() {
        let config = PwaConfig {
            icons: Vec::new(),
            ..test_config()
        };
        let manifest = build_manifest(&config, "/pwa");
        let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        let icons = parsed["icons"].as_array().unwrap();
        assert!(icons.is_empty());
    }

    #[test]
    fn test_pwa_handler_empty_icons() {
        let config = PwaConfig {
            icons: Vec::new(),
            ..test_config()
        };
        let handler = PwaHandler::new(&config, "/pwa");
        assert!(handler.icons.is_empty());

        // Manifest and SW should still work
        assert!(handler.resolve("/pwa/manifest.json").is_some());
        assert!(handler.resolve("/pwa/sw.js").is_some());
    }
}
