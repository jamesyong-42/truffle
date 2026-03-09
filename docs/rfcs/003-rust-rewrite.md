# RFC 003: Truffle Rust Rewrite — Option C: Thin Go Shim + Rust Core

## Context

Truffle is a ~8,200 LOC mesh networking framework (~4,400 LOC TypeScript + ~3,800 LOC Go source) built on Tailscale. The goal is to rewrite it in Rust for long-term robustness, performance, and to support both Node.js packages (via NAPI-RS) and Tauri v2 desktop apps from a single Rust core.

**Option C**: Shrink Go from ~3,800 LOC source (~5,200 including tests) to ~500 LOC (just tsnet wrapper + TCP proxy). Move all application logic — WebSocket handling, mesh topology, file transfer, store sync — into Rust.

### Critical Discovery: tsnet Virtual Networking

tsnet connections are virtual (gvisor/netstack userspace networking inside the Go process). You **cannot** extract file descriptors and hand them to another process. The Go shim must act as a **transparent TCP proxy**: accepting tsnet connections and bridging them to local TCP sockets where Rust is listening. This adds ~10-50µs latency per message (full read-copy-write cycle including syscall overhead, not just loopback RTT), which is negligible compared to WireGuard's ~1ms network latency and 40-60 MB/s throughput bottleneck.

**Performance note on splice(2)**: On Linux, Go's `io.Copy` between two `*net.TCPConn` can use `splice(2)` for zero-copy kernel-to-kernel transfer. However, this only benefits **port 9417** (plain TCP file transfer) where both sides of the bridge are raw TCP connections. **Port 443 cannot use splice** because Go terminates TLS — the source is a `tls.Conn` (userspace), not a `*net.TCPConn`, so bytes must pass through Go's TLS stack before being copied to the local bridge socket. This is still fast (memory-to-memory copy on the same machine) but is not zero-copy.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│  Node.js App / Tauri App                                    │
├─────────────────────────────────────────────────────────────┤
│  truffle-napi (NAPI-RS) │ truffle-tauri-plugin (Tauri v2)  │
├─────────────────────────────────────────────────────────────┤
│  truffle-core (pure Rust library)                           │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ MeshNode  │ MessageBus │ StoreSyncAdapter              │ │
│  │ DeviceManager │ PrimaryElection │ FileTransferAdapter  │ │
│  ├────────────────────────────────────────────────────────┤ │
│  │ ConnectionManager (WebSocket over bridged TCP)         │ │
│  │ FileTransferManager (HTTP PUT/HEAD over bridged TCP)   │ │
│  ├────────────────────────────────────────────────────────┤ │
│  │ BridgeManager (local TCP listener, header parsing)     │ │
│  │ GoShim (spawn process, stdin/stdout JSON commands)     │ │
│  └──────────────┬─────────────────────────────────────────┘ │
│                 │ local TCP (bridge) + stdin/stdout (cmds)  │
│  ┌──────────────▼─────────────────────────────────────────┐ │
│  │  Go Thin Shim (~500 LOC)                               │ │
│  │  - tsnet Start/Stop/GetPeers/GetDNSName               │ │
│  │  - Accept tsnet conns → proxy to Rust's local port     │ │
│  │  - On "dial" command → tsnet.Dial → proxy to Rust      │ │
│  └────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

---

## 1. Go Thin Shim Design (~500 LOC)

**Keeps**: `internal/tsnet/node.go` (400 LOC, unchanged)
**Replaces**: Everything else (server, dialer, filetransfer, reverseproxy, protocol, main)
**New**: ~100 LOC proxy logic

### Two Communication Channels

**Channel 1 — Command channel (stdin/stdout JSON lines)**: Lifecycle only
- Commands: `tsnet:start`, `tsnet:stop`, `tsnet:getPeers`, `bridge:dial`
- Events: `tsnet:status`, `tsnet:authRequired`, `tsnet:peers`, `bridge:dialResult`
- NOT on the data path. No `wsMessage`, `dialMessage`, or file transfer commands.
- `tsnet:start` includes `{ bridgePort, sessionToken }` — the ephemeral port Rust is listening on and a random 32-byte hex token for bridge authentication.

**Channel 2 — Data bridge (local TCP)**: All data flows here
- Rust starts a single `TcpListener` on `127.0.0.1:0`, tells Go the port and session token via the `tsnet:start` command
- For each tsnet connection (incoming or outgoing), Go connects to Rust's local port and sends a binary header (including the session token), then does bidirectional `io.Copy`

### Bridge Header Format (binary, big-endian)

```
Offset  Size  Field
0       4     Magic: 0x54524646 ("TRFF")
4       1     Version: 0x01
5       32    SessionToken: random 32-byte token (must match token from tsnet:start)
37      1     Direction: 0x01=incoming, 0x02=outgoing
38      2     ServicePort: tsnet port (443 or 9417)
40      2     RequestIdLen (0 for incoming, max 128)
42      N     RequestId (UTF-8, for correlating outgoing dials)
42+N    2     RemoteAddrLen (max 256)
44+N    M     RemoteAddr (UTF-8, e.g. "100.64.0.2:12345")
44+N+M  2     RemoteDNSNameLen (max 256)
46+N+M  K     RemoteDNSName (UTF-8, e.g. "peer-host.tailnet.ts.net", resolved by Go via LocalClient)
```

