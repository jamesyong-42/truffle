//! Web Push manager and API handler (RFC 008 Phase 5).
//!
//! Implements VAPID key generation, push subscription management, and the
//! HTTP API endpoints for browser push notification integration.
//!
//! ## Architecture
//!
//! `PushManager` owns the VAPID key pair and subscription store. It provides
//! methods for subscription CRUD and notification delivery.
//!
//! `PushApiHandler` implements `HttpHandler` and serves the push API endpoints:
//! - `GET /api/push/vapid-key` -- returns the public VAPID key
//! - `POST /api/push/subscribe` -- registers a push subscription
//! - `POST /api/push/unsubscribe` -- removes a push subscription
//!
//! ## VAPID Keys
//!
//! VAPID (Voluntary Application Server Identification) uses ECDSA P-256 keys
//! to identify the application server to push services. The key pair is
//! generated once and persisted to disk so subscriptions remain valid across
//! restarts.
//!
//! ## Push Delivery
//!
//! Actual push message delivery requires Web Push encryption (RFC 8291) and
//! VAPID JWT signing (RFC 8292). The delivery method signatures are defined
//! but the crypto implementation is deferred -- it will use the `web-push`
//! crate or manual implementation in a future iteration.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::router::{HttpHandler, PeerInfo};

// ═══════════════════════════════════════════════════════════════════════════
// VAPID key types
// ═══════════════════════════════════════════════════════════════════════════

/// ECDSA P-256 key pair for VAPID authentication.
///
/// The private key is used to sign push messages (JWT). The public key is
/// shared with browsers for `PushManager.subscribe({ applicationServerKey })`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VapidKeys {
    /// Base64url-encoded uncompressed P-256 public key (65 bytes).
    pub public_key: String,
    /// Base64url-encoded P-256 private key scalar (32 bytes).
    pub private_key: String,
}

impl VapidKeys {
    /// Generate a new VAPID key pair using the `p256` crate.
    ///
    /// The keys are ECDSA P-256 as required by the VAPID spec (RFC 8292).
    pub fn generate() -> Result<Self, PushError> {
        use p256::ecdsa::SigningKey;
        use p256::elliptic_curve::rand_core::OsRng;

        let signing_key = SigningKey::random(&mut OsRng);
        let verify_key = signing_key.verifying_key();

        // Encode private key as base64url (32-byte scalar)
        let private_bytes = signing_key.to_bytes();
        let private_key = base64url_encode(&private_bytes);

        // Encode public key as base64url (65-byte uncompressed point)
        let public_point = verify_key.to_encoded_point(false);
        let public_key = base64url_encode(public_point.as_bytes());

        Ok(Self {
            public_key,
            private_key,
        })
    }

    /// Load VAPID keys from a JSON file, or generate and save if missing.
    pub async fn load_or_generate(path: &Path) -> Result<Self, PushError> {
        // Try to load existing keys
        if path.exists() {
            let content = tokio::fs::read_to_string(path).await.map_err(|e| {
                PushError::KeyStorage(format!("failed to read VAPID keys from {}: {e}", path.display()))
            })?;
            let keys: VapidKeys = serde_json::from_str(&content).map_err(|e| {
                PushError::KeyStorage(format!("failed to parse VAPID keys: {e}"))
            })?;
            return Ok(keys);
        }

        // Generate new keys
        let keys = Self::generate()?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                PushError::KeyStorage(format!("failed to create directory {}: {e}", parent.display()))
            })?;
        }

        // Save to disk
        let json = serde_json::to_string_pretty(&keys).map_err(|e| {
            PushError::KeyStorage(format!("failed to serialize VAPID keys: {e}"))
        })?;
        tokio::fs::write(path, &json).await.map_err(|e| {
            PushError::KeyStorage(format!("failed to write VAPID keys to {}: {e}", path.display()))
        })?;

        Ok(keys)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Push subscription types
// ═══════════════════════════════════════════════════════════════════════════

/// A Web Push subscription from a browser client.
///
/// Contains the push service endpoint and the encryption keys needed
/// to encrypt messages for this specific subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushSubscription {
    /// Push service endpoint URL (e.g., `https://fcm.googleapis.com/fcm/send/...`).
    pub endpoint: String,
    /// Encryption keys from the browser.
    pub keys: PushKeys,
    /// Optional device ID that created this subscription.
    #[serde(default)]
    pub device_id: Option<String>,
    /// Unix timestamp when the subscription was created.
    pub created_at: u64,
}

