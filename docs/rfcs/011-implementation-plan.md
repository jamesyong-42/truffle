# RFC 011: File Transfer Over Mesh -- Implementation Plan

**Created**: 2026-03-22
**RFC**: 011-file-transfer-over-mesh.md
**Estimated total new code**: ~900-1,200 lines (integration wiring, not new protocol code)

---

## Current State Assessment

### What Exists and Works (no changes needed)

| Component | File | Lines | Status |
|---|---|---|---|
| FileTransferManager | `truffle-core/.../file_transfer/manager.rs` | ~900 | Complete: state machine, SHA-256, progress, resume, cleanup sweeps |
| FileTransferAdapter | `truffle-core/.../file_transfer/adapter.rs` | ~600 | Complete: OFFER/ACCEPT/REJECT/CANCEL signaling, event routing |
| Receiver (axum HTTP) | `truffle-core/.../file_transfer/receiver.rs` | ~400 | Complete: PUT/HEAD endpoints, Content-Range resume, disk space checks |
| Sender (hyper HTTP) | `truffle-core/.../file_transfer/sender.rs` | ~330 | Complete: streaming upload, resume offset query, progress |
| Types | `truffle-core/.../file_transfer/types.rs` | ~400 | Complete: Transfer, FileInfo, FileTransferEvent, error codes |
| Hasher | `truffle-core/.../file_transfer/hasher.rs` | ~140 | Complete: HashingReader, hash_file, hash_partial_file |
| Progress | `truffle-core/.../file_transfer/progress.rs` | ~170 | Complete: ProgressReader with rate-limited callbacks |
| Resume | `truffle-core/.../file_transfer/resume.rs` | ~100 | Complete: .partial.meta, current_offset |
| Integration wiring | `truffle-core/integration.rs` | ~120 | Complete: wire_file_transfer(mesh_node, adapter, outgoing_rx) |
| Bridge protocol | `truffle-core/bridge/protocol.rs` | -- | Complete: bridge:dial, tsnet:listen command/event types |
| Go sidecar | `packages/sidecar-slim/main.go` | -- | Complete: handleDial, handleListen, bridge connection delivery |
| CLI location parser | `truffle-cli/.../commands/cp.rs` | ~100 | Complete: parse_location(), FileLocation enum, TransferDirection |
| Output helpers | `truffle-cli/src/output.rs` | -- | Complete: print_progress(), print_progress_complete(), format_bytes() |

### What Is Broken / Stubbed

| Component | File | Problem |
|---|---|---|
| `handle_push_file` | `truffle-cli/.../daemon/handler.rs:290-334` | **Stub**: accepts params, returns fake success without transferring bytes |
| `handle_get_file` | `truffle-cli/.../daemon/handler.rs:339-375` | **Stub**: returns "not yet implemented" error |
| `do_upload` | `truffle-cli/.../commands/cp.rs:164-290` | Reads entire file into memory, base64-encodes, sends via JSON-RPC (does not scale) |
| `do_download` | `truffle-cli/.../commands/cp.rs:293-383` | Expects base64-encoded file data in JSON-RPC response (does not work) |
| Daemon startup | `truffle-cli/.../daemon/server.rs` | Does **not** create FileTransferManager, FileTransferAdapter, or start the :9417 listener |
| Runtime | `truffle-core/runtime.rs` | Has `bridge_pending_dials` and `dial_peer()` but no file-transfer-specific wiring |

### Key Architecture Insight

The `dispatch()` function in `handler.rs` currently receives `(request, runtime, started_at, shutdown_signal)`. The `handle_push_file` and `handle_get_file` stubs do not even use the runtime -- they just parse params and return. To wire real file transfer, the handler needs access to:

1. A `FileTransferAdapter` (to call `send_file()`, `accept_transfer()`)
2. The `TruffleRuntime` (to resolve node names and access `bridge_pending_dials` + `shim` for the DialFn)
3. An event channel to forward `FileTransferEvent` progress back to the CLI