After the header: raw bidirectional bytes via `io.Copy`.

**Header invariants**:
- **Session token**: Rust generates a random 32-byte token at startup and passes it to Go via `tsnet:start`. Go includes this token in every bridge header. Rust rejects any connection where the token doesn't match — this prevents arbitrary local processes from injecting into the mesh transport on multi-user systems.
- **Max field lengths**: `RequestIdLen` capped at 128, `RemoteAddrLen` and `RemoteDNSNameLen` capped at 256. If exceeded, Rust closes the connection immediately.
- **Read timeout**: Header must be fully received within 2 seconds of accept (`tokio::time::timeout`). Prevents slowloris-style resource exhaustion.
- **Unknown version**: If `Version != 0x01`, reject the connection, log the mismatched version, and emit a `BridgeVersionMismatch { expected: 1, got: N }` event on the critical channel. This surfaces to the user as a clear error (e.g., "Go shim version 2 is incompatible with Rust core version 1 — update both"). No forward-compat negotiation — version changes require coordinated shim + core updates.
- **Incoming direction invariant**: For `Direction == 0x01` (incoming), `RequestIdLen` MUST be 0. If non-zero, reject — this is a protocol violation.

### Bridge Connection Lifecycle

**Close policy: any EOF closes both directions.** When either `io.Copy` returns (remote peer closed, Rust closed, or error), Go closes **both** the tsnet connection and the local bridge connection. No half-close support — the bridge does not propagate TCP FIN as a directional shutdown.

Rationale: WebSocket has its own close handshake (close frames), and file transfers complete with an HTTP response before either side closes. Half-close adds complexity (tracking two independent shutdown states per bridge connection) for no benefit in our protocol.

**Go-side implementation**: When one `io.Copy` finishes, immediately close both connections to unblock the other goroutine. Use a `sync.Once` to prevent double-close:

```go
func bridgeCopy(tsnetConn, localConn net.Conn) {
    var once sync.Once
    closeAll := func() {
        once.Do(func() {
            tsnetConn.Close()
            localConn.Close()
        })
    }
    defer closeAll()

    go func() {
        io.Copy(localConn, tsnetConn)
        closeAll() // remote EOF → close both
    }()
    io.Copy(tsnetConn, localConn)
    // local (Rust) EOF → close both (handled by defer)
}
```

**Write deadline**: Go sets a 60-second write deadline on both connections. If Rust disappears mid-stream or a peer becomes unresponsive, `io.Copy` will error after 60s rather than blocking forever. The deadline is refreshed on each successful write (via a wrapping `io.Writer` that calls `SetWriteDeadline` after each `Write`).

**Rust-side implications**: When the bridge connection closes (read returns EOF or error), Rust's `tokio-tungstenite` or `hyper` handler sees a transport error and tears down the protocol session. For file transfer, a mid-stream close means the transfer is incomplete — the `.partial` file and `.partial.meta` remain for resume on reconnection.

### TLS Handling

Go terminates TLS for port 443 (via `tsnet.ListenTLS`). The local bridge carries decrypted bytes. For outgoing dials to :443, Go wraps with `tls.Client` (same as current `dialer.go`). Rust sees plain HTTP/WebSocket bytes in both cases.

