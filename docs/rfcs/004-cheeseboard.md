# Cheeseboard RFC: Cross-Device Clipboard Sync + File Drop

## RFC-004: Cheeseboard - Tauri v2 Tray Application

**Author:** James Yong
**Date:** 2026-03-08
**Status:** Draft
**Location:** `project100/p008/cheeseboard`
**License:** MIT

---

## 1. Overview

Cheeseboard is a system tray application for cross-device clipboard synchronization and file transfer, built on Tauri v2 and the truffle-core Rust mesh networking library. It runs as a background tray app with no main window, communicating over Tailscale with zero cloud dependency.

The key architectural advantage: truffle-core is pure Rust, truffle-tauri-plugin already exists with 16 commands and TypeScript guest-js bindings, so Cheeseboard can use them directly as path dependencies with no Node.js runtime or NAPI bridge required.

---

## 2. Architecture

### 2.1 High-Level Component Diagram

```
+--------------------------------------------------+
|  Cheeseboard App (Tauri v2)                       |
|                                                   |
|  +-----------+  +------------------------------+  |
|  | Frontend  |  | Rust Backend (src-tauri/)     |  |
|  | (src/)    |  |                              |  |
|  |           |  |  +------------------------+  |  |
|  | Tray      |<--->| Tauri Commands         |  |  |
|  | Popup     |  |  | (clipboard, filedrop,  |  |  |
|  | Settings  |  |  |  settings, tray)       |  |  |
|  | Drop Zone |  |  +------------------------+  |  |
|  |           |  |            |                 |  |
|  +-----------+  |  +------------------------+  |  |
|                 |  | ClipboardSyncEngine     |  |  |
|                 |  | - OS clipboard monitor  |  |  |
|                 |  | - Echo suppression      |  |  |
|                 |  | - Password detection    |  |  |
|                 |  +------------------------+  |  |
|                 |  | ClipboardHistoryStore   |  |  |
|                 |  | - impl SyncableStore   |  |  |
|                 |  | - synced via StoreSync  |  |  |
|                 |  +------------------------+  |  |
|                 |  | FileDropEngine          |  |  |
|                 |  | - FileTransferAdapter   |  |  |
|                 |  | - Accept/reject flow    |  |  |
|                 |  +------------------------+  |  |
|                 |  | MobileWebServer        |  |  |
|                 |  | - axum HTTP server      |  |  |
|                 |  | - serves web client     |  |  |
|                 |  | - QR code in tray       |  |  |
|                 |  +------------------------+  |  |
|                 |            |                 |  |
|                 |  +------------------------+  |  |
|                 |  | truffle-tauri-plugin    |  |  |
|                 |  | (TruffleState,          |  |  |
|                 |  |  MeshNode, commands)    |  |  |
|                 |  +------------------------+  |  |
|                 |            |                 |  |
|                 |  +------------------------+  |  |
|                 |  | truffle-core            |  |  |
|                 |  | (MeshNode, MessageBus,  |  |  |
|                 |  |  FileTransferManager,   |  |  |
|                 |  |  StoreSyncAdapter)      |  |  |
|                 |  +------------------------+  |  |
|                 +------------------------------+  |
+--------------------------------------------------+
            |                          |
       [Tailscale]              [Local HTTP]
            |                          |
  +-------------------+    +---------------------+
  | Other Cheeseboard |    | Mobile Web Client   |
  | desktop instances |    | (phone browser via  |
  +-------------------+    |  QR code scan)      |
                           +---------------------+
```

### 2.2 Rust Backend Architecture (`src-tauri/src/`)

The backend is organized into focused modules:

```
src-tauri/src/
  main.rs              -- Entry point (windows_subsystem)
  lib.rs               -- Tauri builder, plugin registration, run()
  state.rs             -- CheeseboardState (managed state)
  clipboard/
    mod.rs             -- ClipboardSyncEngine
    monitor.rs         -- OS clipboard polling loop
    echo.rs            -- Echo suppression (fingerprint ring buffer)
    store.rs           -- ClipboardHistoryStore (impl SyncableStore)
    types.rs           -- ClipboardEntry, ClipboardPayload, ContentType
  filedrop/
    mod.rs             -- FileDropEngine
    orchestrator.rs    -- Orchestrates offer/accept/reject with truffle adapter
    types.rs           -- FileDropEvent payloads
  mobile/
    mod.rs             -- MobileWebServer (axum HTTP + WebSocket)
    routes.rs          -- REST API routes for web client
    ws.rs              -- WebSocket handler for real-time events
    qr.rs              -- QR code generation for tray display
    static/            -- Embedded web client assets (built from src/mobile/)
  tray/
    mod.rs             -- System tray setup, menu building, icon management
    icons.rs           -- Icon state management (template images)
  config/
    mod.rs             -- Settings persistence (JSON in app data dir)
    types.rs           -- CheeseboardConfig struct
  commands/
    mod.rs             -- All Tauri command handlers
    clipboard.rs       -- Clipboard-specific commands
    filedrop.rs        -- File drop commands
    settings.rs        -- Settings commands
    mesh.rs            -- Mesh status commands (thin wrappers)
    mobile.rs          -- Mobile web server commands
```

### 2.3 Frontend Architecture (`src/`)