The daemon server currently owns an `Arc<TruffleRuntime>` but not a `FileTransferAdapter` or `FileTransferManager`.

---

## Phase 1: Daemon File Transfer Bootstrap

**Goal**: On `truffle up`, the daemon creates and starts the file transfer subsystem (manager + adapter + axum listener + bridge wiring). No CLI changes yet -- just verify the subsystem starts cleanly.

**Estimated LOC**: ~200

### Files to Modify

1. **`crates/truffle-cli/src/daemon/server.rs`** -- `DaemonServer::start()`
   - After `runtime.start()`, create `FileTransferManager` and `FileTransferAdapter`
   - Start the axum file transfer server on an ephemeral local port
   - Tell the sidecar to listen on port 9417 via `tsnet:listen`
   - Register a bridge handler for port 9417 incoming connections (route to local axum)
   - Wire the adapter to the mesh node using `wire_file_transfer()`
   - Store the `Arc<FileTransferAdapter>` and `Arc<FileTransferManager>` in `DaemonServer`

2. **`crates/truffle-cli/src/daemon/server.rs`** -- `DaemonServer` struct
   - Add fields: `file_transfer_adapter: Arc<FileTransferAdapter>`, `file_transfer_manager: Arc<FileTransferManager>`
   - Pass these to the `dispatch()` call in the accept loop

3. **`crates/truffle-cli/src/daemon/handler.rs`** -- `dispatch()` signature
   - Add `adapter: &Arc<FileTransferAdapter>` parameter (or a `DaemonContext` struct)
   - Thread it through to `handle_push_file` and `handle_get_file`

### Functions to Implement

```rust
// In server.rs -- helper to create the DialFn from runtime
fn create_dial_fn(runtime: &Arc<TruffleRuntime>) -> DialFn {
    let shim = runtime.shim().clone();         // Arc<Mutex<Option<GoShim>>>
    let pending = runtime.pending_dials().clone(); // Arc<Mutex<HashMap<...>>>
    Arc::new(move |addr: &str| {
        let shim = shim.clone();
        let pending = pending.clone();
        let addr = addr.to_string();
        Box::pin(async move {
            // Parse host:port from addr
            // Use shim.dial_raw() + pending_dials oneshot pattern
            // Return the TcpStream from BridgeConnection
        })
    })
}

// In server.rs -- helper to start the file transfer axum listener
async fn start_file_transfer_listener(
    manager: Arc<FileTransferManager>,
) -> Result<u16, String> {
    let app = file_transfer_router(manager);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Ok(port)
}
```

### Acceptance Criteria

- [ ] `truffle up` starts successfully with the file transfer subsystem initialized
- [ ] The axum server is listening on an ephemeral local port
- [ ] The sidecar receives `tsnet:listen { port: 9417 }` and starts accepting connections
- [ ] Bridge handler for port 9417 incoming is registered
- [ ] `wire_file_transfer()` tasks are spawned and running
- [ ] `truffle down` cleanly shuts down all file transfer tasks
- [ ] Unit test: `DaemonServer::start()` with mock sidecar creates adapter + manager

### Risks

- The `TruffleRuntime` does not currently expose `shim()` or `bridge_pending_dials` publicly. May need to add accessor methods or move the DialFn construction into `runtime.rs`.
- The bridge handler for port 9417 incoming needs to forward raw TCP connections to the local axum listener. The existing `ChannelHandler` can do this, but a new handler that `tokio::io::copy`s between the bridge stream and a new `TcpStream::connect("127.0.0.1:{axum_port}")` connection may be needed.

---

## Phase 2: Wire Upload (`truffle cp file.txt server:/tmp/`)

**Goal**: Replace the `handle_push_file` stub with a real implementation that drives the `FileTransferAdapter`, and update the CLI `do_upload` to stop sending file data over JSON-RPC.