/// Encryption keys from a push subscription.
///
/// These are provided by the browser's `PushSubscription.getKey()` API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushKeys {
    /// Base64url-encoded P-256 DH public key from the browser.
    pub p256dh: String,
    /// Base64url-encoded 16-byte authentication secret.
    pub auth: String,
}

/// Payload for a push notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushPayload {
    /// Notification title.
    pub title: String,
    /// Notification body text.
    pub body: String,
    /// Optional icon URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Optional badge URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    /// Optional structured data payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Optional tag for notification grouping.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Whether the notification requires user interaction to dismiss.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_interaction: Option<bool>,
}

// ═══════════════════════════════════════════════════════════════════════════
// PushError
// ═══════════════════════════════════════════════════════════════════════════

/// Errors from push operations.
#[derive(Debug, Clone)]
pub enum PushError {
    /// VAPID key generation or storage failed.
    KeyStorage(String),
    /// Subscription not found.
    NotFound(String),
    /// Push delivery failed (future: when crypto is implemented).
    DeliveryFailed(String),
    /// Invalid request payload.
    InvalidPayload(String),
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PushError::KeyStorage(msg) => write!(f, "VAPID key error: {msg}"),
            PushError::NotFound(msg) => write!(f, "subscription not found: {msg}"),
            PushError::DeliveryFailed(msg) => write!(f, "push delivery failed: {msg}"),
            PushError::InvalidPayload(msg) => write!(f, "invalid payload: {msg}"),
        }
    }
}

impl std::error::Error for PushError {}

// ═══════════════════════════════════════════════════════════════════════════
// PushManager
// ═══════════════════════════════════════════════════════════════════════════

/// Manages VAPID keys, push subscriptions, and notification delivery.
///
/// Thread-safe: uses `RwLock` for the subscription store. Multiple
/// readers can check subscriptions concurrently; writes (subscribe/
/// unsubscribe) acquire exclusive access.
pub struct PushManager {
    /// VAPID key pair for signing push messages.
    vapid_keys: VapidKeys,
    /// Active push subscriptions, indexed by endpoint URL.
    subscriptions: RwLock<HashMap<String, PushSubscription>>,
}

impl PushManager {
    /// Create a new PushManager with the given VAPID keys.
    pub fn new(vapid_keys: VapidKeys) -> Self {
        Self {
            vapid_keys,
            subscriptions: RwLock::new(HashMap::new()),
        }
    }

    /// Create a PushManager, loading or generating VAPID keys from disk.
    pub async fn load_or_create(key_path: &Path) -> Result<Self, PushError> {
        let keys = VapidKeys::load_or_generate(key_path).await?;
        Ok(Self::new(keys))
    }

    /// Get the public VAPID key (base64url-encoded).
    ///
    /// This is the value browsers need for `PushManager.subscribe({
    ///   applicationServerKey: vapidPublicKey
    /// })`.
    pub fn vapid_public_key(&self) -> &str {
        &self.vapid_keys.public_key
    }

    /// Get a reference to the full VAPID key pair.
    pub fn vapid_keys(&self) -> &VapidKeys {
        &self.vapid_keys
    }

    /// Register a new push subscription.
    ///
    /// If a subscription with the same endpoint already exists, it is replaced.
    pub async fn subscribe(&self, subscription: PushSubscription) {
        let endpoint = subscription.endpoint.clone();
        let mut subs = self.subscriptions.write().await;
        subs.insert(endpoint, subscription);
    }

    /// Remove a push subscription by endpoint URL.
    ///
    /// Returns `true` if the subscription was found and removed.
    pub async fn unsubscribe(&self, endpoint: &str) -> bool {
        let mut subs = self.subscriptions.write().await;
        subs.remove(endpoint).is_some()
    }

    /// Get the number of active subscriptions.
    pub async fn subscription_count(&self) -> usize {
        let subs = self.subscriptions.read().await;
        subs.len()
    }

    /// Get all active subscriptions.
    pub async fn subscriptions(&self) -> Vec<PushSubscription> {
        let subs = self.subscriptions.read().await;
        subs.values().cloned().collect()
    }

