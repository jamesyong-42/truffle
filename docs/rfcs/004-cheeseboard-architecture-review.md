# Cheeseboard RFC-004: Architecture Review and Implementation Plan

## Part 1: Architecture Review

### 1.1 Correctness Issues (RFC vs Actual truffle-core APIs)

**ISSUE 1 (CRITICAL): MeshNode::new() constructor mismatch**

The RFC's `lib.rs` setup code (Section 11, line 1321-1336) shows:

```rust
let (mesh_node, event_rx) = MeshNode::new(
    MeshNodeConfig { ... },
    connection_manager,
);
```

But the actual `MeshNode::new()` at `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/mesh/node.rs` line 118-120 takes `Arc<ConnectionManager>`:

```rust
pub fn new(
    config: MeshNodeConfig,
    connection_manager: Arc<ConnectionManager>,
) -> (Self, mpsc::Receiver<MeshNodeEvent>)
```

The RFC creates a `connection_manager` out of thin air. The actual `ConnectionManager::new()` returns `(ConnectionManager, broadcast::Receiver<TransportEvent>)` and requires a `TransportConfig`. Furthermore, the RFC omits the entire Go sidecar lifecycle -- `GoShim`, `BridgeManager`, and how `ConnectionManager` gets its streams from the bridge. This is not a small omission; it is the entire transport stack.

**ISSUE 2 (CRITICAL): MeshNode event_rx is single-consumer, but RFC ignores it**

`MeshNode::new()` returns `mpsc::Receiver<MeshNodeEvent>` -- a single-consumer channel. The RFC's setup code captures `event_rx` but never shows who consumes it. This is the sole pathway for receiving:
- Application-level mesh messages (`MeshNodeEvent::Message`)
- Device discovery/offline events
- Auth required events

Without consuming `event_rx`, clipboard sync messages from remote devices will never be delivered. The channel will fill up (capacity 256) and then `try_send` will fail silently, breaking all mesh communication.

**ISSUE 3 (CRITICAL): MessageBus is disconnected from incoming messages**

The RFC (Section 3.6, 3.8) implies that subscribing to the `clipboard` namespace on the `MessageBus` will receive incoming clipboard messages. But examining the actual code:

1. `MeshNode` creates a `MeshMessageBus` internally (line 150)
2. `MeshNode` exposes it via `message_bus()` (line 388-390)
3. `MeshNode` NEVER calls `message_bus.dispatch()` -- incoming messages go to `event_tx` as `MeshNodeEvent::Message`, not to the MessageBus

The `MeshMessageBus` is designed as a pub/sub layer, but it only works if someone actively dispatches to it. In the NAPI layer, messages go to JS callbacks, and JS code calls `MessageBus.dispatch()`. In Cheeseboard (pure Rust, no JS in the hot path), someone must bridge `event_rx` to `message_bus.dispatch()`. The RFC does not show this.

**ISSUE 4 (MODERATE): SyncableStore trait uses &self, not &mut self**

The RFC's `ClipboardHistoryStore` implements `SyncableStore`. The actual trait at `adapter.rs` line 13-28 uses `&self` for all methods including `apply_remote_slice(&self, slice: DeviceSlice)`. The RFC's implementation uses `RwLock` fields, which is correct for interior mutability, but `get_local_slice(&self)` uses `blocking_read()`:

```rust
fn get_local_slice(&self) -> Option<DeviceSlice> {
    let entries = self.local_entries.blocking_read();
```

This is unsafe if called from within a tokio runtime context. The `StoreSyncAdapter` calls `get_local_slice()` from `broadcast_store_full()` which is called from `broadcast_all_stores()` which is an async method. Using `blocking_read()` inside a tokio context will panic or deadlock.

**ISSUE 5 (MODERATE): StoreSyncAdapter does NOT auto-detect local changes**

The RFC (Section 3.5, flow step 4) states: "StoreSyncAdapter detects the store changed, broadcasts store:sync:update to all mesh nodes." But the actual `StoreSyncAdapter` does NOT poll or watch stores. It has `handle_local_changed(store_id, slice)` which must be called explicitly by the application. The RFC hand-waves this as automatic, but Cheeseboard must call `store_sync_adapter.handle_local_changed("clipboard-history", &slice)` after every `add_local_entry()`.

**ISSUE 6 (MODERATE): StoreSyncAdapter outgoing_tx is not wired to MessageBus/MeshNode**

`StoreSyncAdapter::new()` takes an `mpsc::UnboundedSender<OutgoingSyncMessage>`. Someone must receive from the corresponding `UnboundedReceiver` and turn `OutgoingSyncMessage` into `MeshEnvelope` broadcasts via `MeshNode::broadcast_envelope()`. The RFC does not show this wiring.

**ISSUE 7 (MINOR): FileTransferAdapter::handle_bus_message takes &str payload, not serde_json::Value**

The RFC's `FileDropEngine` will need to convert between the `IncomingMeshMessage` (which has `payload: serde_json::Value`) and `FileTransferAdapter::handle_bus_message(msg_type: &str, payload: &str)` which expects a JSON string. This is a minor API mismatch but will cause compilation errors if not handled.

**ISSUE 8 (MINOR): FileTransferAdapter::new() requires DialFn**

The `FileTransferAdapterConfig` requires a `DialFn: Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = io::Result<TcpStream>> + Send>> + Send + Sync>`. This is the function that dials through Tailscale (via the Go shim bridge). The RFC does not address how to obtain this function. It comes from `GoShim::dial()` in the bridge layer.

### 1.2 Missing Pieces

**MISSING 1: The entire Go sidecar lifecycle**

Cheeseboard needs the Go sidecar (`sidecar-slim`) for Tailscale connectivity. The sidecar provides:
- `tsnet` embedded Tailscale node
- TCP bridge for WebSocket connections and file transfer HTTP streams
- Peer discovery events (list of Tailscale peers)

The RFC mentions Tailscale but never discusses spawning the sidecar, managing its lifecycle, or handling auth flow. The sidecar binary path, state directory, session token, and bridge port must all be configured and managed.

Options:
1. Cheeseboard spawns its own sidecar (separate tsnet node, separate hostname) -- simpler but uses an additional Tailscale device slot
2. Share a sidecar with vibe-ctl if running -- complex, requires IPC coordination
3. Use Tailscale CLI directly instead of tsnet -- loses the embedded approach but avoids the sidecar entirely (requires user to have Tailscale installed)