**Estimated LOC**: ~250

### Files to Modify

1. **`crates/truffle-cli/src/daemon/handler.rs`** -- `handle_push_file()`
   - Accept the `FileTransferAdapter` and `TruffleRuntime` as parameters
   - Resolve node name to device_id using `resolve_node_addr()` (already implemented)
   - Call `adapter.send_file(target_device_id, local_path)` to initiate the transfer
   - Subscribe to `FileTransferEvent` channel to wait for Prepared, then construct OFFER
   - Wait for the transfer to complete (Complete or Error event)
   - Return the result (bytes_transferred, sha256, duration)

2. **`crates/truffle-cli/src/daemon/handler.rs`** -- new `handle_incoming_offer()`
   - When an OFFER with `cli_mode: true` arrives, auto-accept
   - Validate the save_path
   - Call `adapter.accept_transfer()`

3. **`crates/truffle-core/src/services/file_transfer/adapter.rs`** -- `FileTransferOffer`
   - Add optional fields: `cli_mode: Option<bool>`, `save_path: Option<String>`
   - These enable auto-accept on the receiver side

4. **`crates/truffle-cli/src/commands/cp.rs`** -- `do_upload()`
   - Remove `std::fs::read(local_path)` (reading entire file into memory)
   - Remove base64 encoding
   - Send JSON-RPC with only metadata: `{ node, path, verify }` (no `data_base64`)
   - The daemon reads the file directly from disk (it runs on the same machine)

5. **`crates/truffle-cli/src/daemon/protocol.rs`** -- method constants
   - Rename `PUSH_FILE` to `CP_SEND` (or keep and document the new semantics)
   - Update the params contract: remove `data_base64`, add `local_path`

### Functions to Implement

```rust
// handler.rs
async fn handle_push_file(
    id: u64,
    params: &serde_json::Value,
    runtime: &Arc<TruffleRuntime>,
    adapter: &Arc<FileTransferAdapter>,
) -> DaemonResponse {
    // 1. Parse params: node, local_path (absolute), remote_path, verify
    // 2. Verify local_path exists and is a file
    // 3. Resolve node to device_id
    // 4. Generate transfer_id and token
    // 5. Call manager.prepare_file() to stat + hash
    // 6. Wait for Prepared event
    // 7. Construct OFFER with cli_mode=true, save_path=remote_path
    // 8. Send OFFER via adapter's bus_tx
    // 9. Wait for ACCEPT (with timeout)
    // 10. On ACCEPT: register_send + spawn send_file
    // 11. Wait for Complete/Error event
    // 12. Return result
}
```

### Key Design Decision: Orchestration Layer

The existing `FileTransferAdapter.send_file()` is incomplete -- it calls `prepare_file` but doesn't wait for the Prepared event or send the OFFER. The full orchestration (prepare -> wait -> offer -> wait accept -> send) needs to happen somewhere. Options:

**(A) In `handle_push_file` (daemon handler)**: The handler orchestrates the full flow directly. Simple and explicit, but the handler becomes a 100+ line async function.

**(B) Complete `FileTransferAdapter.send_file()`**: Make it a full orchestrating function that returns a `TransferResult` future. Cleaner API but requires more refactoring of the adapter.

**Recommendation**: Option A for Phase 2. The handler is close to the user and has access to runtime + adapter. Option B can be a Phase 5 refactoring if the pattern proves reusable.

### Acceptance Criteria

- [ ] `truffle cp file.txt server:/tmp/` transfers the file end-to-end
- [ ] The file lands at `/tmp/file.txt` on the receiver with correct contents
- [ ] SHA-256 is verified on the receiver
- [ ] The CLI prints transfer result (bytes, duration)
- [ ] No file data passes through JSON-RPC (daemon reads from disk directly)
- [ ] Files up to 100MB transfer successfully
- [ ] Transfer fails gracefully when the node is offline (error message, not crash)
- [ ] Transfer fails gracefully when the remote path is invalid