**Go TLS dialer details** (confirmed from current `dialer.go`):
- **SNI**: Set to the peer's MagicDNS name (`dnsName`), falling back to hostname if unavailable
- **Certificate verification**: Strict, using **system root CAs** (which include Let's Encrypt — the CA that Tailscale provisions certs from via `tsnet.ListenTLS`)
- **No certificate pinning**: Relies on standard PKI chain validation
- Rust loses all of this when TLS is terminated in Go — Rust trusts Go's `RemoteDNSName` header field for peer identity instead

### Go Shim Pseudocode

```go
func main() {
    // stdin/stdout JSON command loop
    // On tsnet:start { bridgePort, sessionToken, ... } →
    //   store sessionToken, start tsnet, listen on :443 and :9417
    // On accept(:443) → resolve peer DNS name → bridgeToRust(conn, 443, "incoming", "", peerDNS)
    // On accept(:9417) → resolve peer DNS name → bridgeToRust(conn, 9417, "incoming", "", peerDNS)
    // On bridge:dial → tsnet.Dial(target) + tls.Client if :443 → bridgeToRust(conn, port, "outgoing", requestId, targetDNS)
}

func resolvePeerDNS(remoteAddr string) string {
    // Map remote IP to peer DNS name via LocalClient().Status()
    // Returns "" if peer not found (Rust should still accept but log warning)
}

func bridgeToRust(tsnetConn net.Conn, port uint16, dir, reqId, peerDNS string) {
    localConn := net.Dial("tcp", "127.0.0.1:" + rustPort)
    writeHeader(localConn, sessionToken, dir, port, reqId, tsnetConn.RemoteAddr(), peerDNS)
    // Any EOF closes both directions (see Bridge Connection Lifecycle)
    bridgeCopy(tsnetConn, localConn)
}
```

---

## 2. Cargo Workspace Structure

```
crates/
  truffle-core/                    # Pure Rust library, no framework deps
    Cargo.toml                     # [lib]
    src/
      lib.rs
      bridge/
        mod.rs
        header.rs                  # BridgeHeader binary parse/write
        manager.rs                 # BridgeManager: TcpListener, connection routing
        shim.rs                    # GoShim: spawn process, stdin/stdout JSON
        protocol.rs                # Command/Event types for Go IPC
      transport/
        mod.rs
        connection.rs              # ConnectionManager, WSConnection
        websocket.rs               # WS read/write pumps over bridged TcpStream
        heartbeat.rs               # Ping/pong logic
        reconnect.rs               # Exponential backoff reconnect
      mesh/
        mod.rs
        node.rs                    # MeshNode orchestrator
        device.rs                  # DeviceManager
        election.rs                # PrimaryElection (longest uptime wins)
        message_bus.rs             # MeshMessageBus (namespace pub/sub)
        routing.rs                 # STAR topology routing
      file_transfer/
        mod.rs
        types.rs                   # TransferId, TransferState, FileInfo, Config
        manager.rs                 # FileTransferManager orchestrator
        sender.rs                  # HTTP PUT client with resume
        receiver.rs                # axum PUT/HEAD handlers
        hasher.rs                  # Streaming SHA-256 with progress
        progress.rs                # Rate-limited progress reporting
        resume.rs                  # .partial.meta handling
        adapter.rs                 # FileTransferAdapter (signaling over MessageBus)
      store_sync/
        mod.rs
        adapter.rs                 # StoreSyncAdapter
        types.rs                   # DeviceSlice, sync payloads
      reverse_proxy/
        mod.rs                     # HTTP reverse proxy with WebSocket upgrade passthrough
      protocol/
        mod.rs
        envelope.rs                # MeshEnvelope, MeshMessage
        message_types.rs           # Device/Election/Route payloads
        codec.rs                   # MeshEnvelope serde (MessagePack with JSON debug fallback, no length framing — WS provides it)
        hostname.rs                # generateHostname, parseHostname
      types/
        mod.rs                     # BaseDevice, DeviceRole, DeviceStatus, etc.

  truffle-napi/                    # NAPI-RS v3 cdylib for Node.js
    Cargo.toml                     # crate-type = ["cdylib"]
    build.rs                       # napi_build::setup()
    src/
      lib.rs
      mesh_node.rs                 # #[napi] MeshNode wrapper
      message_bus.rs               # #[napi] MessageBus wrapper
      file_transfer.rs             # #[napi] FileTransferAdapter wrapper
      store_sync.rs                # #[napi] StoreSyncAdapter wrapper
      types.rs                     # NAPI type conversions

  truffle-tauri-plugin/            # Tauri v2 plugin
    Cargo.toml
    build.rs
    src/
      lib.rs                       # init<R>() -> TauriPlugin<R>
      commands.rs                  # #[tauri::command] handlers
      events.rs                    # Event emission to frontend
    permissions/
      default.toml
    guest-js/
      index.ts                     # @truffle/tauri-plugin JS bindings

packages/                          # npm packages (stays TypeScript)
  @vibecook/truffle/               # Main package — loads NAPI native addon
  @vibecook/truffle-react/         # React hooks (stays TypeScript)
  sidecar-slim/                    # New Go thin shim (replaces current sidecar)
```

### Key Rust Dependencies

```toml
# truffle-core/Cargo.toml
[dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "net", "io-util", "sync", "macros", "time", "fs", "process"] }
axum = "0.8"                          # HTTP server for file transfer receiver (PUT/HEAD)
tokio-tungstenite = "0.28"            # WebSocket over bridged TcpStream
hyper = { version = "1", features = ["client", "http1"] }  # HTTP client for file transfer sender
hyper-util = "0.1"                    # hyper 1.x client utilities
bytes = "1"                           # Zero-copy buffer management (used by tokio/axum/hyper)
sha2 = "0.10"
hex = "0.4"
pin-project-lite = "0.2"             # Safe pin projection for custom AsyncRead wrappers
rmp-serde = "1.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tracing = "0.1"
uuid = { version = "1", features = ["v4"] }
tokio-util = { version = "0.7", features = ["io"] }  # AsyncReadExt utilities; WS framing handled by tokio-tungstenite
rand = "0.9"
subtle = "2"                                     # Constant-time token comparison for bridge auth
nix = { version = "0.29", features = ["fs"] }  # for statvfs disk space check (Unix)

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = ["Win32_Storage_FileSystem"] }  # GetDiskFreeSpaceExW

# truffle-napi/Cargo.toml
[dependencies]
truffle-core = { path = "../truffle-core" }
napi = { version = "3", features = ["async", "napi4", "tokio_rt"] }
napi-derive = "3"
[build-dependencies]
napi-build = "3"
```

---

## 3. Key Design Patterns

### Connection Routing

Rust runs a **single** local `TcpListener`. Each accepted connection is immediately spawned into its own task to avoid blocking the accept loop. A semaphore caps concurrent bridge tasks to prevent resource exhaustion from bugs or misbehavior:

```rust
let semaphore = Arc::new(Semaphore::new(256)); // max concurrent bridge connections

loop {
    let (stream, _) = listener.accept().await?;
    let permit = semaphore.clone().acquire_owned().await?;
    let handlers = handlers.clone();
    tokio::spawn(async move {
        let _permit = permit; // held for task lifetime, released on drop

        // Header must arrive within 2s (prevents slowloris)
        let header = match tokio::time::timeout(
            Duration::from_secs(2),
            BridgeHeader::read_from(&mut stream),
        ).await {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => { tracing::warn!("bad bridge header: {e}"); return; }
            Err(_) => { tracing::warn!("bridge header timeout"); return; }
        };

        // Verify session token (constant-time compare via `subtle` crate)
        if subtle::ConstantTimeEq::ct_eq(
            &header.session_token, &expected_token
        ).unwrap_u8() != 1 {
            tracing::warn!("invalid bridge session token"); return;
        }

        match (header.service_port, header.direction) {
            (443, Incoming)  => handlers.ws.handle_incoming(stream, header).await,
            (443, Outgoing)  => handlers.ws.handle_outgoing(stream, header).await,
            (9417, Incoming) => handlers.ft.handle_incoming(stream, header).await,
            (9417, Outgoing) => handlers.ft.handle_outgoing(stream, header).await,
            (port, _)        => handlers.proxy.handle(stream, port, header).await,
        }
    });
}
```

### Outgoing Dial Correlation

When Rust wants to connect to a peer:
1. Rust generates a `requestId` (UUID v4) and creates a `oneshot::channel()`
2. **Critical ordering**: Insert the oneshot sender into `pending_dials` map **before** sending the `bridge:dial` command to Go. This prevents a race where Go dials fast and the accept loop reads the header before the map entry exists.
3. Sends `bridge:dial` command to Go with the requestId
4. Go dials tsnet, connects to Rust's local port, sends header with requestId
5. Rust's accept loop reads the header, finds the pending dial by requestId, delivers the `TcpStream` via the oneshot channel
6. Original caller receives the stream and upgrades to WebSocket/HTTP

**Dial timeout and cleanup**: The oneshot receiver must be wrapped in a timeout to prevent leaked channels when Go fails to connect (target unreachable, tsnet error). On the Go side, `bridge:dialResult` with an error triggers cleanup via the command channel. On the Rust side:

```rust
// Step 1: insert BEFORE sending command (prevents race)
pending_dials.insert(request_id.clone(), tx);

// Step 2: send command to Go
shim.send_command(BridgeDial { request_id: request_id.clone(), target, port }).await?;

// Step 3: wait with timeout
let stream = tokio::select! {
    result = rx => result.map_err(|_| BridgeError::DialCancelled)?,
    _ = tokio::time::sleep(Duration::from_secs(30)) => {
        pending_dials.remove(&request_id);
        return Err(BridgeError::DialTimeout(Duration::from_secs(30)));
    }
};
```

**Cleanup paths** (all must remove from `pending_dials`):
- **Timeout**: 30s timer fires → remove entry, return error (shown above)
- **Go dial failure**: `bridge:dialResult` error event on command channel → remove entry, drop sender → receiver gets `RecvError`
- **Shim crash**: GoShim crash recovery drains all pending_dials (see crash recovery section)
- **Unknown requestId on accept**: If the accept loop reads a header with an outgoing requestId not in `pending_dials`, close the connection and log a warning (protocol invariant violation)

### Go Shim Crash Recovery

The `GoShim` module must handle unexpected shim termination (OOM, panic, segfault):

1. **Monitor**: `tokio::process::Child::wait()` in a spawned task detects exit
2. **Drain**: On unexpected exit, iterate all `pending_dials` and drop senders (unblocking callers with errors), notify all active bridged connections that the bridge is down
3. **Notify**: Emit a `ShimCrashed { exit_code, stderr_tail }` event via the broadcast channel so NAPI/Tauri layers can inform the application
4. **Restart** (optional): Auto-restart with exponential backoff (1s, 2s, 4s, max 30s). After restart, re-send `tsnet:start` to restore the tailnet connection. Active WebSocket/file-transfer connections cannot be recovered — they must reconnect via the transport layer's existing reconnect logic
5. **Restart storm prevention**: If the shim crashes repeatedly with `tsnet:authRequired` as the last event before each crash, **pause auto-restart** and emit a `ShimAuthRequired` event to the application. The user must complete Tailscale authentication before the shim can function — restarting without auth just wastes resources and spams logs. Resume auto-restart only after the application explicitly requests it or the auth state changes.
6. **Kill on drop**: `kill_on_drop(true)` ensures the shim is terminated when the Rust process exits

### Event Delivery (NAPI)

#### Channel Strategy: mpsc for Critical, broadcast for High-Rate

`tokio::sync::broadcast` has a bounded ring buffer. When a slow receiver lags behind the sender, `recv()` returns `Lagged(n)` — messages are **silently lost**. This is unacceptable for critical mesh events but fine for high-rate progress updates.

**Two-channel design**:

| Channel | Type | Events | Drop policy |
|---|---|---|---|
| Critical | `tokio::sync::mpsc` (per subscriber, bounded 256) | Device join/leave, election results, shim crash, connection state, sync state | **Never drop**. Backpressure propagates to sender. If the channel is full, the sender `.await`s. |
| Progress | `tokio::sync::broadcast` (ring buffer 64) | File transfer progress, heartbeat stats | **Drops allowed**. If receiver lags, it receives `Lagged(n)` and skips to the latest. The next progress event provides current state. |

The NAPI binding subscribes to both channels and forwards events to Node.js via `ThreadsafeFunction`. Events are passed as structured NAPI objects (not JSON strings) to avoid a double serialization round-trip:

```rust
#[napi(object)]
pub struct MeshEvent {
    pub event_type: String,
    pub device_id: Option<String>,
    pub payload: serde_json::Value, // maps directly to JS object
}

#[napi]
pub fn on_event(&self, callback: ThreadsafeFunction<MeshEvent>) {
    let mut critical_rx = self.inner.subscribe_critical();   // mpsc::Receiver
    let mut progress_rx = self.inner.subscribe_progress();   // broadcast::Receiver
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                Some(event) = critical_rx.recv() => event,
                Ok(event) = progress_rx.recv() => event,
                else => break,
            };
            let napi_event = MeshEvent::from(event);
            // Blocking mode: if Node.js event queue is full, wait rather than drop.
            callback.call(Ok(napi_event), ThreadsafeFunctionCallMode::Blocking);
        }
    });
}
```

**Lag recovery for progress channel**: When `progress_rx.recv()` returns `Lagged(n)`, the loop continues — the next successful recv delivers the latest progress state. No resync needed since progress events are self-contained snapshots (current bytes / total bytes).

**Important NAPI notes**:
- **No panics in NAPI addon code.** Both `.unwrap()` and `.expect()` panic — and a panic in a NAPI addon crashes the entire Node.js process with no recovery. All fallible operations must return `Result` and propagate errors via NAPI's error mechanism. If a conversion that "should never fail" does fail, emit an error event on the critical channel and skip the message — never panic.
- All NAPI-exported async functions must be non-blocking to prevent deadlocks with `ThreadsafeFunctionCallMode::Blocking`. If a JS callback blocks the event loop while Rust tries to deliver an event via Blocking TSFN, and the callback tries to call back into Rust synchronously, you get a deadlock. Mitigation: all `#[napi]` methods that trigger event emission must be `async` (not blocking).
- The tokio runtime is managed by NAPI-RS's `tokio_rt` feature (shared multi-threaded runtime, created on first use, lives for the process lifetime). `truffle-core` must **not** create its own global runtime — it accepts a runtime handle or uses `tokio::spawn` which works on the current runtime. This ensures the Tauri plugin (which has its own runtime) is not affected.

### File Transfer in Rust

Ports RFC 002's design 1:1 with Rust equivalents:

| Go | Rust |
|---|---|
| `http.ServeMux` PUT/HEAD handlers | `axum::Router` with PUT/HEAD routes |
| `io.Copy` + `io.TeeReader(body, sha256)` | `tokio::io::copy` with custom `AsyncRead` (via `pin-project-lite`) that feeds `sha2::Sha256` |
| `cancellableReader` checking atomic state | `AsyncRead` wrapper checking `CancellationToken` (uses `pin-project-lite` for safe pin projection) |
| `atomic.CompareAndSwapInt32` for state | `AtomicU8` with `compare_exchange` |
| `sync.Mutex` for progress fields | `tokio::sync::Mutex` or `parking_lot::Mutex` |
| `os.Stat` + `syscall.Statfs` | `tokio::fs::metadata` + `nix::sys::statvfs` (Unix) / `windows-sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW` (Windows, behind `#[cfg(windows)]`) |
| `http.Client` with custom `DialContext` | `hyper::client::conn::http1` over bridged `TcpStream` (no reqwest — one-shot connections don't need pooling) |

For sending: Rust reads file, sends HTTP PUT over a bridged connection (requested via `bridge:dial` to target:9417). For receiving: Rust's axum handler processes the PUT on incoming bridged connections for port 9417.

### Wire Protocol Decision: MessagePack in WebSocket Binary Frames

The existing TypeScript `FrameCodec` (5-byte length-prefix header + MessagePack/JSON serialization) was built but **never wired into the transport layer** — the current transport sends raw JSON strings over WebSocket text frames.

**Decision**: The Rust rewrite uses MessagePack serialization inside WebSocket binary frames. One WS binary frame = one MessagePack-encoded `MeshEnvelope`. Benefits:
- ~30% smaller payloads vs JSON for structured device/election/routing messages
- ~3x faster serialization/deserialization (rmp-serde vs serde_json)
- WebSocket already provides message framing (each WS message is a discrete unit), so the FrameCodec's 5-byte length prefix is **not needed** over WS — it would be redundant overhead

**Why no length prefix over WebSocket**: The original `FrameCodec` was designed for raw TCP stream framing where message boundaries don't exist. WebSocket binary frames already delimit messages. Adding a length prefix inside WS frames adds unnecessary bytes and parse logic. The `codec.rs` module in `truffle-core` still implements MessagePack serialization/deserialization of `MeshEnvelope`, but delegates framing to `tokio-tungstenite`'s WS layer.

**JSON debug mode**: Controlled by a 1-byte flags prefix on the WS binary payload (0x00 = MessagePack, 0x02 = JSON). This enables human-readable inspection during development without a separate code path. In production, always MessagePack.

**Migration note**: This is a **wire-incompatible change**. Rust nodes cannot communicate with old TypeScript nodes. Since the rewrite replaces all nodes simultaneously (Phase 6 swaps the NAPI backend), this is acceptable. There is no mixed Rust/TS node scenario. No rolling upgrade is supported — all nodes must upgrade together.

**Version detection**: The first mesh message after WS handshake includes a protocol version field in the `MeshEnvelope`. If a peer sends an unknown version, the connection is dropped with a clear log message. This catches version skew early if nodes are somehow mismatched (e.g., partial deployment).

### Library Roles Clarification

Three HTTP/WS libraries serve distinct roles:
- **`tokio-tungstenite`**: WebSocket protocol over bridged `TcpStream` (port 443). Handles the WS handshake, frame encoding/decoding, ping/pong. Used for both incoming and outgoing mesh connections.
- **`axum`** (without `ws` feature): HTTP server for file transfer receiver (PUT/HEAD routes on port 9417). Does **not** handle WebSocket.
- **`hyper`**: HTTP/1.1 client for file transfer sender. Sends HTTP PUT over bridged `TcpStream` to target:9417. Used instead of `reqwest` because bridged connections are one-shot — reqwest's connection pooling, cookie handling, and redirect following are unnecessary overhead.

**Per-stream serving model**: Since file transfer connections arrive as pre-accepted `TcpStream`s from the BridgeManager (not via a `TcpListener` that axum binds), we use hyper's per-connection API directly. **Keep-alive is disabled** — each bridged connection carries exactly one HTTP request (PUT or HEAD). After the response, the connection closes.

```rust
// For each incoming bridged stream on port 9417:
let service = axum_router.into_service(); // axum Router → hyper Service
hyper::server::conn::http1::Builder::new()
    .keep_alive(false)  // one request per bridge connection
    .serve_connection(stream, service)
    .await?;
```

Why no keep-alive: each bridged connection has a dedicated Go-side goroutine pair doing `io.Copy`. Reusing the HTTP connection would require the Go side to keep the goroutines alive indefinitely. Since Go creates a fresh bridge connection per tsnet connection, and each file transfer operation (send/resume-check) is a separate tsnet dial, one-request-per-connection is natural.

**WebSocket max message size**: `tokio-tungstenite` is configured with `max_message_size = 16 * 1024 * 1024` (16MB, matching the old FrameCodec `MAX_MESSAGE_SIZE`). Peers sending messages exceeding this limit are disconnected. This prevents a malicious or buggy peer from causing OOM with a single oversized WS message.

### Port 443 Multiplexing

The current Go server multiplexes multiple services on port 443 via HTTP path routing:
- `/ws` → mesh WebSocket (gorilla/websocket upgrade)
- `/health`, `/status`, `/vapid` → JSON API endpoints
- `/` → PWA static files with SPA fallback
- Reverse proxy → separate `ListenTLS` per configured port (not on 443 itself)

In the Rust rewrite, **all port 443 bridged connections arrive at the same handler**. Since they're already decrypted (TLS terminated in Go), Rust sees raw HTTP bytes. The demux strategy:

**Phase 2 (WebSocket only)**: All port 443 connections go through an HTTP handler that checks the request path:
- `GET /ws` with `Upgrade: websocket` → hand off to `tokio-tungstenite` for WS upgrade
- All other paths → return 404 (no PWA serving yet)

**Phase 8 (full multiplex)**: Port 443 handler becomes a `hyper` service that routes by request:

```rust
async fn handle_443(req: Request<Body>, stream: TcpStream) -> Response<Body> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/ws") if is_websocket_upgrade(&req) => {
            // Upgrade to WebSocket, hand to mesh transport
            ws_upgrade(req, stream).await
        }
        (&Method::GET, "/health" | "/status") => {
            // JSON status endpoints
            status_handler(req).await
        }
        _ => {
            // PWA static files with SPA fallback, or reverse proxy
            pwa_or_proxy_handler(req).await
        }
    }
}
```

This means port 443 connections use `hyper::server::conn::http1::serve_connection` (same as file transfer) rather than direct `tokio-tungstenite` accept. The WS upgrade happens *after* HTTP request parsing, inside the hyper service handler.

**Reverse proxy on other ports**: The current Go system creates separate TLS listeners per reverse-proxy port (e.g., 8443 → localhost:5173). In the Rust rewrite, these use additional tsnet listen ports — Go bridges them as separate `ServicePort` values, and Rust's routing dispatches to the `reverse_proxy` handler for unknown ports (the `(port, _) => handlers.proxy.handle(...)` catch-all in the accept loop already handles this).

---

## 4. Migration Phases

### Phase 1: Bridge Infrastructure
**Build**: Go thin shim + `truffle-core/src/bridge/`
**Test**: Two shim instances on a test tailnet, Rust echoes bytes
**Files**:
- New: `packages/sidecar-slim/main.go` (~500 LOC)
- New: `crates/truffle-core/src/bridge/*.rs`

### Phase 2: WebSocket Transport
**Build**: ConnectionManager, WS pumps, heartbeat, reconnect
**Port from**: `packages/transport/src/index.ts` (632 LOC) + `packages/sidecar/internal/server/websocket.go` (218 LOC) + `dialer.go` (273 LOC)
**Test**: Two nodes connect via WS, exchange messages
**Files**:
- New: `crates/truffle-core/src/transport/*.rs`

### Phase 3: Mesh Protocol
**Build**: MeshNode, DeviceManager, PrimaryElection, MeshMessageBus, FrameCodec
**Port from**: `packages/mesh/src/*.ts` (~1,950 LOC: mesh-node 833, device-manager 318, election 319, message-bus 123, file-transfer-adapter 354) + `packages/protocol/src/` (~300 LOC) + `packages/types/src/index.ts` (668 LOC)
**Test**: Three nodes elect primary, route messages through STAR topology
**Files**:
- New: `crates/truffle-core/src/mesh/*.rs`, `src/protocol/*.rs`, `src/types/*.rs`

### Phase 4: File Transfer
**Build**: FileTransferManager, sender, receiver, hasher, progress, resume, adapter
**Port from**: `packages/sidecar/internal/filetransfer/` (~1,360 LOC source, ~2,800 including tests) + `packages/mesh/src/file-transfer-adapter.ts` (354 LOC)
**Test**: Send file between nodes, verify SHA-256, test resume, test cancel
**Files**:
- New: `crates/truffle-core/src/file_transfer/*.rs`

### Phase 5: Store Sync
**Build**: StoreSyncAdapter
**Port from**: `packages/store-sync/src/index.ts` (342 LOC)
**Test**: Two nodes sync state
**Files**:
- New: `crates/truffle-core/src/store_sync/*.rs`

### Phase 6: NAPI-RS Bindings
**Build**: Node.js native addon, TypeScript type definitions
**Test**: Run existing TypeScript tests against NAPI bindings
**Files**:
- New: `crates/truffle-napi/src/*.rs`
- Modified: `packages/core/` — re-export from NAPI addon instead of TS packages

### Phase 7: Tauri Plugin + Example App
**Build**: Tauri v2 plugin, demo desktop app
**Files**:
- New: `crates/truffle-tauri-plugin/src/*.rs`
- New: `apps/desktop/` — example Tauri app

### Phase 8: Reverse Proxy + Cleanup

**Reverse proxy** (382 LOC in current Go, non-trivial — includes WebSocket HMR via TCP hijack): Port to Rust as a `reverse_proxy` module in `truffle-core`. Use `hyper`'s `http1::Builder` for HTTP proxying and `tokio::io::copy_bidirectional` for WebSocket upgrade passthrough. This avoids keeping a second Go binary or bloating the slim shim.

**Build**: `truffle-core/src/reverse_proxy/` — HTTP reverse proxy with WebSocket upgrade support
**Port from**: `packages/sidecar/internal/server/reverseproxy.go` (382 LOC)
**Test**: Proxy HTTP requests and WebSocket connections to a local server

**Cleanup**:
- Remove old TypeScript packages (mesh, transport, protocol, store-sync, sidecar-client)
- Remove old Go sidecar
- Update docs, examples, CI

---

## 5. What Stays TypeScript

- `packages/react/` — React hooks (`useMesh`, `useSyncedStore`). These are thin wrappers, no reason to port.
- Auto-generated `.d.ts` from NAPI-RS replaces hand-written type packages.

## 6. What Stays Go

- `internal/tsnet/node.go` (400 LOC) — unchanged, the tsnet wrapper
- New proxy logic (~100 LOC) — transparent TCP bridging
- Total: ~500 LOC, trivially maintainable

**Note**: The reverse proxy (PWA serving, WebSocket HMR passthrough) moves to Rust in Phase 8. The Go shim handles **only** tsnet lifecycle and TCP bridging — no HTTP logic.

---

## 7. Security Model

### Transport Security

Tailscale provides the outer security boundary: all inter-node traffic is encrypted by WireGuard. The Go shim terminates TLS on port 443 using Tailscale-provisioned certificates (via `tsnet.ListenTLS`). Port 9417 runs plain TCP — WireGuard encryption is sufficient since file transfers are already within the tailnet.

The local bridge (Go ↔ Rust) carries decrypted bytes over `127.0.0.1` TCP. This is acceptable because:
- Both processes run under the same user on the same machine
- The bridge **must** bind to `127.0.0.1` (not `0.0.0.0`) — this is a hard invariant, not optional
- The bridge port is ephemeral (`127.0.0.1:0`) and short-lived
- A per-session 32-byte auth token prevents other local processes from injecting into the bridge (see header format). The token is compared using constant-time comparison (`subtle::ConstantTimeEq`) to eliminate timing side channels. The token is regenerated on every start and never logged.

### Peer Identity

Rust identifies remote peers via two mechanisms:

1. **Bridge header `RemoteDNSName`** (immediate): Go resolves the remote Tailscale IP to a peer DNS name at accept time via `LocalClient().Status()`. This gives Rust immediate peer identity without waiting for application-layer handshake. Connections from IPs not present in the tailnet peer list get an empty `RemoteDNSName` — Rust should accept these (the peer may have just joined) but flag them for verification via the mesh announce protocol.

2. **Mesh announce protocol** (authoritative): After WebSocket upgrade, the first mesh message from a peer contains its `deviceId` and identity claims. Rust cross-references this with the `RemoteDNSName` from the bridge header and the peer list from `tsnet:getPeers`. Mismatches are logged and the connection is dropped.

This two-layer approach means Rust never processes mesh messages from completely unknown peers (header filtering) while still supporting the existing device discovery protocol for identity binding.

### What TLS Termination Implies

- Rust does **not** see TLS certificates or perform certificate validation — Go handles this entirely
- Rust cannot independently verify peer TLS identity; it trusts Go's `RemoteDNSName` field
- If the Go shim is compromised, the bridge is compromised — this is inherent to the shim architecture and acceptable given both run as the same user/process tree

---

## 8. Verification & Testing

### Per-Phase Testing
- **Bridge**: Two Go shims on a test tailnet, Rust processes on each side, verify raw byte roundtrip. Also test with a **mock Go shim** (speaks the same stdin/stdout + TCP bridge protocol over regular TCP) for CI without Tailscale.
- **Transport**: Two MeshNodes connect via WebSocket, verify heartbeat keeps connection alive, verify reconnect after disconnect
- **Mesh**: Three-node cluster: verify election (longest uptime wins), verify STAR routing (secondary → primary → target), verify device discovery
- **File transfer**: Send files of various sizes (1KB, 100MB, 1GB), verify SHA-256 match, kill connection mid-transfer and resume, cancel mid-transfer and verify cleanup
- **Store sync**: Two nodes with local state changes, verify cross-device sync propagation
- **NAPI**: Run existing Vitest test suite against NAPI bindings (should be drop-in compatible)

### Hardening Tests

**Bridge header fuzz / malformed input**:
- Random bytes (non-magic prefix) — must reject within 2s timeout
- Oversized `RequestIdLen` / `RemoteAddrLen` beyond max limits — must reject immediately
- Incomplete header (EOF mid-field) — must reject on read error
- Slowloris header (bytes trickle one at a time) — must reject after 2s timeout
- Invalid session token — must reject and log
- Unknown version byte — must reject and log

**Crash recovery**:
- `kill -9` the Go shim while: idle, active WS connection, dial in-flight, file transfer mid-stream
- Verify: pending dials resolve with error, reconnection logic kicks in, no deadlocks, no task explosion
- Verify: `ShimCrashed` event emitted, auto-restart with backoff, tailnet reconnects

**Event channel lag**:
- Simulate slow Node.js event handler (sleep 200ms per callback) while sending 1000+ events rapidly
- Verify: critical events (mpsc) are never dropped — backpressure propagates
- Verify: progress events (broadcast) may lag — receiver gets `Lagged(n)` and recovers on next event

**Dial correlation edge cases**:
- Go connects with unknown `requestId` — must close connection and log
- Rust sends `bridge:dial` and Go fails immediately — must clean up pending entry via `bridge:dialResult` error
- Two concurrent dials to same target — must use separate `requestId`s and not interfere

**Bridge protocol golden tests** (cross-language correctness):
- Fixed header byte sequences → parsed struct (Go writes known bytes, Rust reads and verifies fields match)
- Struct → serialized bytes (Rust writes, verified against expected byte sequence)
- Shared test vectors file consumed by both Go `_test.go` and Rust `#[test]` to prevent protocol drift

**Connection lifecycle tests**:
- Remote peer closes → Go bridge detects EOF, closes both directions, Rust handler task terminates
- Rust handler closes → Go bridge detects write error, closes tsnet connection, goroutines exit
- Neither side sends data for 60s → Go write deadline fires, both sides close
- Verify no goroutine or task leaks after 100 connect/disconnect cycles

**Resource exhaustion tests**:
- 256 concurrent connections (semaphore limit) → 257th blocks until one completes
- Many connections with valid tokens but no payload after header → must not accumulate indefinitely (handler-level idle timeout)
- Oversized WS message (>16MB) → peer disconnected, no OOM

### End-to-End Test
```bash
# Build Go shim
cd packages/sidecar-slim && go build -o truffle-shim

# Build NAPI addon
cd crates/truffle-napi && napi build --release

# Run existing example
cd examples/discovery && npx tsx index.ts
```

### CI
- Rust: `cargo test --workspace` (unit + integration)
- Go shim: `cd packages/sidecar-slim && go test ./...`
- Node.js: `pnpm test` (existing Vitest suite, now backed by NAPI)
- Cross-compile: NAPI-RS CI matrix for darwin-arm64, darwin-x64, linux-x64, win32-x64

---

## 9. Risk Assessment

| Risk | Mitigation |
|---|---|
| tsnet virtual networking (no FD passing) | Transparent TCP proxy; ~10-50µs per message overhead, negligible vs WireGuard ~1ms latency. Port 9417 (plain TCP) may use `splice(2)` on Linux; port 443 (TLS-terminated) cannot |
| Go shim process crash | Monitor via `Child::wait()`, drain pending dials, emit `ShimCrashed` event, auto-restart with exponential backoff (1s-30s). Active connections rely on transport-layer reconnect logic |
| NAPI-RS learning curve | SWC/Rspack/Rolldown prove the pattern at scale; v3 docs are comprehensive |
| File transfer state machine complexity | RFC 002 is a complete spec; port 1:1 with Rust enums (better than Go atomics) |
| Windows FD differences | Bridge uses local TCP (works on all platforms); no FD passing needed |
| Tauri + Go shim coexistence | Tauri app spawns Go shim same way Node.js does; Rust core is shared |