For Cheeseboard, option 1 is correct: it spawns its own `sidecar-slim` with hostname prefix "cheeseboard".

**MISSING 2: Event dispatch bridge (event_rx -> MessageBus + StoreSyncAdapter + app events)**

The RFC needs a central event dispatch loop that:
1. Consumes `event_rx: mpsc::Receiver<MeshNodeEvent>`
2. For `MeshNodeEvent::Message(msg)`:
   - If `msg.namespace == "clipboard"`: dispatch to clipboard sync handler
   - If `msg.namespace == "file-transfer"`: dispatch to FileTransferAdapter
   - If `msg.namespace == "sync"`: dispatch to StoreSyncAdapter
3. For `MeshNodeEvent::DeviceDiscovered`: call `store_sync_adapter.handle_device_discovered()`
4. For `MeshNodeEvent::DeviceOffline`: call `store_sync_adapter.handle_device_offline()`
5. For all events: emit to Tauri frontend via `app.emit()`

**MISSING 3: StoreSyncAdapter outgoing message pump**

A task that:
1. Reads from `outgoing_rx: mpsc::UnboundedReceiver<OutgoingSyncMessage>`
2. Wraps each message as `MeshEnvelope { namespace: "sync", msg_type, payload }`
3. Calls `mesh_node.broadcast_envelope(&envelope)`

**MISSING 4: FileTransferAdapter outgoing message pump**

Similarly, `FileTransferAdapter` sends messages via `bus_tx: mpsc::UnboundedSender<(String, String, String)>` -- a tuple of (target_device_id, msg_type, payload_json). A pump task must:
1. Read from the corresponding receiver
2. Wrap as `MeshEnvelope { namespace: "file-transfer", msg_type, payload }`
3. Call `mesh_node.send_envelope(target_device_id, &envelope)`

**MISSING 5: BridgeManager and ConnectionManager setup**

Before `MeshNode` can communicate, the following must be set up:
1. `BridgeManager::bind(session_token)` -- listens on localhost, returns ephemeral port
2. Register bridge handlers for WebSocket (mesh transport) and HTTP (file transfer)
3. `ConnectionManager::new(transport_config)` -- manages WebSocket connections
4. Wire `BridgeManager` to `ConnectionManager` so incoming bridge connections become WebSocket connections
5. Pass `bridge_port` to `GoShim` so it knows where to proxy

**MISSING 6: Window creation for settings and dropzone**

The RFC shows `tauri.conf.json` config for the `tray-popup` window but only mentions settings and dropzone windows in passing. Tauri v2 can create windows on demand via `WebviewWindowBuilder`. The settings and dropzone windows should NOT be defined in `tauri.conf.json` (they would be created hidden at startup). Instead, create them on demand when the user clicks "Settings..." or "Show Drop Zone".

**MISSING 7: Tray popup positioning**

The RFC shows `position_at_tray(app, &window)` but doesn't implement it. On macOS, getting the tray icon position requires `tauri::tray::TrayIcon::rect()` which is available in Tauri v2. This requires the `"unstable"` feature flag on the `tauri` crate in some versions.

### 1.3 Design Concerns

**CONCERN 1: Dual clipboard sync (MessageBus broadcast + StoreSyncAdapter) is redundant and inconsistent**

The RFC proposes TWO mechanisms for clipboard sync:
- **Instant sync**: broadcast `clipboard:update` via MessageBus for immediate OS clipboard write on remote devices
- **History sync**: StoreSyncAdapter replicates the `ClipboardHistoryStore` for searchable history

This means every clipboard change generates TWO broadcasts over the mesh. The instant broadcast carries the full content in the `clipboard:update` payload. The StoreSyncAdapter also broadcasts the full content in the `store:sync:update` payload. This doubles bandwidth for every clipboard operation.

**Fix**: Use a single mechanism. The `clipboard:update` broadcast should be the sole transport for instant clipboard sync. The StoreSyncAdapter handles history replication. When a `clipboard:update` arrives:
1. Write to OS clipboard immediately (instant sync)
2. Add to local `ClipboardHistoryStore` (which the StoreSyncAdapter will then replicate to other nodes for history purposes)

But wait -- the StoreSyncAdapter's replication means the entry would appear twice on remote nodes (once from the instant broadcast, once from the store sync). The fix: the instant `clipboard:update` broadcast is the authority. The StoreSyncAdapter is used ONLY for history catch-up (when a device joins the mesh or reconnects). The instant broadcast handler adds to the local store AND writes to the OS clipboard. The StoreSyncAdapter merge logic deduplicates by fingerprint.

**CONCERN 2: arboard::Clipboard::new() in spawn_blocking is wasteful**

