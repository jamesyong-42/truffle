# RFC 008: Truffle Vision -- Architecture & Public API

**Status**: Draft
**Created**: 2026-03-18
**Depends on**: RFC 003 (Rust Rewrite), RFC 005 (Refactor), RFC 007 (Comprehensive Refactor)

---

## 1. Project Vision & Scope

### What Truffle IS

Truffle is a **Rust library** that gives any application automatic multi-device networking over Tailscale. Drop truffle into your app, and every instance of that app -- on every machine, on every platform -- can:

- **Discover** each other automatically via Tailscale's MagicDNS
- **Connect** over encrypted WireGuard tunnels (zero firewall config)
- **Exchange messages** via a namespace-based pub/sub bus
- **Synchronize state** via CRDT-like device-scoped slices
- **Transfer files** with resume and integrity verification
- **Serve HTTP** (reverse proxy local services, host static sites, serve PWAs)
- **Push notifications** to connected browser clients

Truffle handles all the complexity of node lifecycle, authentication, reconnection, primary election, and message routing. The consumer gets a single `TruffleRuntime` handle with a clean, high-level API.

### What Truffle is NOT

- **Not a framework.** It does not impose application structure. It is a library you call.
- **Not a Tailscale replacement.** It builds ON TOP of tsnet. It does not reimplement VPN functionality.
- **Not a general-purpose RPC system.** Messages are JSON envelopes, not protobuf/gRPC. For high-throughput structured data, use file transfer.
- **Not a database.** Store sync provides eventual consistency of device-scoped slices, not a replicated database.
- **Not an end-user product.** Truffle has no UI. It is infrastructure that products build on.

### Positioning

Truffle is to multi-device networking what SQLite is to databases: an embeddable, zero-configuration library that just works. Your app links truffle, calls `start()`, and gets a mesh network. No servers to deploy, no ports to open, no certificates to manage.

---

## 2. Architecture Layers

```
┌─────────────────────────────────────────────────────────────┐
│  Layer 5: HTTP Services                                      │
│  Reverse proxy, static hosting, PWA, Web Push                │
├─────────────────────────────────────────────────────────────┤
│  Layer 4: Application Services                               │
│  Message bus, store sync, file transfer                      │
├─────────────────────────────────────────────────────────────┤
│  Layer 3: Mesh                                               │
│  STAR topology, device discovery, primary election           │
├─────────────────────────────────────────────────────────────┤
│  Layer 2: Transport                                          │
│  WebSocket connections, heartbeat, reconnection              │
├─────────────────────────────────────────────────────────────┤
│  Layer 1: Bridge                                             │
│  Binary-header TCP bridge between Go sidecar and Rust        │
├─────────────────────────────────────────────────────────────┤
│  Layer 0: Tailscale (tsnet)                                  │
│  Encrypted WireGuard tunnels, MagicDNS, auto-certs, ACLs    │
└─────────────────────────────────────────────────────────────┘
```

### Layer 0: Tailscale (tsnet)

**What it owns:** Encrypted tunnel establishment, device identity, DNS resolution, TLS certificates, WireGuard key management, DERP relay fallback, ACL enforcement.