### Risks

- **Prepared event timing**: `prepare_file()` is async and can take seconds for large files (SHA-256 hashing at ~500MB/s). The handler must wait for the Prepared event before sending the OFFER, but `prepare_file()` returns immediately and emits the event via the event channel. The handler needs to subscribe to the manager's event channel and filter for its transfer_id.
- **Accept timeout**: If the receiver is slow or the OFFER gets lost, the handler will block indefinitely. Need a timeout (30s suggested).
- **Channel coordination**: The adapter's `bus_tx` sends messages to the mesh. The handler needs to also listen for incoming ACCEPT messages on the adapter's event channel. This requires subscribing to `adapter_event_rx` -- but that's an `mpsc::UnboundedReceiver` which has a single consumer. May need to change to a `broadcast::Sender` or pass a dedicated `oneshot` for this transfer.

---

## Phase 3: Wire Download (`truffle cp server:/tmp/file.txt ./`)

**Goal**: Implement the PULL_REQUEST reverse-roles download flow.

**Estimated LOC**: ~200

### Files to Modify

1. **`crates/truffle-core/src/services/file_transfer/adapter.rs`**
   - Add `PULL_REQUEST` to `message_types`
   - Add `FileTransferPullRequest` struct: `{ request_id, remote_path, requester_addr, requester_device_id }`
   - Handle incoming `PULL_REQUEST` in `handle_bus_message()`: validate path, prepare_file, send OFFER back

2. **`crates/truffle-cli/src/daemon/handler.rs`** -- `handle_get_file()`
   - Resolve node to device_id
   - Send PULL_REQUEST via adapter's bus_tx
   - Wait for incoming OFFER (from the remote node, which now becomes the sender)
   - Auto-accept the OFFER with local save_path
   - Wait for transfer Complete event
   - Return result

3. **`crates/truffle-cli/src/commands/cp.rs`** -- `do_download()`
   - Remove base64 decode logic
   - Send JSON-RPC with: `{ node, remote_path, local_path }`
   - Wait for result (daemon handles the file I/O)

4. **`crates/truffle-cli/src/daemon/protocol.rs`**
   - Update `GET_FILE` params contract: remove response `data_base64`, add `local_path`

### Functions to Implement

```rust
// adapter.rs -- new message type
pub mod message_types {
    // ... existing ...
    pub const PULL_REQUEST: &str = "PULL_REQUEST";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTransferPullRequest {
    pub request_id: String,
    pub remote_path: String,
    pub requester_device_id: String,
    pub requester_addr: String,
    pub save_path: String,
}

// In handle_bus_message(), add PULL_REQUEST handler:
// 1. Validate remote_path exists, is a file
// 2. prepare_file()
// 3. Wait for Prepared event
// 4. Send OFFER back to requester with cli_mode=true
```

```rust
// handler.rs
async fn handle_get_file(
    id: u64,
    params: &serde_json::Value,
    runtime: &Arc<TruffleRuntime>,
    adapter: &Arc<FileTransferAdapter>,
) -> DaemonResponse {
    // 1. Parse: node, remote_path, local_path
    // 2. Resolve local_path to absolute
    // 3. Resolve node to device_id
    // 4. Send PULL_REQUEST via bus_tx
    // 5. Wait for incoming OFFER from remote
    // 6. Auto-accept with local_path
    // 7. Wait for Complete/Error
    // 8. Return result
}
```

### Acceptance Criteria

- [ ] `truffle cp server:/tmp/file.txt ./` downloads the file
- [ ] The file lands locally with correct contents and SHA-256 match
- [ ] The remote node's file is not modified or deleted
- [ ] Downloads work in both directions between two nodes
- [ ] `truffle cp server:/tmp/file.txt ./renamed.txt` works (rename on download)
- [ ] Error when remote path does not exist
- [ ] Error when remote path is a directory