`arboard::Clipboard::new()` on macOS creates an `NSPasteboard` reference. Creating and dropping this every 500ms is unnecessary overhead. The `arboard::Clipboard` object is not thread-safe (it's `!Send` on some platforms), so it cannot be stored in an `Arc`. But it can be stored in a dedicated thread.

**Fix**: Spawn a dedicated OS thread (not a tokio task) with `std::thread::spawn` that holds a persistent `arboard::Clipboard` instance. The clipboard monitor communicates with this thread via a channel. This avoids creating/destroying the clipboard object every poll cycle and avoids `spawn_blocking` overhead.

**CONCERN 3: blocking_read() in SyncableStore**

As noted in Issue 4, `blocking_read()` inside an async context will deadlock. The `SyncableStore` trait methods use `&self` (not async), so they cannot `.await`. But the `StoreSyncAdapter` calls these methods from async contexts.

**Fix**: Use `std::sync::RwLock` (not `tokio::sync::RwLock`) for the fields accessed in `SyncableStore` methods. `std::sync::RwLock::read()` will not deadlock in a tokio context (it blocks the current thread briefly, which is acceptable for the small critical section of reading clipboard history). This is exactly what the `MockStore` in the test suite does (it uses `Mutex<>` from `std::sync`).

**CONCERN 4: Mobile web server auth token in URL**

Token in the URL query parameter (`?token=<256-bit-token>`) can be logged by:
- Browser history
- Proxy servers (though traffic is over Tailscale, which is WireGuard-encrypted point-to-point)
- iOS Safari URL bar sharing
- Screen recordings/screenshots

Since traffic goes over Tailscale (already encrypted), the primary risk is local leakage. This is acceptable for v1 given the constraints (no cookies/sessions for zero-friction QR flow).

**Mitigation**: After initial WebSocket connection, exchange a short-lived session token via the WebSocket itself, and use that for subsequent REST API calls. The initial QR token becomes single-use.

**CONCERN 5: Tray popup as a separate Tauri window**

On macOS, a separate window for the tray popup has these issues:
- It appears in Mission Control unless configured correctly
- It may steal focus from the active app
- Positioning relative to the tray icon is imprecise across display configurations
- The window decorations need to match the system vibrancy

Tauri v2 handles most of these via the window config options (skipTaskbar, alwaysOnTop, decorations:false, transparent:true). The approach is correct for a rich tray popup; a native NSMenu would be too limited for the desired UI (clipboard history, progress bars, device status).

### 1.4 Missing Error Handling

1. **Sidecar crash recovery**: If the Go sidecar crashes, all mesh connectivity is lost. `GoShim` has auto-restart, but the `MeshNode` and `ConnectionManager` state must be reconciled after restart. The RFC does not address recovery.

2. **arboard failures**: `arboard::Clipboard::new()` can fail on headless systems or when the display server is unavailable (Linux). The polling loop should handle this gracefully rather than panicking.

3. **StoreSyncAdapter outgoing channel full**: The `UnboundedSender` can grow unboundedly if the MeshNode is offline. Should use a bounded channel or drop old messages.

4. **File transfer HTTP server not addressed**: The RFC mentions `FileTransferManager` but does not show who starts the HTTP receiver server. In truffle-core, the file transfer receiver is an axum route handler that must be mounted on a server listening on the bridge. The receiver's HTTP server and the mobile web server are separate concerns.

5. **Tray popup window focus loss**: When the user clicks outside the tray popup, it should hide. The RFC does not handle the `blur` event on the popup window.

### 1.5 Performance Concerns

1. **Base64 encoding 10MB images**: Encoding a 10MB PNG to base64 produces ~13.3MB of text, which must be serialized into a JSON payload, sent over WebSocket, and parsed on the other end. This is acceptable at the 10MB limit but wasteful. Consider using the file transfer mechanism for images above 1MB.

2. **QR code regeneration**: The RFC generates QR SVG on demand via a Tauri command. This is fine -- QR generation for a ~100 character URL takes <1ms.

3. **Clipboard history merge on every remote update**: `apply_remote_slice` must rebuild the merged history. With 50 entries per device and ~5 devices, this is 250 entries -- trivial.

4. **500ms polling interval**: On macOS, `NSPasteboard.changeCount` is extremely cheap to read. 250ms polling would feel more responsive with negligible cost. The RFC mentions 500ms but the product design suggests 250ms.

---

## Part 2: Revised Architecture

### Fix 1: Central Event Dispatch Loop

Create a `MeshEventRouter` that consumes `event_rx` and dispatches to all subsystems.

```rust
// src-tauri/src/mesh_router.rs

pub struct MeshEventRouter {
    clipboard_engine: Arc<ClipboardSyncEngine>,
    file_transfer_adapter: Arc<FileTransferAdapter>,
    store_sync_adapter: Arc<StoreSyncAdapter>,
    app_handle: AppHandle,
}

impl MeshEventRouter {
    pub async fn run(self, mut event_rx: mpsc::Receiver<MeshNodeEvent>) {
        while let Some(event) = event_rx.recv().await {
            match &event {
                MeshNodeEvent::Message(msg) => {
                    match msg.namespace.as_str() {
                        "clipboard" => {
                            self.clipboard_engine.handle_remote_message(msg).await;
                        }
                        "file-transfer" => {
                            let payload_str = serde_json::to_string(&msg.payload)
                                .unwrap_or_default();
                            self.file_transfer_adapter
                                .handle_bus_message(&msg.msg_type, &payload_str)
                                .await;
                        }
                        "sync" => {
                            let sync_msg = SyncMessage {
                                from: msg.from.clone(),
                                msg_type: msg.msg_type.clone(),
                                payload: msg.payload.clone(),
                            };
                            self.store_sync_adapter
                                .handle_sync_message(&sync_msg)
                                .await;
                        }
                        _ => {}
                    }
                }
                MeshNodeEvent::DeviceDiscovered(device) => {
                    self.store_sync_adapter
                        .handle_device_discovered(&device.id)
                        .await;
                }
                MeshNodeEvent::DeviceOffline(id) => {
                    self.store_sync_adapter
                        .handle_device_offline(id)
                        .await;
                }
                _ => {}
            }
            // Emit all events to frontend
            emit_cheeseboard_event(&self.app_handle, &event);
        }
    }
}
```

Location: `cheeseboard/src-tauri/src/mesh_router.rs`

### Fix 2: Outgoing Message Pumps

Two background tasks that bridge outgoing channels to `MeshNode::broadcast_envelope()` / `send_envelope()`.

```rust
// src-tauri/src/pumps.rs

pub async fn store_sync_pump(
    mut rx: mpsc::UnboundedReceiver<OutgoingSyncMessage>,
    mesh_node: Arc<MeshNode>,
) {
    while let Some(msg) = rx.recv().await {
        let envelope = MeshEnvelope::new("sync", msg.msg_type, msg.payload);
        mesh_node.broadcast_envelope(&envelope).await;
    }
}

pub async fn file_transfer_pump(
    mut rx: mpsc::UnboundedReceiver<(String, String, String)>,
    mesh_node: Arc<MeshNode>,
) {
    while let Some((target_device_id, msg_type, payload_json)) = rx.recv().await {
        let payload: serde_json::Value = serde_json::from_str(&payload_json)
            .unwrap_or(serde_json::Value::Null);
        let envelope = MeshEnvelope::new("file-transfer", msg_type, payload);
        mesh_node.send_envelope(&target_device_id, &envelope).await;
    }
}
```

Location: `cheeseboard/src-tauri/src/pumps.rs`

### Fix 3: Dedicated Clipboard Thread

```rust
// src-tauri/src/clipboard/thread.rs

pub enum ClipboardCommand {
    Read(oneshot::Sender<Result<ClipboardContent, String>>),
    Write(ClipboardContent, oneshot::Sender<Result<(), String>>),
    Shutdown,
}

pub fn spawn_clipboard_thread() -> mpsc::Sender<ClipboardCommand> {
    let (tx, mut rx) = mpsc::channel::<ClipboardCommand>(32);
    std::thread::spawn(move || {
        let mut clipboard = arboard::Clipboard::new()
            .expect("Failed to initialize clipboard");
        while let Some(cmd) = rx.blocking_recv() {
            match cmd {
                ClipboardCommand::Read(reply) => {
                    let result = read_clipboard(&mut clipboard);
                    let _ = reply.send(result);
                }
                ClipboardCommand::Write(content, reply) => {
                    let result = write_clipboard(&mut clipboard, &content);
                    let _ = reply.send(result);
                }
                ClipboardCommand::Shutdown => break,
            }
        }
    });
    tx
}
```

Location: `cheeseboard/src-tauri/src/clipboard/thread.rs`

### Fix 4: Use std::sync::RwLock for SyncableStore Fields

```rust
// src-tauri/src/clipboard/store.rs

pub struct ClipboardHistoryStore {
    store_id: String,
    local_device_id: String,
    // Use std::sync::RwLock, NOT tokio::sync::RwLock
    local_entries: std::sync::RwLock<VecDeque<ClipboardEntry>>,
    merged_history: std::sync::RwLock<Vec<ClipboardEntry>>,
    max_per_device: usize,
    max_total: usize,
    // Notify channel for local changes -> StoreSyncAdapter
    change_tx: mpsc::UnboundedSender<()>,
}

impl SyncableStore for ClipboardHistoryStore {
    fn get_local_slice(&self) -> Option<DeviceSlice> {
        let entries = self.local_entries.read().unwrap(); // std::sync is safe here
        // ...
    }
    fn apply_remote_slice(&self, slice: DeviceSlice) {
        let mut merged = self.merged_history.write().unwrap();
        // ...
    }
    // ...
}
```

### Fix 5: Go Sidecar Lifecycle Management

```rust
// src-tauri/src/sidecar.rs

pub struct SidecarManager {
    shim: Arc<GoShim>,
    bridge_manager: Arc<BridgeManager>,
}

impl SidecarManager {
    pub async fn start(
        app_handle: &AppHandle,
        config: &CheeseboardConfig,
    ) -> Result<(Self, Arc<ConnectionManager>), Box<dyn std::error::Error>> {
        // 1. Generate session token
        let session_token = generate_session_token();
        
        // 2. Create BridgeManager (listen on localhost)
        let bridge_manager = BridgeManager::bind(session_token).await?;
        let bridge_port = bridge_manager.local_port();
        
        // 3. Create ConnectionManager
        let transport_config = TransportConfig::default();
        let (connection_manager, _transport_rx) = ConnectionManager::new(transport_config);
        let connection_manager = Arc::new(connection_manager);
        
        // 4. Wire bridge -> connection_manager
        // (register WebSocket handler on bridge)
        
        // 5. Resolve sidecar binary path
        let binary_path = app_handle
            .path()
            .resource_dir()
            .unwrap()
            .join("sidecar-slim");
        
        // 6. Spawn GoShim
        let shim_config = ShimConfig {
            binary_path,
            hostname: format!("{}-{}", config.hostname_prefix, 
                             &config.device_id[..8]),
            state_dir: app_handle.path().app_data_dir()
                .unwrap()
                .join("tsnet-state")
                .to_string_lossy()
                .to_string(),
            auth_key: None,
            bridge_port,
            session_token: hex::encode(&session_token),
            auto_restart: true,
        };
        let shim = GoShim::spawn(shim_config).await?;
        
        Ok((Self {
            shim: Arc::new(shim),
            bridge_manager: Arc::new(bridge_manager),
        }, connection_manager))
    }
}
```

Location: `cheeseboard/src-tauri/src/sidecar.rs`

---

## Part 3: Implementation Plan

### Phase 1: Project Scaffold + Tauri Setup + MeshNode Integration + Basic Tray

**Goal**: App launches as tray icon, spawns Go sidecar, connects to mesh, shows devices.

**Files to create**:

```
cheeseboard/
  package.json
  vite.config.ts
  tsconfig.json
  src/
    index.html                     -- Tray popup (minimal: device list only)
    styles/global.css              -- CSS variables, reset
    styles/tray.css                -- Tray popup styles
    scripts/main.ts                -- Tray popup logic (fetch devices, listen events)
    scripts/types.ts               -- TypeScript types
  src-tauri/
    Cargo.toml
    build.rs
    tauri.conf.json
    capabilities/default.json
    icons/                         -- App icons (generate via tauri icon)
    icons/tray/                    -- Tray state icons
    src/
      main.rs                      -- #![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
      lib.rs                       -- Tauri builder, plugin registration, setup()
      state.rs                     -- CheeseboardState
      error.rs                     -- CheeseboardError enum
      sidecar.rs                   -- SidecarManager (GoShim + BridgeManager lifecycle)
      mesh_router.rs               -- MeshEventRouter (event_rx dispatcher)
      pumps.rs                     -- Outgoing message pump tasks
      config/
        mod.rs                     -- load_or_create(), save()
        types.rs                   -- CheeseboardConfig struct
      tray/
        mod.rs                     -- setup_tray(), build_tray_menu()
        icons.rs                   -- Icon state management
      commands/
        mod.rs                     -- Re-exports
        mesh.rs                    -- cheeseboard_get_devices, cheeseboard_get_connection_status, cheeseboard_get_device_id
```

**Key structs**:

```rust
// state.rs
pub struct CheeseboardState {
    pub mesh_node: RwLock<Option<Arc<MeshNode>>>,
    pub sidecar: RwLock<Option<SidecarManager>>,
    pub config: RwLock<CheeseboardConfig>,
    pub connection_status: RwLock<ConnectionStatus>,
}

// config/types.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheeseboardConfig {
    pub device_id: String,
    pub device_name: String,
    pub clipboard_sync_enabled: bool,
    pub clipboard_poll_interval_ms: u64,
    pub max_clipboard_history: usize,
    pub sync_images: bool,
    pub max_image_size: usize,
    pub file_output_dir: String,
    pub auto_accept_from: Vec<String>,
    pub notifications_enabled: bool,
    pub auto_start: bool,
    pub mesh_port: u16,
    pub hostname_prefix: String,
}

// error.rs
#[derive(Debug, thiserror::Error)]
pub enum CheeseboardError {
    #[error("Engine not initialized")]
    NotInitialized,
    #[error("Sidecar error: {0}")]
    Sidecar(String),
    #[error("Mesh error: {0}")]
    Mesh(String),
    #[error("Config error: {0}")]
    Config(String),
}

impl From<CheeseboardError> for String {
    fn from(e: CheeseboardError) -> String { e.to_string() }
}
```

**Integration wiring (lib.rs setup)**:

```rust
.setup(|app| {
    // 1. Load config
    let config = config::load_or_create(app.handle())?;
    
    // 2. Start sidecar (GoShim + BridgeManager)
    let app_handle = app.handle().clone();
    let config_clone = config.clone();
    
    tauri::async_runtime::spawn(async move {
        // 2a. Start sidecar, get ConnectionManager
        let (sidecar, connection_manager) = 
            SidecarManager::start(&app_handle, &config_clone).await
                .expect("Failed to start sidecar");
        
        // 2b. Create MeshNode
        let mesh_config = MeshNodeConfig {
            device_id: config_clone.device_id.clone(),
            device_name: config_clone.device_name.clone(),
            device_type: "desktop".to_string(),
            hostname_prefix: config_clone.hostname_prefix.clone(),
            prefer_primary: false,
            capabilities: vec![
                "clipboard-sync".to_string(),
                "file-transfer".to_string(),
            ],
            metadata: None,
            timing: MeshTimingConfig::default(),
        };
        let (mesh_node, event_rx) = MeshNode::new(mesh_config, connection_manager);
        let mesh_node = Arc::new(mesh_node);
        
        // 2c. Store in state
        let cb_state = app_handle.state::<CheeseboardState>();
        *cb_state.mesh_node.write().await = Some(Arc::clone(&mesh_node));
        *cb_state.sidecar.write().await = Some(sidecar);
        
        // 2d. Start mesh node
        mesh_node.start().await;
        
        // 2e. Start minimal event router (Phase 1: just emit to frontend)
        let router = MeshEventRouter::new_minimal(app_handle.clone());
        tokio::spawn(router.run(event_rx));
    });
    
    // 3. Setup tray
    tray::setup_tray(app.handle())?;
    
    // 4. Manage state
    app.manage(CheeseboardState::default());
    
    Ok(())
})
```

**Dependencies (Cargo.toml)**:

```toml
[dependencies]
truffle-core = { path = "../../truffle/crates/truffle-core" }
tauri = { version = "2", features = [] }
tokio = { version = "1", features = ["rt-multi-thread", "sync", "macros", "time", "fs"] }
tokio-util = { version = "0.7", features = ["sync"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
uuid = { version = "1", features = ["v4"] }
hostname = "0.4"
directories = "5"
thiserror = "2"
hex = "0.4"
```

Note: `truffle-tauri-plugin` is NOT used as a dependency. Cheeseboard creates its own `MeshNode` directly from `truffle-core`. Using `truffle-tauri-plugin` would be redundant and would create conflicting state management (two sets of `TruffleState`). The existing plugin is a reference for patterns, not a runtime dependency.

**Verification steps**:
1. `cargo tauri dev` launches tray app
2. Tray icon appears in macOS menu bar
3. Right-click tray shows context menu with device list (empty initially)
4. Sidecar starts successfully (check logs for `GoShim spawned`)
5. If another Cheeseboard/truffle device is on the same Tailscale network, it appears in the device list
6. Frontend receives `cheeseboard://device-discovered` events

**Estimated LOC**: ~1200 Rust, ~200 TypeScript, ~100 CSS, ~150 config (JSON/TOML)

---

### Phase 2: Clipboard Sync (Text) + StoreSyncAdapter-Backed History

**Goal**: Copy text on Device A, it appears on Device B's clipboard and in shared history.

**Files to create**:

```
src-tauri/src/
  clipboard/
    mod.rs                  -- ClipboardSyncEngine
    thread.rs               -- Dedicated clipboard OS thread
    monitor.rs              -- Polling loop (async, communicates with thread)
    echo.rs                 -- EchoGuard
    store.rs                -- ClipboardHistoryStore (impl SyncableStore)
    types.rs                -- ClipboardEntry, ClipboardPayload, ClipboardFingerprint, ContentType
  commands/
    clipboard.rs            -- Clipboard Tauri commands
```

**Files to modify**:
- `src-tauri/src/lib.rs` -- Add clipboard engine init, StoreSyncAdapter init, pump tasks
- `src-tauri/src/state.rs` -- Add clipboard_engine, clipboard_history, store_sync fields
- `src-tauri/src/mesh_router.rs` -- Add clipboard + sync namespace dispatch
- `src-tauri/src/pumps.rs` -- Add store_sync_pump
- `src-tauri/Cargo.toml` -- Add arboard, xxhash-rust
- `src/scripts/main.ts` -- Show clipboard history in tray popup
- `src/index.html` -- Add clipboard history section

**Key structs**:

```rust
// clipboard/types.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardEntry {
    pub id: String,
    pub content_type: ContentType,
    pub content: String,           // text content or base64 image
    pub fingerprint: String,       // xxh3 hash hex
    pub timestamp: u64,
    pub device_id: String,
    pub device_name: String,
    pub pinned: bool,
    pub preview: String,           // first 80 chars
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Text,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClipboardFingerprint(pub u64);

impl ClipboardFingerprint {
    pub fn from_bytes(data: &[u8]) -> Self {
        Self(xxhash_rust::xxh3::xxh3_64(data))
    }
    pub fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardPayload {
    pub device_id: String,
    pub device_name: String,
    pub content_type: String,
    pub content: String,
    pub fingerprint: String,
    pub size: usize,
    pub timestamp: u64,
}

// clipboard/echo.rs
pub struct EchoGuard {
    recent_writes: std::sync::Mutex<VecDeque<(ClipboardFingerprint, Instant)>>,
    ttl: Duration,
    capacity: usize,
}

// clipboard/store.rs
pub struct ClipboardHistoryStore {
    store_id: String,
    local_device_id: String,
    local_entries: std::sync::RwLock<VecDeque<ClipboardEntry>>,
    merged_history: std::sync::RwLock<Vec<ClipboardEntry>>,
    max_per_device: usize,
    max_total: usize,
    change_notify: mpsc::UnboundedSender<()>,
}

// clipboard/mod.rs
pub struct ClipboardSyncEngine {
    device_id: String,
    device_name: String,
    mesh_node: Arc<MeshNode>,
    echo_guard: EchoGuard,
    last_fingerprint: std::sync::RwLock<Option<ClipboardFingerprint>>,
    history_store: Arc<ClipboardHistoryStore>,
    enabled: std::sync::RwLock<bool>,
    clipboard_tx: mpsc::Sender<ClipboardCommand>,
    shutdown: CancellationToken,
    app_handle: AppHandle,
    poll_interval: Duration,
}
```

**Integration wiring**:

The setup in `lib.rs` (inside the async spawn block) expands to:

```rust
// After MeshNode is started:

// 3. Create clipboard thread
let clipboard_tx = clipboard::thread::spawn_clipboard_thread();

// 4. Create ClipboardHistoryStore
let (change_notify_tx, mut change_notify_rx) = mpsc::unbounded_channel();
let clipboard_history = Arc::new(ClipboardHistoryStore::new(
    "clipboard-history",
    config.device_id.clone(),
    config.max_clipboard_history,
    change_notify_tx,
));

// 5. Create StoreSyncAdapter
let (sync_outgoing_tx, sync_outgoing_rx) = mpsc::unbounded_channel();
let store_sync = StoreSyncAdapter::new(
    StoreSyncAdapterConfig {
        local_device_id: config.device_id.clone(),
    },
    vec![clipboard_history.clone() as Arc<dyn SyncableStore>],
    sync_outgoing_tx,
);

// 6. Start StoreSyncAdapter
store_sync.start().await;

// 7. Start store sync outgoing pump
let mesh_for_pump = Arc::clone(&mesh_node);
tokio::spawn(pumps::store_sync_pump(sync_outgoing_rx, mesh_for_pump));

// 8. Start store change notification loop
let store_sync_for_changes = Arc::clone(&store_sync);
let history_for_changes = Arc::clone(&clipboard_history);
tokio::spawn(async move {
    while change_notify_rx.recv().await.is_some() {
        if let Some(slice) = history_for_changes.get_local_slice() {
            store_sync_for_changes.handle_local_changed("clipboard-history", &slice).await;
        }
    }
});

// 9. Create ClipboardSyncEngine
let clipboard_engine = Arc::new(ClipboardSyncEngine::new(
    config.device_id.clone(),
    config.device_name.clone(),
    Arc::clone(&mesh_node),
    clipboard_history.clone(),
    clipboard_tx,
    app_handle.clone(),
    Duration::from_millis(config.clipboard_poll_interval_ms),
));

// 10. Start clipboard monitor
let engine_for_monitor = Arc::clone(&clipboard_engine);
tokio::spawn(async move {
    engine_for_monitor.start_monitor().await;
});

// 11. Update event router to dispatch clipboard + sync messages
let router = MeshEventRouter::new(
    clipboard_engine.clone(),
    store_sync.clone(),
    app_handle.clone(),
);
tokio::spawn(router.run(event_rx));
```

**Verification steps**:
1. Copy text on Device A
2. Within 500ms, text appears on Device B's OS clipboard (paste to verify)
3. Clipboard history in tray popup shows the entry with source device indicator
4. Copy on Device B -> appears on Device A and in both devices' history
5. No echo: copying does not trigger an infinite loop
6. Password manager content (test with 1Password) is NOT synced

**Estimated LOC**: ~1000 Rust, ~150 TypeScript

---

### Phase 3: Clipboard Sync (Images) + Password Detection + Clipboard Palette UI

**Goal**: PNG images sync. Password manager content excluded. `Cmd+Shift+V` opens clipboard palette.

**Files to create**:

```
src/
  clipboard-palette.html           -- Floating clipboard palette window
  styles/palette.css               -- Palette styles
  scripts/palette.ts               -- Palette logic
```

**Files to modify**:
- `src-tauri/src/clipboard/thread.rs` -- Add image read/write support
- `src-tauri/src/clipboard/monitor.rs` -- Add image change detection
- `src-tauri/src/clipboard/mod.rs` -- Add image broadcasting, size limits
- `src-tauri/src/clipboard/types.rs` -- Image support in ClipboardContent
- `src-tauri/src/lib.rs` -- Register global shortcut, add palette window creation
- `src-tauri/src/commands/clipboard.rs` -- Add `cheeseboard_copy_history_entry`
- `src-tauri/Cargo.toml` -- Add base64, image, objc2 (macOS)

**Key additions**:

```rust
// clipboard/thread.rs - Enhanced to handle images
pub enum ClipboardContent {
    Text(String),
    Image(ImageData),  // arboard::ImageData
    Empty,
}

// clipboard/monitor.rs - macOS concealed type detection
#[cfg(target_os = "macos")]
fn is_concealed() -> bool {
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;
    
    unsafe {
        let pasteboard = NSPasteboard::generalPasteboard();
        let concealed_type = NSString::from_str("org.nspasteboard.ConcealedType");
        let types = pasteboard.types();
        types.map_or(false, |t| t.containsObject(&concealed_type))
    }
}
```

**Dependencies to add**:
```toml
base64 = "0.22"
image = { version = "0.25", default-features = false, features = ["png"] }
tauri-plugin-global-shortcut = "2"

[target.'cfg(target_os = "macos")'.dependencies]
objc2 = "0.6"
objc2-app-kit = { version = "0.3", features = ["NSPasteboard"] }
objc2-foundation = { version = "0.3", features = ["NSString", "NSArray"] }
```

**Verification steps**:
1. Take screenshot on Device A -> paste on Device B works
2. Image > 10MB shows "Use file drop instead" notification
3. Copy from 1Password -> NOT synced (verify in logs)
4. `Cmd+Shift+V` opens clipboard palette with history from all devices
5. Click entry in palette -> writes to OS clipboard

**Estimated LOC**: ~400 Rust, ~300 TypeScript, ~150 CSS

---

### Phase 4: File Drop (Send + Receive + Progress)

**Goal**: Send files between devices with accept/reject UI and progress.

**Files to create**:

```
src-tauri/src/
  filedrop/
    mod.rs                         -- FileDropEngine
    orchestrator.rs                -- Event loop for adapter events
    types.rs                       -- FileDropEvent payloads
  commands/
    filedrop.rs                    -- File drop Tauri commands
```

**Files to modify**:
- `src-tauri/src/lib.rs` -- Add FileDropEngine init, adapter event pump, file transfer pump
- `src-tauri/src/state.rs` -- Add filedrop_engine field
- `src-tauri/src/mesh_router.rs` -- Add file-transfer namespace dispatch
- `src-tauri/src/pumps.rs` -- Add file_transfer_pump
- `src-tauri/src/tray/mod.rs` -- Add "Send File..." menu item
- `src/scripts/main.ts` -- Add transfer progress display
- `src-tauri/Cargo.toml` -- Add tauri-plugin-notification, tauri-plugin-dialog

**Key structs**:

```rust
// filedrop/mod.rs
pub struct FileDropEngine {
    adapter: Arc<FileTransferAdapter>,
    pending_offers: std::sync::RwLock<HashMap<String, FileTransferOffer>>,
    app_handle: AppHandle,
    output_dir: std::sync::RwLock<String>,
}

impl FileDropEngine {
    pub async fn send_file(&self, target_device_id: &str, file_path: &str) -> Result<String, String> {
        self.adapter.send_file(target_device_id, file_path).await;
        // adapter returns transfer_id
    }
    
    pub async fn accept_transfer(&self, transfer_id: &str, save_path: Option<&str>) -> Result<(), String> {
        let offers = self.pending_offers.read().unwrap();
        let offer = offers.get(transfer_id)
            .ok_or("No pending offer with this ID")?
            .clone();
        drop(offers);
        self.adapter.accept_transfer(&offer, save_path).await;
        self.pending_offers.write().unwrap().remove(transfer_id);
        Ok(())
    }
}
```

**Integration wiring for file transfer**:

```rust
// In setup, after MeshNode:

// Create FileTransferManager
let (ft_event_tx, ft_event_rx) = mpsc::unbounded_channel();
let ft_manager = FileTransferManager::new(
    FileTransferConfig::default(),
    ft_event_tx,
);

// Create FileTransferAdapter
let (ft_bus_tx, ft_bus_rx) = mpsc::unbounded_channel();
let (ft_adapter_event_tx, ft_adapter_event_rx) = mpsc::unbounded_channel();

// DialFn: uses GoShim to dial through Tailscale
let shim_for_dial = Arc::clone(&sidecar.shim);
let dial_fn: DialFn = Arc::new(move |addr: &str| {
    let shim = Arc::clone(&shim_for_dial);
    let addr = addr.to_string();
    Box::pin(async move {
        shim.dial(&addr).await.map_err(|e| std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            e.to_string(),
        ))
    })
});

let ft_adapter = FileTransferAdapter::new(
    FileTransferAdapterConfig {
        local_device_id: config.device_id.clone(),
        local_addr: format!("{}:{}", tailscale_ip, ft_port),
        output_dir: config.file_output_dir.clone(),
        dial_fn,
    },
    ft_manager.clone(),
    ft_bus_tx,
    ft_adapter_event_tx,
);

// Start file transfer outgoing pump
tokio::spawn(pumps::file_transfer_pump(ft_bus_rx, Arc::clone(&mesh_node)));

// Start adapter event orchestrator
let filedrop_engine = Arc::new(FileDropEngine::new(ft_adapter, app_handle.clone()));
let engine_for_events = Arc::clone(&filedrop_engine);
tokio::spawn(async move {
    engine_for_events.run_event_loop(ft_adapter_event_rx).await;
});
```

**Verification steps**:
1. Select "Send File..." from tray menu -> pick file -> pick device -> transfer starts
2. Remote device shows notification with Accept/Reject
3. Accept -> transfer completes, file appears in output directory
4. Progress events update tray popup UI
5. Reject -> sender gets notification
6. Cancel during transfer -> both sides clean up

**Estimated LOC**: ~600 Rust, ~200 TypeScript

---

### Phase 5: Drop Zone + UX Polish

**Goal**: Drag-and-drop file sending, floating drop zone window, tray popup polish.

**Files to create**:

```
src/
  dropzone.html                    -- Floating drop zone
  styles/dropzone.css
  scripts/dropzone.ts
```

**Files to modify**:
- `src-tauri/src/commands/mod.rs` -- Add `cheeseboard_toggle_dropzone`, `cheeseboard_show_settings`
- `src-tauri/src/tray/mod.rs` -- Add "Show Drop Zone" toggle
- `src/scripts/main.ts` -- Polish device list, add transfer progress UI

**Key implementation detail**: The drop zone window is created on demand, not at startup:

```rust
// commands/mod.rs
#[command]
async fn cheeseboard_toggle_dropzone(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("dropzone") {
        if window.is_visible().unwrap_or(false) {
            window.hide().map_err(|e| e.to_string())?;
        } else {
            window.show().map_err(|e| e.to_string())?;
        }
    } else {
        // Create window on demand
        tauri::WebviewWindowBuilder::new(
            &app,
            "dropzone",
            tauri::WebviewUrl::App("dropzone.html".into()),
        )
        .title("Cheeseboard Drop Zone")
        .inner_size(200.0, 200.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .build()
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
```

**Verification steps**:
1. "Show Drop Zone" creates floating transparent window
2. Drag file onto drop zone -> device picker appears -> transfer starts
3. Drop zone window stays on top, can be repositioned
4. Toggle hides/shows drop zone

**Estimated LOC**: ~200 Rust, ~250 TypeScript, ~100 CSS

---

### Phase 6: Mobile Web Server + QR Code

**Goal**: Phone users connect via QR code scan, share clipboard and files.

**Files to create**:

```
src-tauri/src/
  mobile/
    mod.rs                         -- MobileWebServer
    routes.rs                      -- REST API routes
    ws.rs                          -- WebSocket handler
    qr.rs                          -- QR code SVG generation
    auth.rs                        -- Token auth middleware
  commands/
    mobile.rs                      -- Mobile server Tauri commands
src/
  mobile/
    index.html                     -- Mobile web client SPA
    style.css
    app.js
    ws.js                          -- WebSocket client
```

**Key structs**:

```rust
// mobile/mod.rs
pub struct MobileWebServer {
    port: u16,
    auth_token: String,
    history_store: Arc<ClipboardHistoryStore>,
    filedrop_engine: Arc<FileDropEngine>,
    mesh_node: Arc<MeshNode>,
    shutdown: CancellationToken,
    ws_broadcast: broadcast::Sender<String>,
}

// mobile/auth.rs
pub async fn auth_middleware(
    State(token): State<String>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let query: HashMap<String, String> = req.uri().query()
        .map(|q| url::form_urlencoded::parse(q.as_bytes())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect())
        .unwrap_or_default();
    
    match query.get("token") {
        Some(t) if t == &token => next.run(req).await,
        _ => (StatusCode::UNAUTHORIZED, "Invalid token").into_response(),
    }
}
```

**Dependencies to add**:
```toml
axum = "0.8"
qrcode = "0.14"
include_dir = "0.7"
rand = "0.8"
tokio-tungstenite = "0.24"  # for WebSocket in mobile server
```

Note: `axum` is already a transitive dependency of `truffle-core` (used by file transfer receiver). Pin to the same version.

**Verification steps**:
1. Tray popup shows QR code icon
2. Click QR -> QR code displayed with URL
3. Scan QR on phone -> mobile web client loads
4. Type text on phone -> appears in desktop clipboard
5. Browse clipboard history on phone
6. Upload file from phone -> arrives on desktop
7. Invalid token -> 401 response

**Estimated LOC**: ~500 Rust, ~400 JavaScript (mobile client), ~200 CSS

---

### Phase 7: Settings + Auto-Launch + Final Polish

**Goal**: Production-ready tray app.

**Files to create**:

```
src/
  settings.html                    -- Settings window
  styles/settings.css
  scripts/settings.ts
src-tauri/src/
  commands/
    settings.rs                    -- Settings Tauri commands
```

**Files to modify**:
- `src-tauri/src/lib.rs` -- Register autostart plugin, all remaining commands
- `src-tauri/src/config/mod.rs` -- Add config validation, migration
- `src-tauri/src/tray/mod.rs` -- Add tray icon state transitions
- `src-tauri/src/tray/icons.rs` -- Icon state machine

**Dependencies to add**:
```toml
tauri-plugin-autostart = "2"
tauri-plugin-notification = "2"
tauri-plugin-dialog = "2"
tauri-plugin-os = "2"
```

**Verification steps**:
1. Settings window opens with all config options
2. Changes persist across restart
3. Auto-launch at login works (macOS LaunchAgent)
4. Tray icon changes state: connected (solid) / syncing (arrows) / disconnected (outline)
5. Global shortcut `Cmd+Shift+V` toggles clipboard palette
6. Error recovery: disconnect Tailscale, reconnect, sync resumes

**Estimated LOC**: ~400 Rust, ~300 TypeScript, ~200 CSS

---

## Part 4: Risk Register

### Risk 1: Go Sidecar Binary Distribution (HIGH)

**Risk**: The `sidecar-slim` Go binary must be bundled with the Tauri app. Cross-compilation of Go binaries with `tsnet` dependency (which uses CGO for some platforms) is non-trivial. The binary must be code-signed for macOS distribution.

**Impact**: Blocks shipping entirely if the sidecar cannot be bundled.

**Mitigation**:
- Pre-compile sidecar for macOS arm64/amd64 in CI using `GOOS=darwin GOARCH=arm64 go build`
- Use Tauri's `externalBin` configuration to bundle the sidecar
- Verify code signing works with `codesign --force --sign - sidecar-slim`
- **Spike needed**: Build and bundle sidecar in a minimal Tauri app before Phase 1

### Risk 2: arboard Cross-Platform Behavior (MEDIUM)

**Risk**: `arboard` v3 has different behavior across platforms:
- macOS: clipboard image support works but `Clipboard` is `!Send`
- Linux/Wayland: clipboard access requires a running display server and sometimes a clipboard daemon
- The `Clipboard::new()` failure modes are not well-documented

**Impact**: Clipboard sync silently fails on some systems.

**Mitigation**:
- Use the dedicated clipboard thread pattern (Fix 3 above) which isolates `!Send` constraints
- Add robust error handling with fallback (log error, show tray notification, continue without clipboard sync)
- Focus on macOS first; Linux/Windows are Phase 7+

### Risk 3: Tauri v2 Tray API Stability (MEDIUM)

**Risk**: Tauri v2's tray API (particularly `TrayIconBuilder`, `on_tray_icon_event`, window positioning relative to tray) is relatively new. API changes between 2.x minor versions could break the tray implementation.

**Impact**: Tray popup positioning, icon management, and menu event handling may need rework.

**Mitigation**:
- Pin to a specific Tauri 2.x version (e.g., `2.2.x`)
- Avoid unstable/experimental tray features
- Tray popup positioning can fall back to center-of-screen if tray rect API is unavailable
- The context menu is a solid fallback if the popup window approach proves fragile

### Risk 4: StoreSyncAdapter + ClipboardHistoryStore Integration Complexity (MEDIUM)

**Risk**: The `SyncableStore` trait uses synchronous `&self` methods, but the clipboard history needs async notifications for local changes. The adapter's outgoing message pump, change notification channel, and deduplication logic create a complex feedback loop that is easy to get wrong (infinite broadcast loops, missed updates, stale state).

**Impact**: Clipboard history sync is unreliable or causes excessive network traffic.

**Mitigation**:
- Write comprehensive unit tests for `ClipboardHistoryStore` SyncableStore implementation BEFORE integration
- Add a deduplication guard on the change notification (debounce notifications, skip if fingerprint unchanged)
- Log all StoreSyncAdapter messages during development
- **Spike needed**: Create a standalone test binary that creates two `StoreSyncAdapter` instances connected by channels and verifies bidirectional clipboard history sync

### Risk 5: Echo Suppression Race Conditions (LOW-MEDIUM)

**Risk**: The echo suppression mechanism (fingerprint ring buffer with TTL) has race windows:
1. Device A copies text
2. Clipboard monitor detects change, computes fingerprint, broadcasts
3. Broadcast arrives at Device B
4. Device B writes to clipboard
5. Device B's monitor detects change
6. If the fingerprint check happens BEFORE the echo guard records the write (step 4), it will re-broadcast (loop)

The window between "record fingerprint in echo guard" and "write to clipboard" is critical.

**Impact**: Clipboard ping-pong (infinite loop of syncs between two devices).

**Mitigation**:
- Record fingerprint in echo guard BEFORE writing to clipboard (the RFC already specifies this order)
- Use a generous TTL (2-3 seconds) to cover latency variance
- Add a "suppress all changes for N ms after writing" global cooldown as a safety net
- Unit test the echo guard with concurrent access patterns

---

### Critical Files for Implementation

- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/mesh/node.rs` -- MeshNode constructor, event_rx channel, broadcast_envelope/send_envelope APIs are the integration surface
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/store_sync/adapter.rs` -- StoreSyncAdapter::new(), handle_sync_message(), handle_local_changed() -- must be wired correctly for clipboard history
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/file_transfer/adapter.rs` -- FileTransferAdapter::new() requires DialFn, bus_tx, adapter_event_tx; handle_bus_message() takes string payload
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-core/src/bridge/shim.rs` -- GoShim::spawn() and ShimConfig define the sidecar lifecycle that must be replicated in Cheeseboard
- `/Users/jamesyong/Projects/project100/p008/truffle/crates/truffle-tauri-plugin/src/events.rs` -- Reference pattern for emitting MeshNodeEvent to Tauri frontend; Cheeseboard's mesh_router.rs should follow this pattern