    /// Check if a subscription exists for the given endpoint.
    pub async fn has_subscription(&self, endpoint: &str) -> bool {
        let subs = self.subscriptions.read().await;
        subs.contains_key(endpoint)
    }

    /// Send a push notification to a specific subscription.
    ///
    /// **Note:** Push message encryption (RFC 8291) and VAPID JWT signing
    /// (RFC 8292) are not yet implemented. This method will return
    /// `Err(PushError::DeliveryFailed)` until the crypto is added.
    /// The `web-push` crate can be integrated in a future iteration.
    pub async fn notify(
        &self,
        _subscription: &PushSubscription,
        _payload: &PushPayload,
    ) -> Result<(), PushError> {
        // TODO: Implement Web Push encryption (RFC 8291) and VAPID JWT signing (RFC 8292).
        //
        // Steps needed:
        // 1. Generate a content encryption key using ECDH with the subscription's p256dh key
        // 2. Encrypt the payload using AES-128-GCM with the derived key
        // 3. Create a VAPID JWT signed with our private key
        // 4. POST the encrypted payload to the subscription endpoint with
        //    Authorization: vapid t=<jwt>, k=<public_key>
        //    Content-Encoding: aes128gcm
        //    TTL: 86400
        //
        // For now, return an error indicating delivery is not yet supported.
        Err(PushError::DeliveryFailed(
            "push delivery not yet implemented (pending Web Push crypto)".to_string(),
        ))
    }

    /// Send a push notification to all active subscriptions.
    ///
    /// Returns a vec of `(endpoint, result)` for each subscription attempt.
    /// See `notify()` for current limitations.
    pub async fn notify_all(
        &self,
        payload: &PushPayload,
    ) -> Vec<(String, Result<(), PushError>)> {
        let subs = {
            let guard = self.subscriptions.read().await;
            guard.values().cloned().collect::<Vec<_>>()
        };

        let mut results = Vec::with_capacity(subs.len());
        for sub in &subs {
            let result = self.notify(sub, payload).await;
            results.push((sub.endpoint.clone(), result));
        }
        results
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PushApiHandler -- HTTP handler for push API endpoints
// ═══════════════════════════════════════════════════════════════════════════

/// HTTP handler for the Web Push API endpoints.
///
/// Serves:
/// - `GET  /api/push/vapid-key` -- returns the public VAPID key
/// - `POST /api/push/subscribe` -- registers a push subscription
/// - `POST /api/push/unsubscribe` -- removes a push subscription
///
/// Register this handler on the HTTP router at prefix `/api/push`.
pub struct PushApiHandler {
    push_manager: Arc<PushManager>,
    /// The route prefix (e.g., "/api/push").
    prefix: String,
}

impl PushApiHandler {
    /// Create a new push API handler.
    pub fn new(push_manager: Arc<PushManager>, prefix: &str) -> Self {
        let mut p = prefix.to_string();
        if !p.starts_with('/') {
            p = format!("/{p}");
        }
        if p.len() > 1 && p.ends_with('/') {
            p.pop();
        }
        Self {
            push_manager,
            prefix: p,
        }
    }
}

impl HttpHandler for PushApiHandler {
    fn handle(
        &self,
        req: Request<Incoming>,
        _peer: PeerInfo,
    ) -> Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send + '_>> {
        let method = req.method().clone();
        let path = req.uri().path().to_string();

        Box::pin(async move {
            // Strip prefix to get the sub-path
            let sub_path = path
                .strip_prefix(&self.prefix)
                .unwrap_or(&path);

            match (method, sub_path) {
                (Method::GET, "/vapid-key") => {
                    self.handle_vapid_key().await
                }
                (Method::POST, "/subscribe") => {
                    self.handle_subscribe(req).await
                }
                (Method::POST, "/unsubscribe") => {
                    self.handle_unsubscribe(req).await
                }
                _ => {
                    json_response(
                        StatusCode::NOT_FOUND,
                        &serde_json::json!({"error": "not found"}),
                    )
                }
            }
        })
    }
}

impl PushApiHandler {
    async fn handle_vapid_key(&self) -> Response<Full<Bytes>> {
        json_response(
            StatusCode::OK,
            &serde_json::json!({
                "vapid_key": self.push_manager.vapid_public_key(),
            }),
        )
    }