### Risks

- **PULL_REQUEST handling on the remote side**: The remote daemon's adapter must handle the incoming PULL_REQUEST message. This requires that the remote daemon also has Phase 1 bootstrapping completed -- the `wire_file_transfer` integration must be active on both nodes.
- **Event routing for downloads**: The download flow reverses the normal sender/receiver roles. The local daemon is the receiver but it initiated the request. The `handle_bus_message` for OFFER on the local side must recognize this as a response to its PULL_REQUEST and auto-accept.

---

## Phase 4: Progress Reporting (CLI to Daemon Streaming)

**Goal**: Stream progress events from the daemon to the CLI as JSON-RPC notifications, and render a progress bar.

**Estimated LOC**: ~200

### Files to Modify

1. **`crates/truffle-cli/src/daemon/protocol.rs`**
   - Add `DaemonNotification` struct (JSON-RPC notification: no `id` field)
   - Add notification method constants: `CP_PROGRESS`, `CP_COMPLETE`, `CP_ERROR`

2. **`crates/truffle-cli/src/daemon/ipc.rs`** or **`server.rs`**
   - Modify the connection handler to support sending notifications after the initial response
   - The current protocol is request-response (one line in, one line out). Need to extend to: request -> [notification]* -> response
   - Option A: Keep the Unix socket open during the transfer, send notifications as separate JSON lines, then send the final response
   - Option B: Return immediately with a `transfer_id`, then CLI polls via a separate `cp_status` call

   **Recommendation**: Option A. It is simpler and the progress bar needs real-time updates. The `cp` command already blocks waiting for the response.

3. **`crates/truffle-cli/src/daemon/handler.rs`** -- `handle_push_file()` / `handle_get_file()`
   - Accept a `notification_tx: mpsc::Sender<DaemonNotification>` parameter
   - As `FileTransferEvent::Progress` events arrive, send them as notifications
   - The final `Complete` or `Error` event becomes the normal response

4. **`crates/truffle-cli/src/daemon/client.rs`** -- `DaemonClient::request()`
   - Modify to read multiple JSON lines from the socket
   - Lines without `id` are notifications (forward to a callback)
   - The line with matching `id` is the final response

5. **`crates/truffle-cli/src/commands/cp.rs`** -- `do_upload()` / `do_download()`
   - Add a notification handler callback that calls `output::print_progress()`
   - On final response, call `output::print_progress_complete()`

### Protocol Extension

```
CLI -> Daemon (one line):
  {"jsonrpc":"2.0","id":1,"method":"push_file","params":{...}}

Daemon -> CLI (multiple lines):
  {"jsonrpc":"2.0","method":"cp.progress","params":{"transfer_id":"ft-abc","percent":12.5,"bytes_per_second":47400000,"eta":15.2}}
  {"jsonrpc":"2.0","method":"cp.progress","params":{"transfer_id":"ft-abc","percent":45.2,"bytes_per_second":51200000,"eta":8.1}}
  ...
  {"jsonrpc":"2.0","id":1,"result":{"bytes_transferred":104857600,"sha256":"abc...","duration_ms":2100}}
```

### Functions to Implement

```rust
// protocol.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonNotification {
    pub jsonrpc: String, // "2.0"
    pub method: String,
    pub params: serde_json::Value,
}

// client.rs
pub async fn request_with_progress(
    &self,
    method: &str,
    params: serde_json::Value,
    on_notification: impl Fn(&DaemonNotification),
) -> Result<serde_json::Value, DaemonError> {
    // Send request, then loop reading lines:
    // - If line has no "id", it's a notification -> call on_notification
    // - If line has matching "id", it's the response -> return
}

// cp.rs -- progress rendering
fn render_progress(notification: &DaemonNotification) {
    let percent = notification.params["percent"].as_f64().unwrap_or(0.0);
    let speed = notification.params["bytes_per_second"].as_f64().unwrap_or(0.0);
    let total = notification.params["total_bytes"].as_i64().unwrap_or(0) as u64;
    let current = notification.params["bytes_transferred"].as_i64().unwrap_or(0) as u64;
    output::print_progress(current, total, speed);
}
```