The frontend is **vanilla TypeScript + HTML/CSS** -- no framework. Rationale:
- This is a tray popup app, not a full application. Total UI surface is approximately 3-4 small views.
- Zero framework overhead means faster cold start (critical for tray popups).
- The truffle-tauri-plugin already ships guest-js TypeScript bindings.
- Vite as the build tool (consistent with James's stack) for HMR during development.

```
src/
  index.html           -- Tray popup (device list, recent clipboard, quick actions)
  settings.html        -- Settings page (opens in a separate window)
  dropzone.html        -- Floating drop zone window
  styles/
    global.css          -- Shared CSS variables, reset
    tray.css            -- Tray popup styles
    settings.css        -- Settings styles
    dropzone.css        -- Drop zone styles
  scripts/
    main.ts             -- Tray popup logic
    settings.ts         -- Settings page logic
    dropzone.ts         -- Drop zone logic
    truffle.ts           -- Re-exports from truffle-tauri-plugin guest-js
    types.ts             -- Shared TypeScript types
    events.ts            -- Event listener setup (mesh events, clipboard events)
```

---

## 3. Clipboard Sync Design

### 3.1 Clipboard Access Strategy

**Use the `arboard` crate directly in Rust**, not `tauri-plugin-clipboard-manager`.

Rationale:
- `tauri-plugin-clipboard-manager` provides `read_text`/`write_text` commands but does not support change detection or polling -- it is purely on-demand read/write.
- `arboard` (v3+) provides `Clipboard::new()` with `get_text()`, `get_image()`, `set_text()`, `set_image()` -- everything needed.
- Clipboard monitoring requires polling (no OS provides a reliable push notification on all platforms). A Rust-side polling loop is more efficient than IPC round-trips from the frontend.
- macOS `NSPasteboard.changeCount` provides an efficient change counter -- `arboard` exposes this indirectly (the clipboard contents hash will change).

### 3.2 ClipboardSyncEngine

```rust
// src-tauri/src/clipboard/mod.rs

pub struct ClipboardSyncEngine {
    /// Local device ID (from MeshNode config)
    device_id: String,
    /// Reference to MeshNode for sending/broadcasting
    mesh_node: Arc<MeshNode>,
    /// Echo suppression ring buffer
    echo_guard: EchoGuard,
    /// Last known clipboard change count (macOS) or content hash
    last_fingerprint: RwLock<Option<ClipboardFingerprint>>,
    /// Clipboard history store (synced across all mesh nodes via StoreSyncAdapter)
    history_store: Arc<ClipboardHistoryStore>,
    /// Whether sync is enabled
    enabled: RwLock<bool>,
    /// Shutdown token
    shutdown: CancellationToken,
    /// App handle for emitting events to frontend
    app_handle: AppHandle,
}
```

### 3.3 Clipboard Polling Loop

```rust
// src-tauri/src/clipboard/monitor.rs

impl ClipboardSyncEngine {
    /// Start the clipboard monitoring loop.
    /// Polls every 500ms (configurable). Uses macOS changeCount for
    /// efficient change detection before reading content.
    pub async fn start_monitor(&self) {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.check_clipboard().await;
                }
                _ = self.shutdown.cancelled() => {
                    return;
                }
            }
        }
    }

    async fn check_clipboard(&self) {
        // 1. Read clipboard (in blocking task to avoid blocking tokio)
        let result = tokio::task::spawn_blocking(|| {
            let mut clipboard = arboard::Clipboard::new()?;
            // Try text first, then image
            // On macOS, check for org.nspasteboard.ConcealedType
            // (requires raw NSPasteboard access via objc2 crate)
        }).await;

        // 2. Compute fingerprint
        let fingerprint = ClipboardFingerprint::from_content(&content);

        // 3. Check if changed (compare fingerprint)
        // 4. Check echo guard (is this something WE just wrote?)
        // 5. Check password/concealed type
        // 6. If new content from user: broadcast via mesh
    }
}
```

### 3.4 Echo Suppression

```rust
// src-tauri/src/clipboard/echo.rs

/// Prevents re-syncing content that was just written by a remote device.
/// Uses a ring buffer of recent fingerprints with TTL.
pub struct EchoGuard {
    /// Ring buffer of fingerprints we recently wrote to clipboard
    recent_writes: Mutex<VecDeque<(ClipboardFingerprint, Instant)>>,
    /// Time-to-live for fingerprints (default: 2 seconds)
    ttl: Duration,
    /// Max entries in ring buffer
    capacity: usize,
}

impl EchoGuard {
    /// Record that we are about to write to the clipboard.
    /// Called BEFORE writing remote content to local clipboard.
    pub fn record_write(&self, fingerprint: &ClipboardFingerprint) { ... }

    /// Check if this fingerprint was recently written by us.
    /// Called AFTER detecting a clipboard change.
    pub fn is_echo(&self, fingerprint: &ClipboardFingerprint) -> bool { ... }
}
```

The fingerprint is a fast hash (xxhash or FNV) of the clipboard content bytes, not a full SHA-256 (speed matters for polling).

### 3.5 Clipboard History via StoreSyncAdapter

Clipboard history is **not** stored locally per-device. Instead, it uses truffle-core's
`StoreSyncAdapter` so all mesh nodes share one unified clipboard history. When you
copy something on Device A, it appears in the history on Device B (and vice versa)
without any extra broadcast -- StoreSyncAdapter handles the replication automatically.

```rust
// src-tauri/src/clipboard/store.rs

use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;
use truffle_core::store_sync::adapter::SyncableStore;
use truffle_core::store_sync::types::DeviceSlice;

/// ClipboardHistoryStore implements SyncableStore for truffle's StoreSyncAdapter.
///
/// Each device maintains its own slice of clipboard entries (things copied ON
/// that device). StoreSyncAdapter merges all device slices so every node sees
/// the full history from all devices.
///
/// Data shape per device slice:
/// {
///   "entries": [
///     { "id": "uuid", "contentType": "text", "content": "...",
///       "fingerprint": "xxh3...", "timestamp": 1234567890,
///       "pinned": false, "preview": "first 80 chars..." },
///     ...
///   ]
/// }
pub struct ClipboardHistoryStore {
    store_id: String,
    local_device_id: String,
    /// This device's clipboard entries (things copied on THIS device)
    local_entries: RwLock<VecDeque<ClipboardEntry>>,
    /// Merged entries from all devices (local + remote), sorted by timestamp
    merged_history: RwLock<Vec<ClipboardEntry>>,
    /// Max entries per device slice
    max_per_device: usize,
    /// Max total merged history entries
    max_total: usize,
}

impl SyncableStore for ClipboardHistoryStore {
    fn store_id(&self) -> &str {
        &self.store_id // "clipboard-history"
    }

    fn get_local_slice(&self) -> Option<DeviceSlice> {
        let entries = self.local_entries.blocking_read();
        if entries.is_empty() {
            return None;
        }
        Some(DeviceSlice {
            device_id: self.local_device_id.clone(),
            data: serde_json::json!({ "entries": *entries }),
            updated_at: entries.front().map(|e| e.timestamp).unwrap_or(0),
            version: entries.len() as u64,
        })
    }

    fn apply_remote_slice(&self, slice: DeviceSlice) {
        // Parse remote entries, merge into merged_history
        // sorted by timestamp descending, deduped by fingerprint
    }

    fn remove_remote_slice(&self, device_id: &str, _reason: &str) {
        // Remove entries from that device from merged_history
    }

    fn clear_remote_slices(&self) {
        // Keep only local entries in merged_history
    }
}

impl ClipboardHistoryStore {
    /// Get the full merged history (all devices), sorted by most recent first.
    /// This is what the tray popup and Cmd+Shift+V palette display.
    pub async fn merged_history(&self) -> Vec<ClipboardEntry> {
        self.merged_history.read().await.clone()
    }

    /// Add a new local clipboard entry (called when user copies on this device).
    /// Automatically triggers StoreSyncAdapter to broadcast to other nodes.
    pub async fn add_local_entry(&self, entry: ClipboardEntry) {
        let mut entries = self.local_entries.write().await;
        // Dedup by fingerprint (don't add if same content exists recently)
        entries.retain(|e| e.fingerprint != entry.fingerprint);
        entries.push_front(entry.clone());
        while entries.len() > self.max_per_device {
            entries.pop_back();
        }
        // Also add to merged history
        let mut merged = self.merged_history.write().await;
        merged.retain(|e| e.fingerprint != entry.fingerprint);
        merged.insert(0, entry);
        while merged.len() > self.max_total {
            merged.pop();
        }
    }

    /// Pin/unpin an entry (persists across syncs).
    pub async fn toggle_pin(&self, entry_id: &str) { ... }

    /// Search history across all devices.
    pub async fn search(&self, query: &str) -> Vec<ClipboardEntry> { ... }
}
```

**How it flows:**
1. User copies text on Device A
2. `ClipboardSyncEngine` detects change, creates `ClipboardEntry`
3. Entry added to `ClipboardHistoryStore::add_local_entry()`
4. `StoreSyncAdapter` detects the store changed, broadcasts `store:sync:update` to all mesh nodes
5. Device B's `StoreSyncAdapter` receives update, calls `apply_remote_slice()`
6. Device B's merged history now includes Device A's entry
7. Device B's tray popup / clipboard palette shows the entry with "[A]" source indicator

**Benefits over per-device history:**
- One shared history across all devices -- search on any device finds everything
- Pinned items are synced (pin on one device, available on all)
- No separate "clipboard sync" broadcast needed -- StoreSyncAdapter handles replication
- New devices joining the mesh get the full history automatically via `store:sync:full`
- Offline devices catch up when they reconnect

### 3.6 Password Manager Detection (macOS)

On macOS, password managers set `org.nspasteboard.ConcealedType` on the pasteboard. This requires raw `NSPasteboard` access:

```rust
// Requires objc2 + objc2-app-kit crates on macOS only
#[cfg(target_os = "macos")]
fn is_concealed() -> bool {
    // Use NSPasteboard::generalPasteboard()
    // Check for type "org.nspasteboard.ConcealedType"
    // Also check "org.nspasteboard.AutoGeneratedType" for auto-fill
}
```

When concealed content is detected, skip syncing entirely. Log it but do not transmit.

### 3.6 Message Format (MessageBus)

Clipboard sync uses the `clipboard` namespace on the MessageBus:

```rust
pub const CLIPBOARD_NAMESPACE: &str = "clipboard";

pub mod clipboard_message_types {
    pub const CLIPBOARD_UPDATE: &str = "clipboard:update";
    pub const CLIPBOARD_REQUEST: &str = "clipboard:request";
    pub const CLIPBOARD_CLEAR: &str = "clipboard:clear";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardPayload {
    /// Source device ID
    pub device_id: String,
    /// Content type: "text" or "image"
    pub content_type: String,
    /// For text: the text content. For image: base64-encoded PNG.
    pub content: String,
    /// Fingerprint hash for dedup
    pub fingerprint: String,
    /// Content size in bytes (for size limit enforcement)
    pub size: usize,
    /// Timestamp
    pub timestamp: u64,
}
```

### 3.7 Size Limits

| Content Type | Max Size | Behavior |
|---|---|---|
| Text | 1 MB | Truncate with warning |
| Image (PNG) | 10 MB | Skip with notification |

Images are base64-encoded before transmission over the MessageBus JSON payload. For images exceeding 10 MB, show a notification suggesting file drop instead.

### 3.8 Receiving Remote Clipboard Content

When a `clipboard:update` message arrives via the MeshNode event stream:

1. Deserialize `ClipboardPayload`
2. Skip if `device_id == local_device_id` (self-echo)
3. Record fingerprint in `EchoGuard` (pre-write)
4. Write to OS clipboard via `arboard`
5. Add to clipboard history
6. Emit `cheeseboard://clipboard-synced` event to frontend
7. Show notification (if enabled in settings)

---

## 4. File Drop Design

### 4.1 Integration with truffle-core

File transfer uses `FileTransferAdapter` and `FileTransferManager` from truffle-core directly. No NAPI bridge needed -- these are pure Rust.

```rust
// src-tauri/src/filedrop/mod.rs

pub struct FileDropEngine {
    adapter: Arc<FileTransferAdapter>,
    manager: Arc<FileTransferManager>,
    /// Output directory for received files
    output_dir: RwLock<String>,
    /// Pending offers awaiting user accept/reject
    pending_offers: RwLock<HashMap<String, FileTransferOffer>>,
    /// App handle for UI notifications
    app_handle: AppHandle,
    shutdown: CancellationToken,
}
```

### 4.2 Send Flow

1. User picks file via tray menu "Send File to..." or drops onto drop zone window
2. Frontend calls Tauri command `cheeseboard_send_file(device_id, file_path)`
3. Rust: `FileDropEngine` calls `adapter.send_file(device_id, file_path)`
4. truffle-core prepares file (stat + hash), emits `Prepared` event
5. Rust constructs `FileTransferOffer` and sends via MessageBus
6. Remote receives OFFER, shows accept/reject notification
7. On ACCEPT: truffle-core streams file via HTTP PUT over Tailscale
8. Progress events emitted to frontend via Tauri events

### 4.3 Receive Flow

1. `FileDropEngine` event loop receives `AdapterEvent::Offer`
2. Store in `pending_offers` map
3. Emit `cheeseboard://file-offer` event to frontend
4. Show notification with accept/reject actions
5. User clicks Accept: calls `cheeseboard_accept_transfer(transfer_id)`
6. Rust: `adapter.accept_transfer(&offer, save_path)`
7. truffle-core registers receiver, sends ACCEPT message, begins receiving
8. Progress events flow to frontend
9. On completion: notification with "Open" / "Show in Finder" actions

### 4.4 Tauri Commands for File Drop

```rust
#[command]
async fn cheeseboard_send_file(
    state: State<'_, CheeseboardState>,
    device_id: String,
    file_path: String,
) -> Result<String, String>;

#[command]
async fn cheeseboard_accept_transfer(
    state: State<'_, CheeseboardState>,
    transfer_id: String,
    save_path: Option<String>,
) -> Result<(), String>;

#[command]
async fn cheeseboard_reject_transfer(
    state: State<'_, CheeseboardState>,
    transfer_id: String,
    reason: String,
) -> Result<(), String>;

#[command]
async fn cheeseboard_cancel_transfer(
    state: State<'_, CheeseboardState>,
    transfer_id: String,
) -> Result<(), String>;

#[command]
async fn cheeseboard_list_transfers(
    state: State<'_, CheeseboardState>,
) -> Result<Vec<AdapterTransferInfo>, String>;

#[command]
async fn cheeseboard_get_pending_offers(
    state: State<'_, CheeseboardState>,
) -> Result<Vec<FileTransferOffer>, String>;
```

### 4.5 Drop Zone Window

A small floating transparent window (approximately 200x200px) that can be shown/hidden. When files are dragged onto it, they trigger the send flow. Uses Tauri's built-in drag-drop event (`tauri::DragDropEvent`) -- no additional plugin needed since Tauri v2 has it built-in.

The drop zone window is created as a second Tauri window with:
- `decorations: false`
- `transparent: true`
- `always_on_top: true`
- `skip_taskbar: true`
- `resizable: false`
- Small fixed size

---

## 5. Mobile Access via Embedded Web Server

### 5.1 Strategy: No Native Mobile Apps

Instead of building native iOS/Android apps (which face severe platform restrictions
like iOS clipboard background access limits), each desktop Cheeseboard instance
hosts a **built-in web app** accessible from any mobile browser.

The tray menu shows a QR code. User scans it with their phone. Instant access to
clipboard history, file sending, and file receiving -- no app install required.

### 5.2 MobileWebServer Architecture

```rust
// src-tauri/src/mobile/mod.rs

use axum::{Router, extract::State as AxumState, routing::{get, post}};
use tokio::net::TcpListener;

pub struct MobileWebServer {
    /// Local HTTP port (random ephemeral, bound at startup)
    port: u16,
    /// Auth token (256-bit random, embedded in QR code URL)
    auth_token: String,
    /// Reference to clipboard history store
    history_store: Arc<ClipboardHistoryStore>,
    /// Reference to file drop engine
    filedrop_engine: Arc<FileDropEngine>,
    /// Reference to mesh node (for device info)
    mesh_node: Arc<MeshNode>,
    /// Shutdown token
    shutdown: CancellationToken,
}

impl MobileWebServer {
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let app = Router::new()
            // Static web client (embedded at compile time via include_dir)
            .route("/", get(serve_index))
            .route("/assets/*path", get(serve_static))
            // REST API (all require ?token= auth)
            .route("/api/devices", get(routes::list_devices))
            .route("/api/clipboard", get(routes::get_clipboard_history))
            .route("/api/clipboard", post(routes::send_clipboard))
            .route("/api/files/send", post(routes::send_file))
            .route("/api/files/upload", post(routes::upload_file))
            .route("/api/transfers", get(routes::list_transfers))
            // WebSocket for real-time events
            .route("/ws", get(ws::websocket_handler))
            .with_state(self.app_state());

        let listener = TcpListener::bind(("0.0.0.0", self.port)).await?;
        axum::serve(listener, app)
            .with_graceful_shutdown(self.shutdown.cancelled_owned())
            .await?;
        Ok(())
    }
}
```

### 5.3 QR Code in Tray

The tray popup shows a QR code that encodes:
```
http://<tailscale-ip>:<port>/?token=<auth-token>
```

The URL uses the device's Tailscale IP so it works from any device on the same
Tailnet. The auth token prevents unauthorized access.

```rust
// src-tauri/src/mobile/qr.rs

use qrcode::{QrCode, render::svg};

pub fn generate_qr_svg(tailscale_ip: &str, port: u16, token: &str) -> String {
    let url = format!("http://{}:{}/?token={}", tailscale_ip, port, token);
    let code = QrCode::new(url.as_bytes()).unwrap();
    code.render::<svg::Color>()
        .min_dimensions(200, 200)
        .build()
}
```

**Tray popup layout with QR:**
```
+----------------------------------+
| Devices              [QR icon]   |  <- click QR icon to show/hide
|----------------------------------|
| [green] MacBook Pro (this)       |
| [green] Desktop PC               |
|----------------------------------|
|     +------------------+         |
|     |  [QR CODE]       |         |  <- scan with phone
|     |                  |         |
|     +------------------+         |
|  Scan to connect from phone      |
|----------------------------------|
| Clipboard History                |
| ...                              |
+----------------------------------+
```

### 5.4 Mobile Web Client

A lightweight single-page app embedded in the Cheeseboard binary at compile time
(via `include_dir!` or Rust's `include_str!`). Built with vanilla HTML/CSS/JS
(consistent with desktop frontend).

**Views:**
1. **Home** — device list, clipboard history, quick actions
2. **Send** — text input + file upload + device picker
3. **History** — full searchable clipboard history from all devices

**Key features:**
- **Paste from phone**: type or paste text on phone → sends to desktop clipboard
  via `POST /api/clipboard` → desktop writes to OS clipboard
- **Send file from phone**: upload file via `POST /api/files/upload` → file
  forwarded to target device via truffle file transfer
- **Receive on phone**: WebSocket push notification when files arrive, with
  download link
- **Clipboard history**: browse the shared history (from StoreSyncAdapter),
  tap to copy on phone or send to any desktop device

**WebSocket events (server → client):**
```json
{ "type": "clipboard:new", "entry": { ... } }
{ "type": "device:discovered", "device": { ... } }
{ "type": "device:offline", "deviceId": "..." }
{ "type": "transfer:progress", "transferId": "...", "percent": 67 }
{ "type": "transfer:complete", "transferId": "...", "path": "..." }
```

### 5.5 Security

- **Auth token**: 256-bit random token generated per session. Embedded in QR URL.
  No token = 401 on all endpoints.
- **HTTPS**: Optional. For local Tailscale network, HTTP is acceptable (traffic
  is already encrypted by WireGuard). Can add self-signed cert for browsers that
  warn about HTTP.
- **Token rotation**: Token changes when user clicks "Regenerate" in settings or
  on app restart. Old tokens are immediately invalidated.
- **Rate limiting**: Basic rate limiting on API endpoints to prevent abuse.
- **No persistent sessions**: Token is in the URL query param. Closing the browser
  tab = disconnected. No cookies, no localStorage auth.

### 5.6 Dependencies

```toml
# Additional deps for mobile web server
axum = "0.8"       # Already a dep of truffle-core
qrcode = "0.14"    # QR code generation
include_dir = "0.7" # Embed static assets at compile time
```

---

## 6. System Tray Design


### 5.1 Tauri v2 Tray API

Tauri v2 has built-in tray support via `tauri::tray::TrayIconBuilder`. No separate plugin needed.

```rust
// src-tauri/src/tray/mod.rs

use tauri::tray::{TrayIconBuilder, TrayIconEvent, MouseButton, MouseButtonState};
use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder, PredefinedMenuItem};
use tauri::{AppHandle, Manager};

pub fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .icon_as_template(true)  // macOS dark/light mode
        .tooltip("Cheeseboard")
        .menu(&build_tray_menu(app)?)
        .on_tray_icon_event(|tray, event| {
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => {
                    // Show tray popup window
                    toggle_tray_popup(tray.app_handle());
                }
                _ => {}
            }
        })
        .on_menu_event(handle_menu_event)
        .build(app)?;

    Ok(())
}
```

### 5.2 Tray Icon States

Three icon states, each with macOS template image support (monochrome for automatic dark/light mode adaptation):

| State | Icon | Description |
|---|---|---|
| Connected | Cheese wedge (solid) | Mesh active, devices visible |
| Syncing | Cheese wedge + arrows | Clipboard syncing or file transferring |
| Disconnected | Cheese wedge (outline/dimmed) | No mesh connection |

Icon files stored at `src-tauri/icons/tray/`:
- `tray-connected.png` + `tray-connected@2x.png`
- `tray-syncing.png` + `tray-syncing@2x.png`
- `tray-disconnected.png` + `tray-disconnected@2x.png`

### 5.3 Tray Menu Structure

```
[Cheeseboard]  ← tray icon

Context menu (right-click):
─────────────────────────
 Devices
   ✓ MacBook Pro (this device)
   ● Desktop PC          →  Send File...
   ○ Linux Server        (offline)
─────────────────────────
 Send File...             ← native file picker
 Show Drop Zone           ← toggles floating window
─────────────────────────
 Clipboard History
   "Hello world..."      ← click to re-copy
   "https://example..."
   [image preview]
   (5 more items)
─────────────────────────
 ✓ Clipboard Sync Enabled
   Settings...
─────────────────────────
   Quit Cheeseboard
```

Left-click on macOS shows a tray popup window (like Raycast) with a richer UI than a context menu. This popup contains:
- Device list with online status
- Recent clipboard entries
- Active transfers with progress
- Quick settings toggle

### 5.4 Tray Popup Window

A Tauri window positioned at the tray icon location:

```rust
fn toggle_tray_popup(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("tray-popup") {
        if window.is_visible().unwrap_or(false) {
            window.hide().unwrap();
        } else {
            // Position at tray icon
            position_at_tray(app, &window);
            window.show().unwrap();
            window.set_focus().unwrap();
        }
    }
}
```

Window config in `tauri.conf.json`:
```json
{
    "label": "tray-popup",
    "url": "index.html",
    "visible": false,
    "decorations": false,
    "transparent": true,
    "skipTaskbar": true,
    "alwaysOnTop": true,
    "width": 360,
    "height": 480,
    "resizable": false,
    "focus": false
}
```

---

## 7. State Management

### 7.1 CheeseboardState (Tauri Managed State)

```rust
// src-tauri/src/state.rs

use std::sync::Arc;
use tokio::sync::RwLock;

pub struct CheeseboardState {
    /// Clipboard sync engine
    pub clipboard_engine: RwLock<Option<Arc<ClipboardSyncEngine>>>,
    /// Clipboard history store (synced across mesh via StoreSyncAdapter)
    pub clipboard_history: RwLock<Option<Arc<ClipboardHistoryStore>>>,
    /// Store sync adapter (manages clipboard history replication)
    pub store_sync: RwLock<Option<Arc<StoreSyncAdapter>>>,
    /// File drop engine
    pub filedrop_engine: RwLock<Option<Arc<FileDropEngine>>>,
    /// Mobile web server
    pub mobile_server: RwLock<Option<Arc<MobileWebServer>>>,
    /// App configuration
    pub config: RwLock<CheeseboardConfig>,
    /// Connection status for tray icon updates
    pub connection_status: RwLock<ConnectionStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConnectionStatus {
    Connected,
    Syncing,
    Disconnected,
}
```

### 7.2 Configuration Persistence

```rust
// src-tauri/src/config/types.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheeseboardConfig {
    /// Unique device ID (generated on first run, persisted)
    pub device_id: String,
    /// User-friendly device name
    pub device_name: String,
    /// Clipboard sync enabled
    pub clipboard_sync_enabled: bool,
    /// Clipboard polling interval in ms
    pub clipboard_poll_interval_ms: u64,
    /// Max clipboard history entries
    pub max_clipboard_history: usize,
    /// Sync images (can be disabled for bandwidth)
    pub sync_images: bool,
    /// Max image size for clipboard sync (bytes)
    pub max_image_size: usize,
    /// File drop output directory
    pub file_output_dir: String,
    /// Auto-accept files from trusted devices
    pub auto_accept_from: Vec<String>,
    /// Show notifications
    pub notifications_enabled: bool,
    /// Launch at login
    pub auto_start: bool,
    /// Truffle mesh port
    pub mesh_port: u16,
    /// Truffle hostname prefix
    pub hostname_prefix: String,
}

impl Default for CheeseboardConfig {
    fn default() -> Self {
        Self {
            device_id: generate_device_id(),
            device_name: hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "Unknown".to_string()),
            clipboard_sync_enabled: true,
            clipboard_poll_interval_ms: 500,
            max_clipboard_history: 50,
            sync_images: true,
            max_image_size: 10 * 1024 * 1024, // 10 MB
            file_output_dir: default_download_dir(),
            auto_accept_from: vec![],
            notifications_enabled: true,
            auto_start: false,
            mesh_port: 9418,
            hostname_prefix: "cheeseboard",
        }
    }
}
```

Config stored at `{app_data_dir}/config.json`. Use `tauri::api::path::app_data_dir()` to resolve.

---

## 8. Project Structure

```
project100/p008/cheeseboard/
  Cargo.toml              -- Workspace-level if needed, or redirect
  package.json            -- Frontend: vite, typescript
  vite.config.ts          -- Vite config
  tsconfig.json
  index.html              -- Vite entry (redirects to tray-popup)
  src/
    index.html            -- Tray popup view
    settings.html         -- Settings view
    dropzone.html         -- Drop zone view
    styles/
      global.css
      tray.css
      settings.css
      dropzone.css
    scripts/
      main.ts
      settings.ts
      dropzone.ts
      truffle.ts
      types.ts
      events.ts
    mobile/               -- Mobile web client (embedded into binary at compile time)
      index.html          -- Single-page app for phone browser
      style.css           -- Mobile-optimized styles
      app.js              -- Mobile client logic (vanilla JS, no build step)
      ws.js               -- WebSocket client for real-time events
  src-tauri/
    Cargo.toml
    build.rs
    tauri.conf.json
    capabilities/
      default.json
    icons/
      icon.icns
      icon.ico
      32x32.png
      128x128.png
      128x128@2x.png
      tray/
        tray-connected.png
        tray-connected@2x.png
        tray-syncing.png
        tray-syncing@2x.png
        tray-disconnected.png
        tray-disconnected@2x.png
    src/
      main.rs
      lib.rs
      state.rs
      clipboard/
        mod.rs
        monitor.rs
        echo.rs
        types.rs
      filedrop/
        mod.rs
        orchestrator.rs
        types.rs
      tray/
        mod.rs
        icons.rs
      config/
        mod.rs
        types.rs
      commands/
        mod.rs
        clipboard.rs
        filedrop.rs
        settings.rs
        mesh.rs
```

### 8.1 Dependency Graph (Cargo.toml)

```toml
# src-tauri/Cargo.toml

[package]
name = "cheeseboard"
version = "0.1.0"
edition = "2021"
description = "Cross-device clipboard sync and file drop"
authors = ["James Yong"]
license = "MIT"

[lib]
name = "cheeseboard_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
# Truffle (local path deps, crates.io later)
truffle-core = { path = "../../truffle/crates/truffle-core" }
truffle-tauri-plugin = { path = "../../truffle/crates/truffle-tauri-plugin" }

# Tauri
tauri = { version = "2", features = [] }
tauri-plugin-notification = "2"
tauri-plugin-dialog = "2"
tauri-plugin-autostart = "2"
tauri-plugin-fs = "2"
tauri-plugin-os = "2"
tauri-plugin-global-shortcut = "2"

# Clipboard
arboard = { version = "3", features = ["image-data"] }

# Async runtime
tokio = { version = "1", features = ["rt-multi-thread", "sync", "macros", "time", "fs"] }
tokio-util = { version = "0.7", features = ["sync"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Utilities
tracing = "0.1"
tracing-subscriber = "0.3"
uuid = { version = "1", features = ["v4"] }
base64 = "0.22"
xxhash-rust = { version = "0.8", features = ["xxh3"] }  # Fast fingerprinting
hostname = "0.4"
directories = "5"  # XDG/known folder paths
image = { version = "0.25", default-features = false, features = ["png"] }

# Mobile web server
qrcode = "0.14"        # QR code generation for tray display
include_dir = "0.7"    # Embed mobile web client at compile time

# macOS-specific
[target.'cfg(target_os = "macos")'.dependencies]
objc2 = "0.6"
objc2-app-kit = { version = "0.3", features = ["NSPasteboard"] }
```

### 8.2 Frontend Dependencies (package.json)

```json
{
  "name": "@vibecook/cheeseboard",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc && vite build",
    "preview": "vite preview",
    "tauri": "tauri"
  },
  "devDependencies": {
    "@tauri-apps/cli": "^2",
    "typescript": "^5",
    "vite": "^7"
  },
  "dependencies": {
    "@tauri-apps/api": "^2",
    "@tauri-apps/plugin-notification": "^2",
    "@tauri-apps/plugin-dialog": "^2",
    "@tauri-apps/plugin-fs": "^2",
    "@tauri-apps/plugin-os": "^2",
    "@tauri-apps/plugin-autostart": "^2",
    "@tauri-apps/plugin-global-shortcut": "^2"
  }
}
```

---

## 9. Tauri Configuration

### 9.1 tauri.conf.json

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Cheeseboard",
  "version": "0.1.0",
  "identifier": "com.jamesyong.cheeseboard",
  "build": {
    "beforeDevCommand": "npm run dev",
    "devUrl": "http://localhost:1421",
    "beforeBuildCommand": "npm run build",
    "frontendDist": "../dist"
  },
  "app": {
    "windows": [
      {
        "label": "tray-popup",
        "title": "Cheeseboard",
        "url": "index.html",
        "visible": false,
        "decorations": false,
        "transparent": true,
        "skipTaskbar": true,
        "alwaysOnTop": true,
        "width": 360,
        "height": 480,
        "resizable": false,
        "focus": false
      }
    ],
    "security": {
      "csp": "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'"
    },
    "trayIcon": {
      "iconPath": "icons/tray/tray-disconnected.png",
      "iconAsTemplate": true,
      "tooltip": "Cheeseboard"
    }
  },
  "bundle": {
    "active": true,
    "targets": ["dmg", "app"],
    "icon": [
      "icons/32x32.png",
      "icons/128x128.png",
      "icons/128x128@2x.png",
      "icons/icon.icns",
      "icons/icon.ico"
    ],
    "macOS": {
      "minimumSystemVersion": "10.15"
    }
  },
  "plugins": {
    "notification": {},
    "dialog": {},
    "autostart": {},
    "fs": {
      "scope": {
        "allow": ["$DOWNLOAD/**", "$HOME/Cheeseboard/**"]
      }
    },
    "os": {},
    "global-shortcut": {}
  }
}
```

### 9.2 Capabilities (capabilities/default.json)

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Capabilities for Cheeseboard tray app",
  "windows": ["tray-popup", "settings", "dropzone"],
  "permissions": [
    "core:default",
    "core:event:default",
    "core:window:default",
    "core:window:allow-create",
    "core:window:allow-show",
    "core:window:allow-hide",
    "core:window:allow-close",
    "core:window:allow-set-position",
    "core:window:allow-set-focus",
    "core:tray:default",
    "notification:default",
    "dialog:default",
    "dialog:allow-open",
    "dialog:allow-save",
    "autostart:default",
    "fs:default",
    "os:default",
    "global-shortcut:default",
    "truffle:default"
  ]
}
```

---

## 10. Tauri Commands (Full List)

### 10.1 Application Commands

```rust
// -- Clipboard --
cheeseboard_get_clipboard_history    // -> Vec<ClipboardEntry>
cheeseboard_clear_clipboard_history  // -> ()
cheeseboard_set_clipboard_sync       // (enabled: bool) -> ()
cheeseboard_copy_history_entry       // (index: usize) -> ()

// -- File Drop --
cheeseboard_send_file                // (device_id, file_path) -> String (transfer_id)
cheeseboard_accept_transfer          // (transfer_id, save_path?) -> ()
cheeseboard_reject_transfer          // (transfer_id, reason) -> ()
cheeseboard_cancel_transfer          // (transfer_id) -> ()
cheeseboard_list_transfers           // -> Vec<AdapterTransferInfo>
cheeseboard_get_pending_offers       // -> Vec<FileTransferOffer>

// -- Settings --
cheeseboard_get_config               // -> CheeseboardConfig
cheeseboard_update_config            // (config: CheeseboardConfig) -> ()
cheeseboard_get_output_dir           // -> String
cheeseboard_set_output_dir           // (path: String) -> ()

// -- Mesh Status (thin wrappers around truffle plugin) --
cheeseboard_get_devices              // -> Vec<DeviceInfo>
cheeseboard_get_connection_status    // -> String
cheeseboard_get_device_id            // -> String

// -- Tray --
cheeseboard_toggle_dropzone          // -> ()
cheeseboard_show_settings            // -> ()

// -- Mobile Web Server --
cheeseboard_get_mobile_qr            // -> String (SVG of QR code)
cheeseboard_get_mobile_url           // -> String (http://tailscale-ip:port/?token=...)
cheeseboard_regenerate_mobile_token  // -> String (new token, invalidates old)
cheeseboard_is_mobile_server_running // -> bool
```

### 10.2 Tauri Events (Backend -> Frontend)

```
cheeseboard://clipboard-synced       { deviceId, contentType, preview }
cheeseboard://clipboard-changed      { contentType, preview }
cheeseboard://file-offer             { transferId, senderDeviceId, file }
cheeseboard://transfer-progress      { transferId, percent, bytesPerSecond, eta }
cheeseboard://transfer-complete      { transferId, path, size }
cheeseboard://transfer-failed        { transferId, code, message }
cheeseboard://connection-changed     { status, deviceCount }
cheeseboard://device-discovered      { device }
cheeseboard://device-offline         { deviceId }
```

---

## 11. App Initialization (lib.rs)

```rust
// src-tauri/src/lib.rs

pub fn run() {
    tauri::Builder::default()
        // Truffle plugin (provides mesh networking)
        .plugin(truffle_tauri_plugin::init())
        // Tauri ecosystem plugins
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_global_shortcut::init())
        // App state
        .manage(CheeseboardState::default())
        // Commands
        .invoke_handler(tauri::generate_handler![
            commands::clipboard::cheeseboard_get_clipboard_history,
            commands::clipboard::cheeseboard_clear_clipboard_history,
            commands::clipboard::cheeseboard_set_clipboard_sync,
            commands::clipboard::cheeseboard_copy_history_entry,
            commands::filedrop::cheeseboard_send_file,
            commands::filedrop::cheeseboard_accept_transfer,
            commands::filedrop::cheeseboard_reject_transfer,
            commands::filedrop::cheeseboard_cancel_transfer,
            commands::filedrop::cheeseboard_list_transfers,
            commands::filedrop::cheeseboard_get_pending_offers,
            commands::settings::cheeseboard_get_config,
            commands::settings::cheeseboard_update_config,
            commands::mesh::cheeseboard_get_devices,
            commands::mesh::cheeseboard_get_connection_status,
            commands::mesh::cheeseboard_get_device_id,
            commands::settings::cheeseboard_show_settings,
        ])
        // Setup
        .setup(|app| {
            // 1. Load or create config
            let config = config::load_or_create(app.handle())?;

            // 2. Initialize MeshNode via TruffleState
            let truffle_state = app.state::<truffle_tauri_plugin::TruffleState>();
            let connection_manager = /* create from config */;
            let (mesh_node, event_rx) = MeshNode::new(
                MeshNodeConfig {
                    device_id: config.device_id.clone(),
                    device_name: config.device_name.clone(),
                    device_type: "desktop".to_string(),
                    hostname_prefix: config.hostname_prefix.clone(),
                    prefer_primary: false,
                    capabilities: vec![
                        "clipboard-sync".to_string(),
                        "file-transfer".to_string(),
                    ],
                    metadata: None,
                    timing: MeshTimingConfig::default(),
                },
                connection_manager,
            );
            let mesh_node = Arc::new(mesh_node);

            // 3. Initialize ClipboardSyncEngine
            let clipboard_engine = ClipboardSyncEngine::new(
                config.device_id.clone(),
                Arc::clone(&mesh_node),
                app.handle().clone(),
                config.max_clipboard_history,
            );
            let clipboard_engine = Arc::new(clipboard_engine);

            // 4. Initialize FileDropEngine
            let filedrop_engine = FileDropEngine::new(
                Arc::clone(&mesh_node),
                config.clone(),
                app.handle().clone(),
            );
            let filedrop_engine = Arc::new(filedrop_engine);

            // 5. Store engines in CheeseboardState
            let cb_state = app.state::<CheeseboardState>();
            {
                let mut ce = cb_state.clipboard_engine.blocking_write();
                *ce = Some(Arc::clone(&clipboard_engine));
            }
            {
                let mut fe = cb_state.filedrop_engine.blocking_write();
                *fe = Some(Arc::clone(&filedrop_engine));
            }

            // 6. Setup system tray
            tray::setup_tray(app.handle())?;

            // 7. Start background tasks
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Start mesh node
                mesh_node.start().await;

                // Start clipboard monitor
                clipboard_engine.start_monitor().await;
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Cheeseboard");
}
```

---

## 12. Implementation Phases

### Phase 1: Scaffold + Tray + Mesh Connection

**Goal:** App launches as tray icon, connects to mesh, shows devices.

1. Scaffold Tauri v2 project at `p008/cheeseboard/`
2. Add truffle-core and truffle-tauri-plugin as path deps
3. Create `lib.rs` with plugin registration and setup
4. Implement `config/` module (load/save `config.json`)
5. Implement `tray/` module (basic tray icon with context menu)
6. Implement `state.rs` (CheeseboardState)
7. Wire up MeshNode initialization in setup
8. Create minimal tray popup frontend (`index.html` + `main.ts`)
9. Display device list from `truffle.devices()` guest-js call
10. Test: two machines on same Tailscale network see each other

**Deliverable:** Tray app shows connected devices.

### Phase 2: Clipboard Sync - Text

**Goal:** Copy text on one device, it appears on the other.

1. Add `arboard` dependency
2. Implement `clipboard/types.rs` (ClipboardEntry, ClipboardPayload, ClipboardFingerprint)
3. Implement `clipboard/echo.rs` (EchoGuard)
4. Implement `clipboard/store.rs` (ClipboardHistoryStore implementing SyncableStore)
5. Implement `clipboard/monitor.rs` (polling loop with `arboard`)
6. Implement `clipboard/mod.rs` (ClipboardSyncEngine)
7. Wire up StoreSyncAdapter with ClipboardHistoryStore — register as a syncable store
8. Subscribe to `clipboard` namespace on MessageBus
9. On change: add to ClipboardHistoryStore (triggers StoreSyncAdapter broadcast) + broadcast `clipboard:update` for instant OS clipboard sync
10. On receive: write to local clipboard via `arboard`, suppress echo
11. Add clipboard commands to `commands/clipboard.rs`
12. Show shared clipboard history (from StoreSyncAdapter) in tray popup

**Deliverable:** Text clipboard syncs between two devices. Clipboard history shared across all nodes.

### Phase 3: Clipboard Sync - Images + Polish

**Goal:** PNG images sync. Password manager content excluded.

1. Add image reading via `arboard::ImageData`
2. Encode PNG to base64 for `ClipboardPayload.content`
3. Enforce size limits (10 MB max for images)
4. Add macOS concealed type detection (`objc2-app-kit`)
5. Add clipboard history display with image thumbnails in tray popup
6. Add notifications via `tauri-plugin-notification`

**Deliverable:** Images sync, passwords excluded.

### Phase 4: File Drop

**Goal:** Send files between devices with accept/reject UI.

1. Implement `filedrop/mod.rs` (FileDropEngine wrapping `FileTransferAdapter`)
2. Implement `filedrop/orchestrator.rs` (event loop: offer/accept/reject/progress/complete)
3. Wire up the HTTP file transfer server (axum router from truffle-core)
4. Add file drop commands to `commands/filedrop.rs`
5. Create send file flow: tray menu "Send File..." -> `tauri-plugin-dialog` file picker -> transfer
6. Create receive flow: notification with accept/reject -> progress -> completion notification
7. Add transfer progress display in tray popup
8. Test: send a file from device A to device B

**Deliverable:** File transfer works end-to-end.

### Phase 5: Drop Zone + UX Polish

**Goal:** Drag-and-drop file sending, floating drop zone window.

1. Create `dropzone.html` with drag-drop UI
2. Create Tauri window config for drop zone (transparent, always-on-top)
3. Handle `tauri::DragDropEvent` or HTML5 drag events
4. Implement device picker (when dropping: "Send to which device?")
5. Add tray menu toggle for drop zone visibility

**Deliverable:** Drag files onto floating window to send.

### Phase 6: Mobile Web Access

**Goal:** Phone users can connect via QR code scan, share clipboard and files.

1. Implement `mobile/mod.rs` (MobileWebServer with axum)
2. Implement `mobile/routes.rs` (REST API for clipboard history, file upload, device list)
3. Implement `mobile/ws.rs` (WebSocket handler for real-time events)
4. Implement `mobile/qr.rs` (QR code SVG generation with auth token)
5. Build mobile web client (`src/mobile/` — vanilla HTML/CSS/JS, embedded at compile time)
6. Mobile web client: clipboard history view, text send, file upload, device picker
7. Add QR code display to tray popup (click to show/hide)
8. Add mobile server commands to `commands/mobile.rs`
9. Test: scan QR on phone, paste text from phone to desktop clipboard

**Deliverable:** Phone users can share clipboard and files via browser — no app install.

### Phase 7: Settings + Auto-Launch + Final Polish

**Goal:** Production-ready tray app.

1. Create `settings.html` with full settings UI
2. Implement all settings commands
3. Wire up `tauri-plugin-autostart` for launch at login
4. Add global shortcut (`Cmd+Shift+V` to toggle tray popup)
5. Add "received files" folder with configurable path
6. Tray icon state management (connected/syncing/disconnected)
7. Error handling and recovery (mesh reconnection, clipboard errors)
8. Cross-platform testing (macOS primary, Windows/Linux secondary)

**Deliverable:** Shippable v0.1.0.

---

## 13. Testing Strategy

### 13.1 Rust Unit Tests

- `clipboard/echo.rs` -- EchoGuard ring buffer: record, is_echo, TTL expiry
- `clipboard/types.rs` -- ClipboardFingerprint hashing, ClipboardPayload serde
- `config/types.rs` -- Config serde roundtrip, default values
- `filedrop/types.rs` -- FileDropEvent payloads

### 13.2 Integration Tests

- Two-device clipboard sync (requires two Tailscale-connected machines or mocked transport)
- File transfer end-to-end (can use loopback -- send to self)
- Config persistence (write, read back, verify)

### 13.3 Manual Testing Checklist

- Clipboard text sync: copy on A, paste on B
- Clipboard image sync: screenshot on A, paste on B
- Password manager detection: copy from 1Password, verify NOT synced
- File send: A picks file, B accepts, file arrives
- File reject: A picks file, B rejects, A gets notification
- Tray icon states: disconnect Tailscale, verify icon changes
- Auto-start: enable, reboot, verify app launches
- Settings: change output dir, verify new files go there
- Mobile web: scan QR, see clipboard history, send text to desktop
- Mobile web: upload file from phone, arrives on target desktop
- Store sync: copy on A, clipboard history shows on B within 1s
- Store sync: new device C joins mesh, gets full clipboard history

---

## 14. Cross-Platform Considerations

| Feature | macOS | Windows | Linux |
|---|---|---|---|
| Clipboard text | `arboard` | `arboard` | `arboard` (X11/Wayland) |
| Clipboard images | `arboard` | `arboard` | `arboard` (X11 only) |
| Password detection | `NSPasteboard` ConcealedType | Not standard | N/A |
| Tray icon | Template images | Regular icons | Regular icons |
| Auto-start | LaunchAgent | Registry | XDG autostart |
| Drop zone transparency | Full | Full | Depends on compositor |

Primary target is **macOS**. Windows and Linux are secondary and can be deferred.

---

## 15. Security Considerations

1. **No cloud dependency** -- all data stays on the Tailscale network
2. **Password exclusion** -- macOS ConcealedType detection prevents credential leakage
3. **File transfer tokens** -- 256-bit random tokens prevent unauthorized transfers
4. **Accept/reject flow** -- incoming files require explicit user approval (unless auto-accept configured)
5. **Path traversal protection** -- truffle-core's `validate_save_path()` prevents directory traversal attacks
6. **Tauri capabilities** -- least-privilege permissions per window
7. **CSP policy** -- strict content security policy in `tauri.conf.json`
8. **Size limits** -- clipboard (1 MB text, 10 MB image) and file transfer (4 GB) limits prevent DoS

---

## 16. Error Handling Patterns

```rust
// Consistent error type across commands
#[derive(Debug, thiserror::Error)]
pub enum CheeseboardError {
    #[error("Engine not initialized")]
    NotInitialized,
    #[error("Clipboard error: {0}")]
    Clipboard(String),
    #[error("File transfer error: {0}")]
    FileTransfer(String),
    #[error("Config error: {0}")]
    Config(String),
    #[error("Mesh error: {0}")]
    Mesh(String),
}

// Tauri commands return Result<T, String> (Tauri requirement)
impl From<CheeseboardError> for String {
    fn from(e: CheeseboardError) -> String {
        e.to_string()
    }
}
```

---

## 17. Key Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Frontend framework | Vanilla TS | Tray popup is tiny; no framework overhead |
| Clipboard crate | `arboard` (not tauri-plugin-clipboard) | Need polling loop + image support; tauri plugin is read/write only |
| Fingerprint hash | xxhash (xxh3) | 10x faster than SHA-256 for change detection; not security-critical |
| Clipboard transport | MessageBus JSON envelope | Text/base64 over existing mesh; simple, debuggable |
| File transport | truffle-core HTTP PUT | Already built with resume, integrity checks, progress |
| Image encoding | PNG base64 in JSON | Universal format; base64 penalty acceptable at 10 MB limit |
| Config format | JSON in app data dir | Human-readable, easy to debug, matches Tauri convention |
| Tray popup | Separate Tauri window (not native menu) | Richer UI: history list, progress bars, device status |
| Clipboard history | StoreSyncAdapter (not per-device) | One shared history across all mesh nodes; auto-replication; new devices get full history on join |
| Mobile access | Embedded web server + QR code (not native app) | No app install needed; avoids iOS clipboard restrictions; works on any phone with a browser |
| Mobile auth | URL-embedded token (not cookies/sessions) | Stateless; closing browser = disconnected; QR makes sharing the token frictionless |