    async fn handle_subscribe(&self, req: Request<Incoming>) -> Response<Full<Bytes>> {
        // Read request body
        let body = match read_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };

        // Parse subscription request
        #[derive(Deserialize)]
        struct SubscribeRequest {
            endpoint: String,
            keys: PushKeys,
            device_id: Option<String>,
        }

        let parsed: SubscribeRequest = match serde_json::from_slice(&body) {
            Ok(p) => p,
            Err(e) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &serde_json::json!({"error": format!("invalid JSON: {e}")}),
                );
            }
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let subscription = PushSubscription {
            endpoint: parsed.endpoint.clone(),
            keys: parsed.keys,
            device_id: parsed.device_id,
            created_at: now,
        };

        self.push_manager.subscribe(subscription).await;

        json_response(
            StatusCode::OK,
            &serde_json::json!({
                "ok": true,
                "endpoint": parsed.endpoint,
            }),
        )
    }

    async fn handle_unsubscribe(&self, req: Request<Incoming>) -> Response<Full<Bytes>> {
        // Read request body
        let body = match read_body(req).await {
            Ok(b) => b,
            Err(resp) => return resp,
        };

        // Parse unsubscribe request
        #[derive(Deserialize)]
        struct UnsubscribeRequest {
            endpoint: String,
        }

        let parsed: UnsubscribeRequest = match serde_json::from_slice(&body) {
            Ok(p) => p,
            Err(e) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &serde_json::json!({"error": format!("invalid JSON: {e}")}),
                );
            }
        };

        let removed = self.push_manager.unsubscribe(&parsed.endpoint).await;

        if removed {
            json_response(
                StatusCode::OK,
                &serde_json::json!({"ok": true, "endpoint": parsed.endpoint}),
            )
        } else {
            json_response(
                StatusCode::NOT_FOUND,
                &serde_json::json!({"error": "subscription not found", "endpoint": parsed.endpoint}),
            )
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Base64url-encode bytes (no padding).
fn base64url_encode(data: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    URL_SAFE_NO_PAD.encode(data)
}

/// Build a JSON response with the given status code and body.
fn json_response(status: StatusCode, body: &serde_json::Value) -> Response<Full<Bytes>> {
    let json = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(json)))
        .unwrap_or_else(|_| {
            let mut resp = Response::new(Full::new(Bytes::from("{}")));
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            resp
        })
}