### Acceptance Criteria

- [ ] Progress bar updates in real-time during transfer
- [ ] Progress shows: percentage, speed (MB/s), ETA
- [ ] Final line shows: 100%, total size, total time, SHA-256 status
- [ ] Progress bar renders correctly in standard 80-column terminal
- [ ] Progress is suppressed when stdout is not a TTY (piped output)
- [ ] `--json` flag outputs progress as JSON lines (for scripting)
- [ ] Notification parsing does not break if daemon sends unexpected messages

### Risks

- **IPC protocol change**: Extending the request-response protocol to support notifications is the most significant change in this plan. The current `client.rs` `request()` reads exactly one line. Making it read multiple lines introduces complexity around when to stop reading.
- **Terminal rendering**: `print_progress` uses carriage return (`\r`) to overwrite the current line. This works in standard terminals but may behave oddly in some environments. Use `atty::is(Stream::Stdout)` or `std::io::stdout().is_terminal()` to detect.

---

## Phase 5: Auto-Accept and Security

**Goal**: Implement auto-accept for CLI-mode transfers and ensure security validation is in place.

**Estimated LOC**: ~100

### Files to Modify

1. **`crates/truffle-core/src/services/file_transfer/adapter.rs`** -- `handle_bus_message()` OFFER branch
   - Check for `cli_mode: true` in the offer payload
   - If `cli_mode` and `save_path` is present, auto-accept without emitting AdapterEvent::Offer
   - Validate save_path before accepting
   - Emit a log message for auto-accepted transfers

2. **`crates/truffle-core/src/services/file_transfer/adapter.rs`** -- `FileTransferOffer`
   - Add `#[serde(default)] pub cli_mode: bool`
   - Add `#[serde(default, skip_serializing_if = "Option::is_none")] pub save_path: Option<String>`

3. **`crates/truffle-core/src/services/file_transfer/manager.rs`** -- `validate_save_path()`
   - Already implemented and tested. No changes needed.

### Functions to Modify

```rust
// adapter.rs -- OFFER handler
message_types::OFFER => {
    if let Ok(offer) = serde_json::from_value::<FileTransferOffer>(payload.clone()) {
        if offer.cli_mode && offer.save_path.is_some() {
            // Auto-accept: validate path, register, send ACCEPT
            let save_path = offer.save_path.as_deref().unwrap_or("");
            if !save_path.is_empty() {
                self.accept_transfer(&offer, Some(save_path)).await;
                return;
            }
        }
        // Interactive mode: emit Offer event for UI confirmation
        let _ = self.adapter_event_tx.send(AdapterEvent::Offer(offer));
    }
}
```

### Acceptance Criteria

- [ ] CLI transfers auto-accept without user interaction
- [ ] GUI/NAPI consumers still receive AdapterEvent::Offer for interactive confirmation
- [ ] Path traversal attacks are rejected (e.g., `save_path: "/tmp/../etc/passwd"`)
- [ ] Relative paths are rejected
- [ ] Missing parent directories are rejected
- [ ] Token validation is constant-time (already implemented in manager.rs)

### Risks

- Low risk. The auto-accept logic is straightforward and `validate_save_path()` is already thoroughly tested with adversarial inputs.

---

## Phase 6: Polish, Edge Cases, and Testing

**Goal**: Handle edge cases, add integration tests, and clean up the CLI experience.

**Estimated LOC**: ~150

### Files to Modify

1. **`crates/truffle-cli/src/commands/cp.rs`**
   - Remove base64_encode/base64_decode helper functions (no longer needed)
   - Remove `compute_sha256` function (daemon handles hashing now)
   - Update `do_upload` and `do_download` error messages
   - Handle `--verify` / `--no-verify` flags properly
   - Handle `truffle cp file.txt server:` (no path, use default receive dir)
   - Add `--force` flag for overwrite behavior