**Public API (from truffle's perspective):** This is the Go sidecar (`sidecar-slim`). Truffle communicates with it via stdin/stdout JSON IPC and a local TCP bridge.

**Depends on:** Nothing. This is the foundation.

**Relationship to Tailscale:** Truffle uses `tsnet.Server` (embedded node), `ListenTLS`, `Listen`, `Dial`, and `LocalClient` for status/peers. The sidecar is intentionally thin -- it owns only what MUST be in Go.

**Current state:** Implemented. `sidecar-slim/main.go` handles `tsnet:start`, `tsnet:stop`, `tsnet:getPeers`, `bridge:dial`. Listens on `:443` (TLS) and `:9417` (plain TCP).

**Gaps (from API surface analysis):**
- No background state monitoring after initial startup
- No key expiry detection or re-auth flow
- No `NeedsMachineAuth` handling
- Uses expensive `Status()` for peer DNS resolution instead of `WhoIs()`
- No listener cleanup in `handleStop()`
- No health monitoring
- No ephemeral node support

### Layer 1: Bridge

**What it owns:** The binary-framed TCP channel between the Go sidecar and Rust. Session token authentication. Port-based routing of incoming connections. Dial request correlation.

**Public API:**

```rust
// BridgeManager -- accepts incoming TCP connections from the sidecar
pub struct BridgeManager { .. }
impl BridgeManager {
    pub async fn bind(session_token: [u8; 32]) -> Result<Self>;
    pub fn local_port(&self) -> u16;
    pub fn add_handler(&mut self, port: u16, direction: Direction, handler: Arc<dyn BridgeHandler>);
    pub async fn run(self, cancel: CancellationToken);
}

// BridgeConnection -- a framed TCP stream with metadata
pub struct BridgeConnection {
    pub stream: TcpStream,
    pub header: BridgeHeader,
}
```

**Depends on:** Layer 0 (Go sidecar connects to the bridge port).

**Relationship to Tailscale:** The bridge exists because tsnet is a Go library. All tsnet connections (TLS-terminated by Go) arrive as local TCP connections to the bridge, tagged with a binary header containing direction, port, remote address, and peer DNS name.

**Current state:** Fully implemented. `bridge/manager.rs`, `bridge/header.rs`, `bridge/protocol.rs`, `bridge/shim.rs`.

### Layer 2: Transport

**What it owns:** WebSocket connection lifecycle over bridged TCP streams. WebSocket handshake (server and client). Heartbeat (ping/pong). Reconnection with exponential backoff. Connection state tracking. Device-to-connection mapping.

**Public API:**

```rust
pub struct ConnectionManager { .. }
impl ConnectionManager {
    pub fn new(config: TransportConfig) -> (Self, broadcast::Receiver<TransportEvent>);
    pub fn subscribe(&self) -> broadcast::Receiver<TransportEvent>;
    pub async fn handle_incoming(&self, conn: BridgeConnection);
    pub async fn handle_outgoing(&self, conn: BridgeConnection);
    pub async fn send(&self, connection_id: &str, value: &serde_json::Value) -> Result<()>;
    pub async fn set_device_id(&self, connection_id: &str, device_id: String);
    pub async fn get_connections(&self) -> Vec<WSConnection>;
    pub async fn close_connection(&self, connection_id: &str);
    pub async fn close_all(&self);
}
```

**Depends on:** Layer 1 (receives `BridgeConnection`s from the bridge).

**Relationship to Tailscale:** Tailscale provides the encrypted tunnel and TLS termination. The transport layer sees only local TCP streams and performs WebSocket framing on top. The WireGuard encryption is transparent.

**Current state:** Fully implemented. `transport/connection.rs`, `transport/heartbeat.rs`, `transport/reconnect.rs`, `transport/websocket.rs`.

### Layer 3: Mesh

**What it owns:** STAR topology management. Device discovery via Tailscale peer lists. Device announcement protocol. Primary election (deterministic: longest-running wins). Device role assignment (primary/secondary). Message routing (secondaries route through primary). Device lifecycle (announce, goodbye, timeout).

**Public API:**

```rust
pub struct MeshNode { .. }
impl MeshNode {
    pub fn new(config: MeshNodeConfig, conn_mgr: Arc<ConnectionManager>) -> (Self, broadcast::Receiver<MeshNodeEvent>);
    pub fn subscribe_events(&self) -> broadcast::Receiver<MeshNodeEvent>;
    pub async fn start(&self);
    pub async fn stop(&self);
    pub async fn broadcast_envelope(&self, envelope: &MeshEnvelope);
    pub async fn send_envelope(&self, device_id: &str, envelope: &MeshEnvelope);
    pub async fn local_device(&self) -> BaseDevice;
    pub async fn devices(&self) -> Vec<BaseDevice>;
    pub async fn device_id(&self) -> String;
    pub fn is_primary(&self) -> bool;
}
```

**Depends on:** Layer 2 (sends/receives messages via `ConnectionManager`).

**Relationship to Tailscale:** Uses Tailscale's peer list for device discovery. Filters peers by hostname prefix to identify truffle nodes belonging to the same application. MagicDNS names are used for connection targets.

**Current state:** Fully implemented. `mesh/node.rs`, `mesh/device.rs`, `mesh/election.rs`, `mesh/routing.rs`, `mesh/handler.rs`, `mesh/message_bus.rs`.

### Layer 4: Application Services

**What it owns:** Higher-level features built on the mesh. Each service uses the message bus for coordination and may use direct connections for data transfer.

#### Message Bus

Namespace-based pub/sub messaging over the mesh. Any component can subscribe to a namespace and receive messages. Messages are broadcast via the STAR topology.

```rust
pub struct MeshMessageBus { .. }
impl MeshMessageBus {
    pub fn new(event_tx: mpsc::Sender<MessageBusEvent>) -> Self;
    pub async fn subscribe(&self, namespace: &str, handler: BusMessageHandler);
    pub async fn dispatch(&self, message: &BusMessage);
    pub async fn dispose(&self);
}
```

#### Store Sync

CRDT-like state synchronization via device-scoped slices. Each device owns its own slice of each store. Slices are broadcast and merged on the receiver side. Stores implement the `SyncableStore` trait.

```rust
pub trait SyncableStore: Send + Sync {
    fn store_id(&self) -> &str;
    fn get_local_slice(&self) -> Option<DeviceSlice>;
    fn apply_remote_slice(&self, slice: DeviceSlice);
    fn remove_remote_slice(&self, device_id: &str, reason: &str);
    fn clear_remote_slices(&self);
}

pub struct StoreSyncAdapter { .. }
impl StoreSyncAdapter {
    pub fn new(config: StoreSyncAdapterConfig, stores: Vec<Arc<dyn SyncableStore>>, outgoing_tx: mpsc::UnboundedSender<OutgoingSyncMessage>) -> Arc<Self>;
    pub async fn start(&self);
    pub async fn stop(&self);
    pub async fn handle_sync_message(&self, message: &SyncMessage);
    pub async fn handle_local_changed(&self, store_id: &str, slice: &DeviceSlice);
    pub async fn handle_device_offline(&self, device_id: &str);
    pub async fn handle_device_discovered(&self, device_id: &str);
}
```

#### File Transfer

Fast file transfer between any two nodes. Uses the message bus for signaling (offer/accept/reject/cancel) and direct HTTP PUT for data transfer with resume and SHA-256 integrity.

```rust
pub struct FileTransferAdapter { .. }
impl FileTransferAdapter {
    pub fn new(config: FileTransferAdapterConfig, manager: Arc<FileTransferManager>, bus_tx: .., event_tx: ..) -> Arc<Self>;
    pub async fn send_file(&self, target_device_id: &str, file_path: &str) -> String;
    pub async fn accept_transfer(&self, offer: &FileTransferOffer, save_path: Option<&str>);
    pub async fn reject_transfer(&self, offer: &FileTransferOffer, reason: &str);
    pub async fn cancel_transfer(&self, transfer_id: &str);
    pub async fn get_transfers(&self) -> Vec<AdapterTransferInfo>;
}
```

**Depends on:** Layer 3 (mesh node for message routing and device awareness).

**Current state:** All three services are fully implemented. `mesh/message_bus.rs`, `store_sync/adapter.rs`, `file_transfer/adapter.rs`, plus integration wiring in `integration.rs`.

### Layer 5: HTTP Services (NEW)

**What it owns:** HTTP request handling on the Tailscale network. Reverse proxying local services. Static file serving. PWA support. Web Push notifications.

See Section 3 for the full design.

**Depends on:** Layer 1 (receives HTTP connections from the bridge), Layer 3 (mesh for device discovery and DNS name resolution), Layer 0 (Tailscale TLS certificates).

**Current state:** Reverse proxy is partially implemented (`reverse_proxy/mod.rs`). Static hosting, PWA, and Web Push are not implemented.

---

## 3. HTTP Layer Design (Layer 5)

The HTTP layer is the biggest gap in the current architecture. It handles four capabilities:

1. **Reverse Proxy** -- Expose local services to the tailnet
2. **Static Hosting** -- Serve files over HTTPS on the tailnet
3. **PWA Support** -- Serve progressive web apps with service worker registration
4. **Web Push** -- Send push notifications to connected browser clients

### 3.1 How HTTP Works in Truffle's Architecture

All HTTP traffic arrives through the same pipeline:

```
Browser on another device
    |
    | HTTPS (port 443, Tailscale auto-cert)
    v
Go sidecar (ListenTLS ":443")
    |
    | Bridge TCP (binary header: port=443, direction=incoming, remote_addr, peer_dns)
    v
Rust BridgeManager
    |
    | Route by path prefix
    v
┌──────────────────────────────────────────────────────────┐
│ /ws           -> Transport layer (WebSocket upgrade)      │
│ /proxy/*      -> Reverse proxy handler                    │
│ /static/*     -> Static file handler                      │
│ /pwa/*        -> PWA handler (manifest, SW, push)         │
│ /api/push/*   -> Web Push API handler                     │
│ /*            -> Default handler (SPA fallback or 404)    │
└──────────────────────────────────────────────────────────┘
```

**Key insight:** Currently, port 443 connections are ALL upgraded to WebSocket (`handle_incoming` rejects anything that isn't `GET /ws`). The HTTP layer changes this: port 443 becomes a **general HTTP port** that serves WebSocket, reverse proxy, static files, and PWA content. The routing decision happens BEFORE the WebSocket upgrade.

### 3.2 HTTP Router

A new `HttpRouter` sits between the bridge and the specialized handlers. It inspects the first HTTP request on each connection and routes based on path prefix.

```rust
/// Routes incoming HTTP connections to the appropriate handler.
pub struct HttpRouter {
    /// WebSocket paths (default: ["/ws"])
    ws_paths: Vec<String>,
    /// Registered path-prefix handlers
    routes: Vec<(String, Arc<dyn HttpHandler>)>,
    /// Fallback handler (404 or SPA fallback)
    fallback: Option<Arc<dyn HttpHandler>>,
}

/// Trait for HTTP request handlers.
pub trait HttpHandler: Send + Sync {
    fn handle(
        &self,
        req: Request<Incoming>,
        peer_info: PeerInfo,
    ) -> Pin<Box<dyn Future<Output = Response<BoxBody>> + Send>>;
}

/// Information about the connecting peer (from bridge header + WhoIs).
pub struct PeerInfo {
    pub remote_addr: String,
    pub dns_name: String,
    pub tailscale_ip: Option<String>,
    pub user_profile: Option<String>,
}
```

### 3.3 Reverse Proxy

The reverse proxy lets an app expose any local port to the entire tailnet under a path prefix.

```rust
// App tells truffle: "proxy /api to localhost:3000"
runtime.http().proxy("/api", "localhost:3000").await?;

// Another device accesses it:
// https://my-app-desktop-abc123.tail1234.ts.net/api/users
```

**Design:**

```rust
pub struct ReverseProxyHandler {
    /// Map of path prefix -> target address
    targets: Arc<RwLock<HashMap<String, ProxyTarget>>>,
}

pub struct ProxyTarget {
    /// Target address (e.g., "127.0.0.1:3000")
    pub addr: String,
    /// Target scheme ("http" or "https")
    pub scheme: String,
    /// Whether to strip the prefix before forwarding
    pub strip_prefix: bool,
    /// Optional rewrite rules
    pub headers: HashMap<String, String>,
}

impl ReverseProxyHandler {
    /// Add a proxy route.
    pub async fn add_route(&self, prefix: &str, target: ProxyTarget) -> Result<()>;

    /// Remove a proxy route.
    pub async fn remove_route(&self, prefix: &str) -> Result<()>;

    /// List all active routes.
    pub async fn routes(&self) -> Vec<(String, ProxyTarget)>;
}
```

**How it works:**

1. Browser requests `https://my-app.tail.ts.net/api/users`
2. Tailscale handles TLS termination (auto-cert via `ListenTLS`)
3. Go sidecar accepts the TLS connection, bridges to Rust
4. `HttpRouter` matches `/api` prefix, routes to `ReverseProxyHandler`
5. Handler connects to `127.0.0.1:3000`, forwards the request with path `/users` (prefix stripped)
6. Response is relayed back through the bridge to the browser

**WebSocket proxying:** If the request includes `Upgrade: websocket`, the handler performs a WebSocket upgrade on both sides and does bidirectional byte copying. This is already implemented in the current `reverse_proxy/mod.rs`.

**Difference from current implementation:** The current `ProxyManager` uses separate tsnet ports (e.g., 8443) for each proxy. The new design routes by path prefix on port 443, which is simpler (no port management) and uses Tailscale's auto-cert.

### 3.4 Static Site Hosting

Serve static files from a directory or in-memory file map over HTTPS on the node's Tailscale address.

```rust
// Serve files from disk
runtime.http().serve_static("/", "./public").await?;

// Serve files from memory (for embedded assets)
runtime.http().serve_static_memory("/assets", vec![
    StaticFile { path: "app.js", content: include_bytes!("../dist/app.js"), mime: "application/javascript" },
    StaticFile { path: "style.css", content: include_bytes!("../dist/style.css"), mime: "text/css" },
]).await?;
```

**Design:**

```rust
pub struct StaticHandler {
    /// Root directory for file serving
    root: PathBuf,
    /// In-memory file map (path -> (content, mime_type))
    memory_files: Arc<RwLock<HashMap<String, (Bytes, String)>>>,
    /// SPA mode: serve index.html for unmatched paths
    spa_fallback: bool,
    /// Cache-Control header value
    cache_control: String,
    /// Index file name (default: "index.html")
    index: String,
}

impl StaticHandler {
    pub fn from_dir(root: impl Into<PathBuf>) -> Self;
    pub fn from_memory(files: Vec<StaticFile>) -> Self;
    pub fn with_spa_fallback(self, enabled: bool) -> Self;
    pub fn with_cache_control(self, value: &str) -> Self;
}
```

**SPA fallback:** When `spa_fallback` is true, any request that doesn't match a file returns `index.html` instead of 404. This is essential for React/Vue/Svelte single-page apps.

**MIME types:** Auto-detected from file extension. Defaults to `application/octet-stream`.

**Caching:** Static files are served with `Cache-Control: public, max-age=3600` by default. Files with content hashes in their names (e.g., `app.a1b2c3.js`) get `max-age=31536000, immutable`.

### 3.5 PWA Support

Progressive Web App support allows browser clients to connect to truffle nodes and receive push notifications.

```rust
// Enable PWA with a manifest
runtime.http().enable_pwa(PwaConfig {
    manifest_path: "./pwa/manifest.json",
    service_worker_path: "./pwa/sw.js",
    icon_dir: "./pwa/icons",
    vapid_key_path: "~/.my-app/vapid-keys.json",
}).await?;
```

**Design:**

```rust
pub struct PwaConfig {
    /// Path to web app manifest (manifest.json)
    pub manifest_path: PathBuf,
    /// Path to service worker script
    pub service_worker_path: PathBuf,
    /// Directory containing PWA icons
    pub icon_dir: PathBuf,
    /// Path to store/load VAPID key pair (auto-generated if missing)
    pub vapid_key_path: PathBuf,
    /// Scope for the service worker (default: "/")
    pub scope: String,
}

pub struct PwaHandler {
    config: PwaConfig,
    vapid_keys: VapidKeyPair,
}
```

**How a browser discovers and connects to the PWA:**

1. User navigates to `https://my-app.tail.ts.net/pwa/` from any device on the tailnet
2. The PWA handler serves `index.html` with the manifest link
3. Browser registers the service worker from `/pwa/sw.js`
4. Service worker establishes a WebSocket connection to `/ws` for real-time updates
5. On initial connection, the mesh node detects no `device:announce` within 3 seconds and registers the connection as a PWA client (existing behavior from TypeScript reference)
6. PWA subscribes for push notifications via the Push API

**Primary discovery:** Since PWA browsers connect to a specific node's DNS name, they need to find the primary. The existing `primary:redirect` mechanism handles this: when a PWA connects to a secondary, the secondary sends a redirect message with the primary's DNS name, and the PWA reconnects.

### 3.6 Web Push API

Push notifications to connected browser clients using the Web Push protocol (RFC 8030 + RFC 8291 + RFC 8292).

```rust
// Send a push notification to all subscribed browsers
runtime.push().notify_all("New message", PushPayload {
    title: "My App",
    body: "You have a new message from Device B",
    icon: "/pwa/icons/icon-192.png",
    data: json!({"type": "new_message", "from": "device-b"}),
}).await?;

// Send to a specific subscription
runtime.push().notify(&subscription, payload).await?;
```

**Design:**

```rust
pub struct PushManager {
    /// VAPID key pair for signing push messages
    vapid_keys: VapidKeyPair,
    /// Active push subscriptions (indexed by endpoint URL)
    subscriptions: Arc<RwLock<HashMap<String, PushSubscription>>>,
    /// HTTP client for sending push messages
    client: reqwest::Client,
}

pub struct PushSubscription {
    pub endpoint: String,
    pub keys: PushSubscriptionKeys,
    pub device_id: Option<String>,
    pub subscribed_at: u64,
}

pub struct PushSubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

pub struct PushPayload {
    pub title: String,
    pub body: String,
    pub icon: Option<String>,
    pub badge: Option<String>,
    pub data: Option<serde_json::Value>,
    pub tag: Option<String>,
    pub require_interaction: Option<bool>,
}

impl PushManager {
    /// Register a new push subscription (called from PWA via API).
    pub async fn register(&self, subscription: PushSubscription) -> Result<()>;

    /// Unregister a push subscription.
    pub async fn unregister(&self, endpoint: &str) -> Result<()>;

    /// Send a push notification to a specific subscription.
    pub async fn notify(&self, subscription: &PushSubscription, payload: &PushPayload) -> Result<()>;

    /// Send a push notification to all subscriptions.
    pub async fn notify_all(&self, payload: &PushPayload) -> Result<()>;

    /// Get the public VAPID key (for the browser PushManager.subscribe() call).
    pub fn vapid_public_key(&self) -> &str;
}
```

**API Endpoints (served by the HTTP router):**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/api/push/vapid-key` | GET | Return the public VAPID key for subscription |
| `/api/push/subscribe` | POST | Register a push subscription |
| `/api/push/unsubscribe` | POST | Unregister a push subscription |

**VAPID key management:** On first run, truffle generates an ECDSA P-256 key pair and stores it at the configured path. The public key is served to browsers so they can subscribe. The private key is used to sign push messages.

### 3.7 HTTP Layer: Sidecar Changes Required

The current sidecar needs one change for the HTTP layer: port 443 connections must be routed based on HTTP path, not always assumed to be WebSocket.

**Option A (preferred): No sidecar changes.** The sidecar already bridges ALL port 443 connections to Rust. Rust currently rejects non-`/ws` requests. Instead, Rust accepts all HTTP requests on port 443 and routes them. The bridge protocol is unchanged.

**Option B: Add HTTP handler ports.** Add new `Listen` ports in the sidecar for dedicated HTTP traffic (e.g., `:8080`). This adds complexity without clear benefit since TLS auto-certs work on port 443.

Recommendation: **Option A.** The only Rust-side change is replacing the hard-coded `GET /ws` check in `ConnectionManager::handle_incoming` with the `HttpRouter` that dispatches to WebSocket, proxy, static, or PWA handlers.

---

## 4. Tailscale Integration Improvements

Based on the gap analysis in `tailscale-api-surface.md`, these improvements are prioritized by impact.

### 4.1 Critical: Background State Monitoring

**Problem:** After reaching `Running` state, the sidecar never checks `BackendState` again. Key expiry causes silent connectivity loss.

**Solution:** Add a background goroutine in the sidecar that periodically polls `StatusWithoutPeers()` (every 60 seconds) or uses `WatchIPNBus()` for push-based notifications.

```go
// New goroutine in sidecar
func (s *shim) monitorState(lc *tailscale.LocalClient) {
    ticker := time.NewTicker(60 * time.Second)
    defer ticker.Stop()

    lastState := "Running"
    for {
        select {
        case <-s.ctx.Done():
            return
        case <-ticker.C:
            status, err := lc.StatusWithoutPeers(s.ctx)
            if err != nil { continue }

            if status.BackendState != lastState {
                s.sendEvent("tsnet:stateChange", stateChangeData{
                    PreviousState: lastState,
                    NewState:      status.BackendState,
                    AuthURL:       status.AuthURL,
                })
                lastState = status.BackendState

                if status.BackendState == "NeedsLogin" && status.AuthURL != "" {
                    s.sendEvent("tsnet:authRequired", authRequiredData{AuthURL: status.AuthURL})
                }
                if status.BackendState == "NeedsMachineAuth" {
                    s.sendEvent("tsnet:needsApproval", nil)
                }
            }

            // Check key expiry
            if status.Self != nil && !status.Self.KeyExpiry.IsZero() {
                expiresIn := time.Until(status.Self.KeyExpiry)
                if expiresIn < 7*24*time.Hour {
                    s.sendEvent("tsnet:keyExpiring", keyExpiryData{
                        ExpiresAt: status.Self.KeyExpiry.Unix(),
                        ExpiresIn: int64(expiresIn.Seconds()),
                    })
                }
            }

            // Health warnings
            if len(status.Health) > 0 {
                s.sendEvent("tsnet:healthWarning", healthData{Warnings: status.Health})
            }
        }
    }
}
```

**New Rust events:**

```rust
pub enum ShimLifecycleEvent {
    // ... existing variants ...

    /// Backend state changed (e.g., Running -> NeedsLogin).
    StateChanged {
        previous_state: String,
        new_state: String,
        auth_url: Option<String>,
    },

    /// Node needs admin approval (NeedsMachineAuth).
    NeedsApproval,

    /// Node key is expiring soon.
    KeyExpiring {
        expires_at: u64,
        expires_in_seconds: i64,
    },

    /// Health warnings from Tailscale.
    HealthWarning {
        warnings: Vec<String>,
    },
}
```

### 4.2 Critical: Replace resolvePeerDNS with WhoIs

**Problem:** `resolvePeerDNS()` calls `lc.Status(ctx)` with full peer enumeration on every incoming connection. This is O(n) in peer count per connection.

**Solution:** Replace with `lc.WhoIs(ctx, remoteAddr)`, a purpose-built O(1) lookup.

```go
func (s *shim) resolvePeerDNS(lc *tailscale.LocalClient, remoteAddr string) string {
    whois, err := lc.WhoIs(s.ctx, remoteAddr)
    if err != nil {
        log.Printf("WhoIs failed for %s: %v", remoteAddr, err)
        return ""
    }
    return strings.TrimSuffix(whois.Node.Name, ".")
}
```

**Bonus:** `WhoIs` also returns `UserProfile` and `Node` info, which could be included in the bridge header for Rust-side authorization in the future.

### 4.3 Critical: Graceful Shutdown with Listener Cleanup

**Problem:** `handleStop()` calls `server.Close()` without first closing listeners. In-progress connections may hang.

**Solution:** Track listeners and close them explicitly before `server.Close()`.

```go
type shim struct {
    // ... existing fields ...
    listeners []net.Listener // Track all active listeners
    listenerMu sync.Mutex
}

func (s *shim) trackListener(ln net.Listener) {
    s.listenerMu.Lock()
    s.listeners = append(s.listeners, ln)
    s.listenerMu.Unlock()
}

func (s *shim) handleStop() {
    if s.cancel != nil {
        s.cancel()
    }
    // Close all listeners first
    s.listenerMu.Lock()
    for _, ln := range s.listeners {
        ln.Close()
    }
    s.listeners = nil
    s.listenerMu.Unlock()

    if s.server != nil {
        s.server.Close()
        s.server = nil
    }
    s.sendEvent("tsnet:stopped", nil)
}
```

### 4.4 Should Have: Rich Peer Info

**Problem:** Peer reports include only hostname, DNS name, IPs, online, and OS. Missing: connection quality (direct vs relayed), key expiry, DERP region, last seen.

**Solution:** Extend `peerInfo` struct with additional fields already available from `PeerStatus`.

```go
type peerInfo struct {
    // ... existing fields ...
    CurAddr    string `json:"curAddr,omitempty"`    // Empty = relayed
    Relay      string `json:"relay,omitempty"`      // DERP region name
    LastSeen   int64  `json:"lastSeen,omitempty"`   // Unix timestamp
    KeyExpiry  int64  `json:"keyExpiry,omitempty"`  // Unix timestamp
    Expired    bool   `json:"expired,omitempty"`
}
```

### 4.5 Should Have: Ephemeral Node Support

**Problem:** All truffle nodes are persistent, accumulating in the Tailscale admin console.

**Solution:** Add `ephemeral` field to the start command. Important for testing, CI, and development.

```go
type startData struct {
    // ... existing fields ...
    Ephemeral bool `json:"ephemeral,omitempty"`
}

// In handleStart():
if d.Ephemeral {
    s.server.Ephemeral = true
}
```

### 4.6 Future: WatchIPNBus for Push-Based Monitoring

Replace the polling loop in `monitorState()` with `WatchIPNBus()` for real-time state change notifications. This eliminates the 60-second polling window and gives instant detection of key expiry, peer changes, and network events.

This is a Priority 3 improvement -- the polling approach works correctly, `WatchIPNBus` is an optimization.

---

## 5. Module Structure

### 5.1 Proposed Module Layout

```
truffle-core/src/
    lib.rs                    -- Module declarations, re-exports

    tailscale/                -- NEW: Tailscale node lifecycle
        mod.rs                -- TailscaleNode config, state machine
        auth.rs               -- Auth flow management (key expiry, re-auth)
        health.rs             -- Health monitoring, state change events

    bridge/                   -- Go sidecar bridge protocol
        mod.rs
        header.rs             -- Binary frame header codec
        manager.rs            -- BridgeManager (TCP accept loop)
        protocol.rs           -- IPC command/event types
        shim.rs               -- GoShim process management

    transport/                -- WebSocket connections
        mod.rs
        connection.rs         -- ConnectionManager
        heartbeat.rs          -- Ping/pong lifecycle
        reconnect.rs          -- Exponential backoff reconnection
        websocket.rs          -- WS read/write pumps, codec

    mesh/                     -- STAR topology mesh
        mod.rs
        node.rs               -- MeshNode (orchestrator)
        device.rs             -- DeviceManager, DeviceIdentity
        election.rs           -- PrimaryElection (deterministic)
        handler.rs            -- Mesh message handler
        message_bus.rs        -- MeshMessageBus (pub/sub)
        routing.rs            -- STAR routing logic

    services/                 -- NEW: Reorganized application services
        mod.rs
        store_sync/           -- CRDT state synchronization
            mod.rs
            adapter.rs
            types.rs
        file_transfer/        -- Resumable file transfer
            mod.rs
            adapter.rs
            manager.rs
            sender.rs
            receiver.rs
            hasher.rs
            progress.rs
            resume.rs
            types.rs

    http/                     -- NEW: HTTP services layer
        mod.rs
        router.rs             -- HttpRouter (path-based dispatch)
        proxy.rs              -- ReverseProxyHandler
        static_site.rs        -- StaticHandler (file serving)
        pwa.rs                -- PwaHandler (manifest, SW, icons)
        push.rs               -- PushManager (Web Push API)

    protocol/                 -- Wire protocol types
        mod.rs
        envelope.rs           -- MeshEnvelope
        message_types.rs      -- Device announce/list/goodbye payloads
        hostname.rs           -- Hostname parsing
        codec.rs              -- Binary codec helpers

    types/                    -- Shared types
        mod.rs                -- BaseDevice, TailnetPeer, AuthStatus, etc.

    runtime.rs                -- TruffleRuntime (top-level orchestrator)
    integration.rs            -- Wire helpers (connect services to mesh)
    util.rs                   -- Timestamp helpers, etc.
```

### 5.2 Changes from Current Structure

| Change | Reason |
|--------|--------|
| New `tailscale/` module | Separates Tailscale lifecycle concerns from the bridge protocol. Auth flow, health monitoring, and state machine live here. |
| New `services/` parent module | Groups store_sync and file_transfer under a common parent. Makes the layer 4 boundary explicit. |
| New `http/` module | Layer 5 implementation. Reverse proxy moves from `reverse_proxy/` to `http/proxy.rs`. |
| `reverse_proxy/` removed | Consolidated into `http/proxy.rs`. The current implementation moves with minimal changes. |

### 5.3 Migration Notes

- `store_sync/` and `file_transfer/` become `services/store_sync/` and `services/file_transfer/`. This is a path change only; the internal code is unchanged.
- `reverse_proxy/mod.rs` moves to `http/proxy.rs`. The `ProxyManager` becomes `ReverseProxyHandler` with the same core logic but adapted for path-based routing.
- The `tailscale/` module is new code, not a refactor. It wraps state management that currently lives inline in `runtime.rs`.

---

## 6. Public API Design

The consumer-facing API should be simple and discoverable. Here is the target `TruffleRuntime` API.

### 6.1 Builder Pattern

```rust
use truffle_core::TruffleRuntime;

let runtime = TruffleRuntime::builder()
    // Required
    .hostname("my-app")
    .state_dir("~/.my-app/tailscale")
    .sidecar_path("/usr/local/bin/truffle-sidecar")

    // Optional
    .auth_key("tskey-auth-xxx")
    .ephemeral(false)
    .prefer_primary(true)
    .device_name("My Laptop")
    .device_type("desktop")
    .capabilities(vec!["pty", "file-transfer"])

    // Timing (all optional with sane defaults)
    .heartbeat_interval(Duration::from_secs(2))
    .heartbeat_timeout(Duration::from_secs(5))
    .announce_interval(Duration::from_secs(30))

    .build()?;
```

### 6.2 Lifecycle

```rust
// Start the runtime (spawns sidecar, starts mesh)
let mut events = runtime.start().await?;

// Event loop
tokio::spawn(async move {
    while let Some(event) = events.recv().await {
        match event {
            TruffleEvent::AuthRequired { auth_url } => {
                println!("Open: {auth_url}");
            }
            TruffleEvent::Online { ip, dns_name } => {
                println!("Online at {dns_name}");
            }
            TruffleEvent::DeviceDiscovered(device) => {
                println!("Found: {} ({})", device.name, device.id);
            }
            TruffleEvent::DeviceOffline(id) => {
                println!("Lost: {id}");
            }
            TruffleEvent::RoleChanged { role, is_primary } => {
                println!("Role: {role:?} (primary={is_primary})");
            }
            TruffleEvent::Message(msg) => {
                println!("[{}] {}: {:?}", msg.namespace, msg.msg_type, msg.payload);
            }
            TruffleEvent::Error(e) => {
                eprintln!("Error: {e}");
            }
            _ => {}
        }
    }
});

// Graceful shutdown
runtime.stop().await;
```

### 6.3 Unified Event Type

```rust
/// Top-level events from the TruffleRuntime.
///
/// Consumers subscribe to a single event stream instead of
/// wiring together multiple broadcast channels.
#[derive(Debug, Clone)]
pub enum TruffleEvent {
    // Lifecycle
    Started,
    Stopped,
    AuthRequired { auth_url: String },
    NeedsApproval,
    Online { ip: String, dns_name: String },
    KeyExpiring { expires_at: u64, expires_in_seconds: i64 },
    HealthWarning { warnings: Vec<String> },

    // Mesh
    DeviceDiscovered(BaseDevice),
    DeviceUpdated(BaseDevice),
    DeviceOffline(String),
    RoleChanged { role: DeviceRole, is_primary: bool },
    PrimaryChanged { primary_id: Option<String> },

    // Message bus
    Message(BusMessage),

    // File transfer
    FileTransferOffer(FileTransferOffer),
    FileTransferProgress(FileTransferEvent),
    FileTransferComplete(FileTransferEvent),
    FileTransferFailed { transfer_id: String, error: String },

    // Errors
    Error(String),
    SidecarCrashed { exit_code: Option<i32>, stderr_tail: String },
}
```

### 6.4 Message Bus

```rust
// Subscribe to a namespace
runtime.bus().subscribe("chat", Arc::new(|msg: &BusMessage| {
    let text = msg.payload["text"].as_str().unwrap_or("");
    println!("{}: {}", msg.from.as_deref().unwrap_or("unknown"), text);
})).await;

// Broadcast to all devices
runtime.bus().broadcast("chat", "message", json!({
    "text": "Hello from this device!",
    "timestamp": 1710764400,
})).await;

// Send to a specific device
runtime.bus().send_to("device-abc", "chat", "message", json!({
    "text": "Private message",
})).await;
```

### 6.5 Store Sync

```rust
use truffle_core::services::store_sync::{SyncableStore, DeviceSlice};

/// Example: a task list that syncs across devices.
struct TaskStore {
    tasks: std::sync::Mutex<Vec<Task>>,
    device_id: String,
    version: std::sync::atomic::AtomicU64,
}

impl SyncableStore for TaskStore {
    fn store_id(&self) -> &str { "tasks" }

    fn get_local_slice(&self) -> Option<DeviceSlice> {
        let tasks = self.tasks.lock().unwrap();
        Some(DeviceSlice {
            device_id: self.device_id.clone(),
            data: serde_json::to_value(&*tasks).unwrap(),
            version: self.version.load(Ordering::SeqCst),
            updated_at: current_timestamp_ms(),
        })
    }

    fn apply_remote_slice(&self, slice: DeviceSlice) {
        // Merge remote tasks (app-specific merge logic)
    }

    fn remove_remote_slice(&self, device_id: &str, _reason: &str) {
        // Remove tasks from this device
    }

    fn clear_remote_slices(&self) {
        // Clear all remote tasks
    }
}

// Register and start sync
let task_store = Arc::new(TaskStore::new(runtime.device_id()));
runtime.store_sync().register(task_store.clone()).await;
runtime.store_sync().start().await;

// When local data changes
task_store.add_task("Buy groceries");
runtime.store_sync().notify_changed("tasks").await;
```

### 6.6 File Transfer

```rust
// Send a file
let transfer_id = runtime.file_transfer()
    .send("device-xyz", "/path/to/large-file.zip")
    .await?;

// Events arrive via TruffleEvent::FileTransfer*

// Accept an incoming offer (in event handler)
if let TruffleEvent::FileTransferOffer(offer) = event {
    runtime.file_transfer()
        .accept(&offer, Some("/downloads/received-file.zip"))
        .await;
}

// Cancel a transfer
runtime.file_transfer().cancel(&transfer_id).await;
```

### 6.7 HTTP Services

```rust
// Reverse proxy: expose local dev server
runtime.http().proxy("/", "localhost:5173").await?;

// Reverse proxy: expose API server under a prefix
runtime.http().proxy("/api", "localhost:3000").await?;

// Static file hosting
runtime.http().serve_static("/docs", "./public/docs").await?;

// SPA hosting (with fallback to index.html)
runtime.http().serve_spa("/app", "./dist").await?;

// PWA support
runtime.http().enable_pwa(PwaConfig {
    manifest_path: "./pwa/manifest.json".into(),
    service_worker_path: "./pwa/sw.js".into(),
    icon_dir: "./pwa/icons".into(),
    vapid_key_path: "~/.my-app/vapid-keys.json".into(),
    scope: "/".to_string(),
}).await?;

// Remove a route
runtime.http().remove_route("/api").await?;

// List active routes
let routes = runtime.http().routes().await;
```

### 6.8 Web Push

```rust
// Get the public VAPID key (pass to browser)
let vapid_key = runtime.push().vapid_public_key();

// Register a subscription (from browser via API)
runtime.push().register(subscription).await?;

// Send notification to all subscribers
runtime.push().notify_all(&PushPayload {
    title: "New File".to_string(),
    body: "device-abc shared report.pdf".to_string(),
    icon: Some("/pwa/icons/icon-192.png".to_string()),
    data: Some(json!({"type": "file_shared", "name": "report.pdf"})),
    ..Default::default()
}).await?;
```

### 6.9 Device & Mesh Info

```rust
// Get local device info
let local = runtime.local_device().await;
println!("I am {} ({})", local.name, local.id);

// Get all devices in the mesh
let devices = runtime.devices().await;
for device in &devices {
    println!("  {} - {:?} - {:?}", device.name, device.role, device.status);
}

// Check role
if runtime.is_primary() {
    println!("I am the primary node");
}

// Get DNS name (for constructing URLs)
if let Some(dns) = runtime.dns_name() {
    println!("Access me at https://{dns}");
}
```

---

## 7. Crate Structure

### 7.1 Current Crates

```
truffle/
    crates/
        truffle-core/         -- Rust library (13k LOC)
        truffle-napi/         -- Node.js NAPI bindings
        truffle-tauri-plugin/ -- Tauri v2 plugin
    packages/
        sidecar-slim/         -- Go sidecar binary
        core/                 -- TypeScript wrapper
        react/                -- React hooks
        cli/                  -- CLI tool (TypeScript)
```

### 7.2 Target Crate Structure

```
truffle/
    crates/
        truffle-core/         -- Pure Rust library (no runtime assumptions)
                                 Everything in Sections 2-6 lives here.
                                 Depends on: tokio, serde, hyper, tungstenite, etc.
                                 Does NOT depend on: napi, tauri.

        truffle-napi/         -- Node.js NAPI bindings
                                 Thin wrapper: TruffleRuntime -> napi::JsObject
                                 Converts Rust events -> JS callbacks
                                 Ships as npm package with prebuilt binaries

        truffle-tauri-plugin/ -- Tauri v2 plugin
                                 Implements tauri::plugin::Plugin
                                 Exposes truffle via Tauri's IPC system
                                 Commands: start, stop, send_message, etc.

        truffle-cli/          -- NEW: Native Rust CLI tool
                                 For testing and debugging truffle networks
                                 Replaces TypeScript CLI (packages/cli)
                                 Commands: status, peers, send, proxy, etc.

    packages/
        sidecar-slim/         -- Go sidecar binary
                                 Only Go code in the project.
                                 Built with CGO_ENABLED=0 for static linking.

        core/                 -- TypeScript wrapper (npm: @truffle/core)
                                 Wraps truffle-napi with TypeScript types
                                 Re-exports for Node.js/Electron consumers

        react/                -- React hooks (npm: @truffle/react)
                                 useTruffle(), useDevices(), useFileTransfer(), etc.
```

### 7.3 Dependency Graph

```
truffle-cli ──────┐
truffle-napi ─────┤
truffle-tauri ────┼──> truffle-core ──> sidecar-slim (runtime, not compile-time)
                  │
react ──> core ───┘ (via truffle-napi)
```

### 7.4 truffle-core Design Principles

1. **Pure library.** No `fn main()`, no global state, no singletons. Multiple `TruffleRuntime` instances can coexist in the same process (each with its own sidecar, hostname, and state directory).

2. **Async-first.** All I/O operations are async using tokio. The runtime does not spawn its own tokio runtime -- the consumer provides one.

3. **No panic.** All fallible operations return `Result`. No `.unwrap()` on user-controlled inputs. Panics are bugs.

4. **Minimal unsafe.** Zero `unsafe` code in truffle-core. NAPI bindings use unsafe only where required by the FFI boundary.

5. **Feature flags for optional deps.** Heavy dependencies (like `reqwest` for Web Push) are behind feature flags:
   ```toml
   [features]
   default = ["mesh", "store-sync", "file-transfer"]
   mesh = []
   store-sync = ["mesh"]
   file-transfer = ["mesh"]
   http = ["hyper", "http-body-util"]
   push = ["http", "reqwest", "p256", "aes-gcm"]
   ```

---

## 8. Migration Path

### 8.1 Phase 1: Module Reorganization (Non-Breaking)

**Effort:** 2-3 hours
**Breaking changes:** None (re-exports maintain backward compatibility)

1. Create `services/` module, move `store_sync/` and `file_transfer/` under it
2. Create `tailscale/` module, extract auth/health concerns from `runtime.rs`
3. Create `http/` module, move `reverse_proxy/` into `http/proxy.rs`
4. Add re-exports in `lib.rs` so existing imports continue to work:
   ```rust
   // Backward compatibility
   pub use services::store_sync;
   pub use services::file_transfer;
   pub use http::proxy as reverse_proxy;
   ```

### 8.2 Phase 2: Sidecar Improvements (Non-Breaking)

**Effort:** 4-6 hours
**Breaking changes:** None (additive only)

1. Replace `resolvePeerDNS` with `WhoIs()` in the sidecar
2. Add background state monitoring goroutine
3. Add `NeedsMachineAuth` handling
4. Fix listener cleanup in `handleStop()`
5. Add rich peer info fields (curAddr, relay, keyExpiry, expired)
6. Add ephemeral node support to start command
7. Add corresponding Rust event variants (additive to `ShimLifecycleEvent`)

### 8.3 Phase 3: Unified Event API (Breaking for NAPI)

**Effort:** 4-6 hours
**Breaking changes:** NAPI bindings need updating

1. Create `TruffleEvent` enum (Section 6.3)
2. Add `TruffleRuntime::start() -> broadcast::Receiver<TruffleEvent>` that merges all internal event streams into one
3. Implement builder pattern for `TruffleRuntime` (Section 6.1)
4. Add convenience accessors: `runtime.bus()`, `runtime.store_sync()`, `runtime.file_transfer()`, `runtime.http()`, `runtime.push()`
5. Update NAPI bindings to use the new API

### 8.4 Phase 4: HTTP Router (Breaking for Reverse Proxy)

**Effort:** 6-8 hours
**Breaking changes:** Reverse proxy API changes (path-based instead of port-based)

1. Implement `HttpRouter` (Section 3.2)
2. Change `ConnectionManager::handle_incoming` to route through `HttpRouter` instead of hard-coding `GET /ws`
3. Migrate `ProxyManager` to `ReverseProxyHandler` (path-based routing)
4. Implement `StaticHandler` for static file serving

### 8.5 Phase 5: PWA & Web Push (Additive)

**Effort:** 8-12 hours
**Breaking changes:** None (entirely new functionality)

1. Implement `PwaHandler` (manifest, service worker, icons)
2. Implement `PushManager` (VAPID keys, subscription management, push delivery)
3. Add API endpoints for push subscription
4. Add integration tests with a headless browser

### 8.6 Phase 6: CLI Tool (New Crate)

**Effort:** 4-6 hours
**Breaking changes:** None (new crate)

1. Create `truffle-cli` crate
2. Implement commands: `status`, `peers`, `send`, `proxy`, `serve`
3. Replace TypeScript CLI (`packages/cli`)

### 8.7 Summary

| Phase | Effort | Breaking | Can Ship Independently |
|-------|--------|----------|----------------------|
| 1. Module reorg | 2-3h | No | Yes |
| 2. Sidecar improvements | 4-6h | No | Yes |
| 3. Unified event API | 4-6h | NAPI | Yes (with NAPI update) |
| 4. HTTP router | 6-8h | Reverse proxy | Yes |
| 5. PWA & Push | 8-12h | No | Yes |
| 6. CLI tool | 4-6h | No | Yes |

Total: ~30-40 hours of implementation across 6 independent phases.

---

## 9. Non-Goals

The following are explicitly out of scope for truffle. They will not be implemented unless requirements fundamentally change.

### 9.1 No Custom VPN/Tunnel

Truffle does not implement its own tunneling. It relies entirely on Tailscale's WireGuard tunnels. This means:
- Truffle requires a Tailscale account
- Devices must be on the same tailnet
- Tailscale's free tier limits (100 devices, 3 users) apply

Truffle will NOT implement Headscale integration, custom relay servers, or alternative tunnel protocols.

### 9.2 No Structured RPC

Truffle uses JSON-over-WebSocket messaging, not protobuf/gRPC/Cap'n Proto. This is intentional:
- JSON is human-debuggable
- The message bus is pub/sub, not request/response
- File transfer handles bulk data (where binary efficiency matters)

If an application needs structured RPC, it should use the reverse proxy to expose a gRPC/REST server.

### 9.3 No Replicated Database

Store sync provides **eventual consistency of device-scoped slices**, not a replicated database. Specifically:
- Each device owns its own slice
- There are no cross-device transactions
- There is no conflict resolution (last-write-wins per device)
- There is no query language

For real database needs, use SQLite (local) with store sync for state sharing, or expose a database server via reverse proxy.

### 9.4 No User Authentication System

Truffle uses Tailscale's identity system. Every device on the tailnet is authenticated by Tailscale. Truffle does not implement:
- Username/password auth
- OAuth flows (beyond Tailscale's own)
- Session management
- Role-based access control

Device identity IS the auth system. A device's Tailscale identity (user + node) determines what it can do.

### 9.5 No Video/Audio Streaming

Truffle handles messages, files, and HTTP. It does not handle real-time media:
- No WebRTC
- No RTMP
- No audio/video codecs

For media, use the reverse proxy to expose a streaming server, or implement media-specific protocols at the application layer using the message bus for signaling.

### 9.6 No iOS/Android Native SDK

Truffle targets desktop and server platforms:
- macOS, Linux, Windows (via Rust)
- Browser (via PWA over the mesh)

There is no Swift or Kotlin SDK. Mobile clients connect as PWA browsers, which covers the primary use case (monitoring and control). If native mobile support is needed in the future, it would be a separate project (`truffle-swift`, `truffle-kotlin`).

### 9.7 No Automatic Sidecar Distribution

The Go sidecar binary must be provided by the consumer application. Truffle does not:
- Download the sidecar at runtime
- Bundle the sidecar in the crate
- Auto-update the sidecar

The consumer is responsible for packaging the sidecar with their application (e.g., Tauri bundles it as a sidecar binary, Electron includes it in the app directory). Build scripts in `truffle-napi` and `truffle-tauri-plugin` handle cross-compilation.

### 9.8 No Guaranteed Message Delivery

The message bus is best-effort. Messages can be lost if:
- The target device is offline
- The primary node crashes during routing
- A WebSocket connection drops between send and receive

For critical data, use file transfer (which has integrity verification and resume) or implement application-level acknowledgments over the message bus.

---

## Appendix A: How This Relates to the vibe-ctl Ecosystem

Truffle is the networking infrastructure that vibe-ctl and cheeseboard build on:

```
vibe-ctl (main product: remote Claude Code monitoring)
    |
    +-- truffle (mesh networking)
    |       |
    |       +-- sidecar-slim (Go/tsnet)
    |
    +-- cheeseboard (clipboard + file sync) -- uses truffle mesh
    |
    +-- xterm-r3f (terminal renderer) -- for the PWA UI
    |
    +-- r3f-msdf (text rendering) -- supports xterm-r3f
```

Truffle's design ensures it serves both vibe-ctl's needs (terminal monitoring, session sharing, push notifications) and cheeseboard's needs (clipboard sync, file drop) without either product knowing about the other.

## Appendix B: Tailscale Features Truffle Leverages

| Tailscale Feature | Truffle Usage |
|---|---|
| `tsnet.Server` | Embedded node per app instance |
| `ListenTLS ":443"` | Mesh WebSocket + HTTP services (auto-cert) |
| `Listen ":9417"` | File transfer data plane |
| `Dial(ctx, "tcp", addr)` | Outgoing connections to peers |
| `LocalClient.Status()` | Peer discovery |
| `LocalClient.WhoIs()` | Incoming connection identification (planned) |
| `LocalClient.StatusWithoutPeers()` | Health monitoring (planned) |
| MagicDNS | Addressing (`hostname.tailnet.ts.net`) |
| WireGuard tunnels | All traffic encrypted, zero config |
| DERP relays | Fallback when direct connections fail |
| Auto-certs (Let's Encrypt) | HTTPS on port 443 |
| ACLs + Tags | Node-level access control (planned) |

## Appendix C: Wire Protocol Summary

### Sidecar IPC (stdin/stdout JSON lines)

**Commands (Rust -> Go):**
```json
{"command": "tsnet:start", "data": {"hostname": "...", "stateDir": "...", "authKey": "...", "bridgePort": 12345, "sessionToken": "hex..."}}
{"command": "tsnet:stop"}
{"command": "tsnet:getPeers"}
{"command": "bridge:dial", "data": {"requestId": "uuid", "target": "peer.tail.ts.net", "port": 443}}
```

**Events (Go -> Rust):**
```json
{"event": "tsnet:status", "data": {"state": "running", "hostname": "...", "dnsName": "...", "tailscaleIP": "100.x.y.z"}}
{"event": "tsnet:authRequired", "data": {"authUrl": "https://login.tailscale.com/..."}}
{"event": "tsnet:peers", "data": {"peers": [...]}}
{"event": "bridge:dialResult", "data": {"requestId": "uuid", "success": true}}
{"event": "tsnet:stateChange", "data": {"previousState": "Running", "newState": "NeedsLogin", "authUrl": "..."}}
```

### Bridge Header (binary, big-endian)

```
[4 bytes] Magic: 0x54524646 ("TRFF")
[1 byte]  Version: 0x01
[32 bytes] Session token
[1 byte]  Direction: 0x01 (incoming) | 0x02 (outgoing)
[2 bytes] Service port (443 for WS/HTTP, 9417 for file transfer)
[2 bytes] Request ID length, then [N bytes] Request ID
[2 bytes] Remote addr length, then [N bytes] Remote addr
[2 bytes] Remote DNS name length, then [N bytes] Remote DNS name
```

### Mesh Envelope (JSON over WebSocket)

```json
{
    "namespace": "mesh",
    "type": "device:announce",
    "payload": { "device": {...}, "protocolVersion": 2 },
    "timestamp": 1710764400000
}
```

Namespaces: `mesh` (internal), `sync` (store sync), `file-transfer` (file transfer signaling), and any application-defined namespace.