/// Read the full body from an incoming request.
async fn read_body(req: Request<Incoming>) -> Result<Bytes, Response<Full<Bytes>>> {
    use http_body_util::BodyExt;

    match req.into_body().collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e) => Err(json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"error": format!("failed to read body: {e}")}),
        )),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vapid_key_generation() {
        let keys = VapidKeys::generate().expect("VAPID key generation should succeed");

        // Public key should be base64url-encoded (65 bytes -> ~88 chars)
        assert!(!keys.public_key.is_empty());
        assert!(keys.public_key.len() > 40, "public key too short: {}", keys.public_key.len());

        // Private key should be base64url-encoded (32 bytes -> ~44 chars)
        assert!(!keys.private_key.is_empty());
        assert!(keys.private_key.len() > 20, "private key too short: {}", keys.private_key.len());

        // Keys should not contain standard base64 padding
        assert!(!keys.public_key.contains('='));
        assert!(!keys.private_key.contains('='));
    }

    #[test]
    fn test_vapid_key_generation_unique() {
        let keys1 = VapidKeys::generate().unwrap();
        let keys2 = VapidKeys::generate().unwrap();
        assert_ne!(keys1.public_key, keys2.public_key, "generated keys should be unique");
        assert_ne!(keys1.private_key, keys2.private_key);
    }

    #[test]
    fn test_vapid_keys_serialization() {
        let keys = VapidKeys::generate().unwrap();
        let json = serde_json::to_string(&keys).unwrap();
        let deserialized: VapidKeys = serde_json::from_str(&json).unwrap();
        assert_eq!(keys.public_key, deserialized.public_key);
        assert_eq!(keys.private_key, deserialized.private_key);
    }

    #[tokio::test]
    async fn test_vapid_keys_load_or_generate() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("vapid-keys.json");

        // First call: generates and saves
        let keys1 = VapidKeys::load_or_generate(&key_path).await.unwrap();
        assert!(key_path.exists());

        // Second call: loads existing
        let keys2 = VapidKeys::load_or_generate(&key_path).await.unwrap();
        assert_eq!(keys1.public_key, keys2.public_key);
        assert_eq!(keys1.private_key, keys2.private_key);
    }

    #[tokio::test]
    async fn test_push_manager_subscribe() {
        let keys = VapidKeys::generate().unwrap();
        let manager = PushManager::new(keys);

        assert_eq!(manager.subscription_count().await, 0);

        let sub = PushSubscription {
            endpoint: "https://push.example.com/sub1".to_string(),
            keys: PushKeys {
                p256dh: "test-p256dh".to_string(),
                auth: "test-auth".to_string(),
            },
            device_id: Some("dev-1".to_string()),
            created_at: 1710764400,
        };

        manager.subscribe(sub).await;
        assert_eq!(manager.subscription_count().await, 1);
        assert!(manager.has_subscription("https://push.example.com/sub1").await);
    }

    #[tokio::test]
    async fn test_push_manager_unsubscribe() {
        let keys = VapidKeys::generate().unwrap();
        let manager = PushManager::new(keys);

        let sub = PushSubscription {
            endpoint: "https://push.example.com/sub1".to_string(),
            keys: PushKeys {
                p256dh: "test-p256dh".to_string(),
                auth: "test-auth".to_string(),
            },
            device_id: None,
            created_at: 1710764400,
        };

        manager.subscribe(sub).await;
        assert_eq!(manager.subscription_count().await, 1);

        // Unsubscribe existing
        let removed = manager.unsubscribe("https://push.example.com/sub1").await;
        assert!(removed);
        assert_eq!(manager.subscription_count().await, 0);

        // Unsubscribe non-existent
        let removed = manager.unsubscribe("https://push.example.com/not-found").await;
        assert!(!removed);
    }

    #[tokio::test]
    async fn test_push_manager_replace_subscription() {
        let keys = VapidKeys::generate().unwrap();
        let manager = PushManager::new(keys);

        let sub1 = PushSubscription {
            endpoint: "https://push.example.com/sub1".to_string(),
            keys: PushKeys {
                p256dh: "key-v1".to_string(),
                auth: "auth-v1".to_string(),
            },
            device_id: None,
            created_at: 1000,
        };

        let sub2 = PushSubscription {
            endpoint: "https://push.example.com/sub1".to_string(), // same endpoint
            keys: PushKeys {
                p256dh: "key-v2".to_string(),
                auth: "auth-v2".to_string(),
            },
            device_id: None,
            created_at: 2000,
        };

        manager.subscribe(sub1).await;
        manager.subscribe(sub2).await;

        // Should still be 1 subscription (replaced, not duplicated)
        assert_eq!(manager.subscription_count().await, 1);

        let subs = manager.subscriptions().await;
        assert_eq!(subs[0].keys.p256dh, "key-v2");
        assert_eq!(subs[0].created_at, 2000);
    }

    #[tokio::test]
    async fn test_push_manager_multiple_subscriptions() {
        let keys = VapidKeys::generate().unwrap();
        let manager = PushManager::new(keys);

        for i in 0..5 {
            let sub = PushSubscription {
                endpoint: format!("https://push.example.com/sub{i}"),
                keys: PushKeys {
                    p256dh: format!("p256dh-{i}"),
                    auth: format!("auth-{i}"),
                },
                device_id: None,
                created_at: 1000 + i,
            };
            manager.subscribe(sub).await;
        }

        assert_eq!(manager.subscription_count().await, 5);

        let subs = manager.subscriptions().await;
        assert_eq!(subs.len(), 5);
    }

    #[tokio::test]
    async fn test_push_manager_vapid_key_accessor() {
        let keys = VapidKeys::generate().unwrap();
        let expected_public = keys.public_key.clone();
        let manager = PushManager::new(keys);

        assert_eq!(manager.vapid_public_key(), expected_public);
    }

    #[tokio::test]
    async fn test_push_manager_notify_returns_not_implemented() {
        let keys = VapidKeys::generate().unwrap();
        let manager = PushManager::new(keys);

        let sub = PushSubscription {
            endpoint: "https://push.example.com/sub1".to_string(),
            keys: PushKeys {
                p256dh: "test".to_string(),
                auth: "test".to_string(),
            },
            device_id: None,
            created_at: 0,
        };

        let payload = PushPayload {
            title: "Test".to_string(),
            body: "Hello".to_string(),
            icon: None,
            badge: None,
            data: None,
            tag: None,
            require_interaction: None,
        };

        let result = manager.notify(&sub, &payload).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            PushError::DeliveryFailed(msg) => {
                assert!(msg.contains("not yet implemented"));
            }
            other => panic!("expected DeliveryFailed, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_push_manager_notify_all() {
        let keys = VapidKeys::generate().unwrap();
        let manager = PushManager::new(keys);

        // Add 3 subscriptions
        for i in 0..3 {
            let sub = PushSubscription {
                endpoint: format!("https://push.example.com/sub{i}"),
                keys: PushKeys {
                    p256dh: "test".to_string(),
                    auth: "test".to_string(),
                },
                device_id: None,
                created_at: 0,
            };
            manager.subscribe(sub).await;
        }

        let payload = PushPayload {
            title: "Broadcast".to_string(),
            body: "Hello everyone".to_string(),
            icon: None,
            badge: None,
            data: None,
            tag: None,
            require_interaction: None,
        };

        let results = manager.notify_all(&payload).await;
        assert_eq!(results.len(), 3);
        // All should fail with DeliveryFailed (not implemented)
        for (_, result) in &results {
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_push_error_display() {
        let err = PushError::KeyStorage("test error".to_string());
        assert_eq!(err.to_string(), "VAPID key error: test error");

        let err = PushError::NotFound("endpoint".to_string());
        assert_eq!(err.to_string(), "subscription not found: endpoint");

        let err = PushError::DeliveryFailed("timeout".to_string());
        assert_eq!(err.to_string(), "push delivery failed: timeout");

        let err = PushError::InvalidPayload("bad json".to_string());
        assert_eq!(err.to_string(), "invalid payload: bad json");
    }

    #[test]
    fn test_push_subscription_serialization() {
        let sub = PushSubscription {
            endpoint: "https://push.example.com/sub1".to_string(),
            keys: PushKeys {
                p256dh: "BKNZ6...".to_string(),
                auth: "4vQK...".to_string(),
            },
            device_id: Some("dev-1".to_string()),
            created_at: 1710764400,
        };

        let json = serde_json::to_string(&sub).unwrap();
        let deserialized: PushSubscription = serde_json::from_str(&json).unwrap();
        assert_eq!(sub.endpoint, deserialized.endpoint);
        assert_eq!(sub.keys.p256dh, deserialized.keys.p256dh);
        assert_eq!(sub.keys.auth, deserialized.keys.auth);
        assert_eq!(sub.device_id, deserialized.device_id);
        assert_eq!(sub.created_at, deserialized.created_at);
    }

    #[test]
    fn test_push_payload_serialization_minimal() {
        let payload = PushPayload {
            title: "Hello".to_string(),
            body: "World".to_string(),
            icon: None,
            badge: None,
            data: None,
            tag: None,
            require_interaction: None,
        };

        let json = serde_json::to_string(&payload).unwrap();
        // Optional fields should be omitted (skip_serializing_if)
        assert!(!json.contains("icon"));
        assert!(!json.contains("badge"));
        assert!(!json.contains("data"));
    }

    #[test]
    fn test_push_payload_serialization_full() {
        let payload = PushPayload {
            title: "New Message".to_string(),
            body: "You have a new message".to_string(),
            icon: Some("/icons/msg.png".to_string()),
            badge: Some("/icons/badge.png".to_string()),
            data: Some(serde_json::json!({"type": "message", "id": 42})),
            tag: Some("messages".to_string()),
            require_interaction: Some(true),
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("icon"));
        assert!(json.contains("badge"));
        assert!(json.contains("messages"));
    }

    #[test]
    fn test_base64url_encode() {
        // Known test vector
        let data = b"Hello, World!";
        let encoded = base64url_encode(data);
        assert!(!encoded.contains('='), "should not have padding");
        assert!(!encoded.contains('+'), "should use URL-safe chars");
        assert!(!encoded.contains('/'), "should use URL-safe chars");
    }

    #[test]
    fn test_json_response() {
        let resp = json_response(
            StatusCode::OK,
            &serde_json::json!({"key": "value"}),
        );
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