2. **`crates/truffle-cli/src/daemon/handler.rs`**
   - Handle transfer cancellation (Ctrl+C in CLI -> cancel the in-progress transfer)
   - Handle daemon restart during transfer (graceful error)
   - Handle simultaneous transfers (the manager's semaphore already limits to 5)

3. **`crates/truffle-core/src/services/file_transfer/types.rs`** -- `FileTransferConfig`
   - Raise `max_file_size` default from 4GB to 100GB for CLI transfers
   - Make configurable via `truffle.toml`

4. **Tests** (new files)
   - `crates/truffle-cli/tests/cp_integration.rs` -- end-to-end cp tests with mock daemon
   - `crates/truffle-core/tests/file_transfer_e2e.rs` -- manager+receiver+sender integration test over localhost

### Acceptance Criteria

- [ ] `truffle cp` works for files from 0 bytes to 1GB+
- [ ] Ctrl+C during transfer cancels cleanly (no orphaned .partial files)
- [ ] Concurrent transfers work (2 uploads simultaneously)
- [ ] Error messages are user-friendly for common failures
- [ ] Base64 encoding/decoding is removed from cp.rs
- [ ] `truffle cp --json file.txt server:/tmp/` outputs machine-readable JSON
- [ ] Integration test: upload + download round-trip verifies SHA-256

---

## Dependency Graph

```
Phase 1: Bootstrap ─────────┐
  (daemon creates FT         │
   subsystem on startup)     │
                             v
Phase 2: Upload ────────────┐
  (truffle cp file server:)  │
                             ├──> Phase 4: Progress
Phase 3: Download ──────────┤     (streaming notifications)
  (truffle cp server:file .) │
                             ├──> Phase 5: Auto-Accept
                             │     (cli_mode flag)
                             v
                        Phase 6: Polish
                          (edge cases, tests)
```

- **Phase 1** is a hard prerequisite for everything else.
- **Phases 2 and 3** are independent of each other but both require Phase 1.
- **Phase 4** can be started alongside Phase 2/3 (protocol extension is independent) but is most useful after at least one of them works.
- **Phase 5** can be done at any point after Phase 1.
- **Phase 6** depends on all previous phases.

---

## What Gets Reused vs. What Is New

### Reused (zero or trivial changes)

- `FileTransferManager` -- all methods (prepare_file, register_receive, register_send, send_file, receive_file_bytes, cancel_transfer)
- `FileTransferAdapter` -- most methods (accept_transfer, reject_transfer, cancel_transfer, handle_bus_message for OFFER/ACCEPT/REJECT/CANCEL, handle_manager_event)
- `file_transfer_router()` -- axum PUT/HEAD endpoints
- Sender HTTP client (hyper)
- HashingReader, ProgressReader, resume module
- `wire_file_transfer()` integration helper
- Go sidecar bridge:dial and tsnet:listen support
- `parse_location()`, `FileLocation`, `TransferDirection` in cp.rs
- `output::print_progress()`, `output::print_progress_complete()`
- `resolve_node_addr()` in handler.rs

### New Code Required

| What | Where | Est. Lines |
|---|---|---|
| Daemon bootstrap (create manager/adapter, start listener, register bridge) | `server.rs` | ~80 |
| DialFn construction from runtime shim/bridge | `server.rs` or `runtime.rs` | ~40 |
| Bridge-to-axum forwarding handler | `server.rs` | ~30 |
| `handle_push_file` real implementation | `handler.rs` | ~120 |
| `handle_get_file` real implementation | `handler.rs` | ~100 |
| PULL_REQUEST message type + handler | `adapter.rs` | ~80 |
| Auto-accept for cli_mode offers | `adapter.rs` | ~30 |
| `DaemonNotification` type | `protocol.rs` | ~20 |
| `request_with_progress()` client method | `client.rs` | ~50 |
| Progress rendering callback | `cp.rs` | ~30 |
| Updated `do_upload` (remove base64) | `cp.rs` | ~30 (net reduction) |
| Updated `do_download` (remove base64) | `cp.rs` | ~30 (net reduction) |
| `FileTransferOffer` cli_mode/save_path fields | `adapter.rs` | ~10 |
| **Total new** | | **~650** |
| **Total removed** (base64, stub handlers) | | **~200** |
| **Net change** | | **~450 lines** |

---

## Open Questions to Resolve Before Implementation

1. **Handler context**: Should we introduce a `DaemonContext` struct to bundle `runtime + adapter + manager + shutdown_signal + started_at`, or keep passing individual parameters to `dispatch()`?
   - **Recommendation**: Introduce `DaemonContext`. The parameter list is growing and will grow further.

2. **Adapter event channel**: The adapter's `adapter_event_tx` is an `mpsc::UnboundedSender`. The handler needs to receive events for a specific transfer_id. Options:
   - (A) Subscribe to the manager's `event_tx` directly (it's `mpsc::UnboundedSender` -- can only have one consumer)
   - (B) Change to `broadcast::Sender` so multiple consumers can subscribe
   - (C) Use a per-transfer `oneshot::Sender` registered before initiating the transfer
   - **Recommendation**: (B) Change manager's `event_tx` to `broadcast::Sender<FileTransferEvent>`. This allows the adapter event loop AND the handler to both receive events. The adapter already filters by transfer_id; the handler will do the same.

3. **Notification framing**: Should notifications use the same newline-delimited JSON as responses, or a different framing?
   - **Recommendation**: Same framing (newline-delimited JSON). A notification is distinguished from a response by the absence of an `id` field. This is standard JSON-RPC 2.0.

4. **Default receive directory**: When `truffle cp file.txt server:` has no path, what is the default?
   - **Recommendation**: `~/Downloads/truffle/` (configurable in truffle.toml). Create the directory on first use.

5. **DialFn for file transfer vs. DialFn for mesh connections**: The runtime's existing `dial_peer()` method dials port 9417 and upgrades to WebSocket. The file transfer DialFn needs to dial port 9417 and return a raw `TcpStream` (no WebSocket upgrade). These are different flows sharing the same bridge infrastructure.
   - **Recommendation**: Create a separate `dial_tcp_raw()` method on runtime that returns `TcpStream` without WebSocket upgrade. Or extract the bridge-dial-and-wait pattern into a helper.

---

## Implementation Order for a Single Developer

If working sequentially, the recommended order is:

1. **Phase 1** (bootstrap) -- 1 session
2. **Phase 5** (auto-accept) -- quick, ~30 min, enables Phase 2 testing
3. **Phase 2** (upload) -- 1-2 sessions, the core work
4. **Phase 3** (download) -- 1 session, mostly analogous to Phase 2
5. **Phase 4** (progress) -- 1 session, protocol extension
6. **Phase 6** (polish) -- 1 session, cleanup and tests

Total estimated effort: **4-6 focused sessions**.

---

## References

- RFC 011: `docs/rfcs/011-file-transfer-over-mesh.md`
- Existing file transfer code: `crates/truffle-core/src/services/file_transfer/`
- Daemon handler: `crates/truffle-cli/src/daemon/handler.rs`
- Daemon server: `crates/truffle-cli/src/daemon/server.rs`
- CLI cp command: `crates/truffle-cli/src/commands/cp.rs`
- Runtime: `crates/truffle-core/src/runtime.rs`
- Bridge manager: `crates/truffle-core/src/bridge/manager.rs`
- Integration wiring: `crates/truffle-core/src/integration.rs`
- Go sidecar: `packages/sidecar-slim/main.go`
