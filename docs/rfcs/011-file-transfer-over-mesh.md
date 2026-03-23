# RFC 011: File Transfer Over Mesh Connections

**Status**: Proposed
**Created**: 2026-03-22
**Depends on**: RFC 008 (Vision), RFC 009 (Wire Protocol), RFC 010 (P2P Redesign)
**Supersedes**: RFC 010 Section 2.6 (Taildrop for `truffle cp`)
**Related**: RFC 001 (File Transfer Primitive), RFC 002 (Native Go File Transfer)

---

## 1. Problem Statement

`truffle cp` is currently broken. The daemon handler is a stub that acknowledges receipt without transferring any bytes. RFC 010 proposed using Taildrop (Tailscale's built-in file transfer) but Taildrop has fundamental limitations that make it unsuitable as truffle's primary file transfer mechanism:

### 1.1 Taildrop Limitations

| Limitation | Impact |
|---|---|
| **Queue-based model** | Files go to a "waiting files" queue, not directly to the target path. Receiver must poll `WaitingFiles()`, then `GetWaitingFile()`, then `DeleteWaitingFile()`. Three round-trips per file. |
| **No target path control** | Sender cannot specify where the file lands on the receiver. Receiver gets a blob with a filename, not a path. This breaks `truffle cp file.txt server:/opt/data/`. |
| **No progress reporting** | `lc.PushFile()` is a blocking call with no progress callback. The Go sidecar's handler (line 829) calls `PushFile` and waits. No progress events reach the CLI. |
| **No resume support** | If the connection drops mid-transfer, the entire file must be re-sent. |
| **5-minute timeout** | The sidecar hardcodes `context.WithTimeout(s.ctx, 5*time.Minute)`. Files larger than what can transfer in 5 minutes will fail. |
| **Requires `tailcfg.StableNodeID`** | The sender needs the receiver's stable node ID, not its DNS name or device ID. This requires an extra lookup step. |
| **No pull (download) support** | Taildrop is push-only. `truffle cp server:/file.txt ./` has no implementation path with Taildrop alone. |

### 1.2 What Already Exists

Truffle already has a complete, production-quality file transfer system in `truffle-core`:

- **`FileTransferManager`** (`services/file_transfer/manager.rs`, 907 lines) -- full transfer lifecycle, state machine, SHA-256 verification, partial files, progress reporting, cleanup sweeps
- **`FileTransferAdapter`** (`services/file_transfer/adapter.rs`, 938 lines) -- control plane: OFFER/ACCEPT/REJECT/CANCEL signaling over MessageBus
- **Receiver** (`services/file_transfer/receiver.rs`, 401 lines) -- axum HTTP server with PUT/HEAD endpoints, Content-Range resume, disk space checks
- **Sender** (`services/file_transfer/sender.rs`, 332 lines) -- hyper HTTP client, resume offset query, streaming upload with progress
- **Types** (`services/file_transfer/types.rs`, 765 lines) -- Transfer state machine, FileInfo, error codes, config
- **Supporting modules** -- hasher, progress, resume (partial meta files)

This system was designed per RFC 002 and fully implemented in Rust. It uses HTTP PUT over direct TCP connections (port 9417) with resume, SHA-256 integrity, concurrent transfer limiting, and a complete error code taxonomy.

The problem: **it is not wired to the CLI**. The `truffle cp` command talks to the daemon via JSON-RPC, and the daemon handler is a stub. The `FileTransferManager` + `FileTransferAdapter` system needs a dial function to establish TCP connections through the Go sidecar, and the CLI needs a way to drive it.

### 1.3 Design Question

Should `truffle cp` use:
- **(A) Taildrop** -- Tailscale's built-in, via `lc.PushFile()` / `lc.WaitingFiles()`
- **(B) Direct TCP** -- truffle's own HTTP PUT system over `tsnet.Dial()` connections
- **(C) Hybrid** -- Taildrop for simple push, custom for everything else

This RFC argues for **(B) Direct TCP** as the primary mechanism, with Taildrop as a possible future fallback for interop with non-truffle Tailscale nodes.

---

## 2. Prior Art: How Others Solve This

### 2.1 croc

[croc](https://github.com/schollz/croc) (Go, ~15k stars) -- P2P file transfer with relay fallback.

- **Transport**: Raw TCP sockets or WebSocket, via relay servers. Direct P2P on LAN.
- **Chunking**: Splits files into chunks, only sends changed chunks (rsync-like).
- **Resume**: Detects partial files and resumes automatically.
- **Encryption**: PAKE (password-authenticated key exchange) using code phrases.
- **Relevance to truffle**: croc must solve NAT traversal and encryption; truffle gets both free from Tailscale. croc's relay model is unnecessary when every peer is directly reachable.

### 2.2 magic-wormhole

[magic-wormhole](https://github.com/magic-wormhole/magic-wormhole) (Python) -- one-shot file transfer.

- **Architecture**: Mailbox server for signaling (PAKE key exchange), then Transit object for bulk data.
- **Transit**: Tries direct TCP first, falls back to Transit Relay. Both sides share IPs inside encrypted messages.
- **File protocol**: Sender sends offer (filename, size), receiver sends answer (accepted), then raw bytes flow.
- **Relevance to truffle**: Similar control-plane/data-plane split. Truffle's MessageBus = wormhole's mailbox. Truffle's HTTP PUT = wormhole's Transit. But truffle already has persistent connections; wormhole establishes them per-transfer.

### 2.3 Syncthing (BEP)

[Syncthing](https://docs.syncthing.net/specs/bep-v1.html) (Go, ~70k stars) -- continuous file synchronization.

- **Block Exchange Protocol (BEP)**: Files described as blocks of 128 KiB to 16 MiB (powers of two), block size varies by file size.
- **Hashing**: SHA-256 per block. Only blocks with mismatched hashes are transferred. This is delta-sync, not bulk transfer.
- **Protocol**: Protobuf over TLS. Asynchronous -- any message can be sent at any time, no request-response ordering required.
- **Relevance to truffle**: Syncthing's block-level hashing is overkill for truffle's use case (one-shot transfers, not continuous sync). But its variable block sizing strategy is smart: 128 KiB for small files, 16 MiB for large files. Truffle should adopt a similar approach for chunked hashing during resume.

### 2.4 Taildrop (Tailscale built-in)

- **Transport**: Uses Tailscale's PeerAPI (HTTP over WireGuard) on a per-node port.
- **Model**: Push-only. Files land in a waiting queue. Receiver polls for files.
- **Resume**: None.
- **Progress**: None (opaque `io.Copy`).
- **API**: `lc.PushFile(ctx, stableNodeID, size, name, reader)`. Simple but limited.

### 2.5 Comparison Matrix

| Feature | croc | magic-wormhole | Syncthing | Taildrop | Truffle (RFC 002) |
|---|---|---|---|---|---|
| Direct P2P | LAN only | Tries first | Always | Always (WG) | Always (WG) |
| Encryption | PAKE | PAKE | TLS | WireGuard | WireGuard |
| Resume | Yes | No | Yes (blocks) | No | Yes (Content-Range) |
| Progress | Yes | Limited | N/A (sync) | No | Yes (events) |
| Target path | No | receiver decides | Sync folder | No (queue) | Yes (save_path) |
| Pull (download) | No | No | N/A | No | Yes (can reverse) |
| Integrity | SHA-256 | Yes | SHA-256/block | No | SHA-256 whole-file |
| Chunking | Yes | No | Yes (128K-16M) | No | No (streaming) |
| NAT traversal | Relay | Transit relay | Relay | DERP relay | DERP relay |

**Conclusion**: Truffle's existing RFC 002 system is already more capable than any of the alternatives in the features that matter for truffle's use case. The only missing piece is wiring it to the CLI.

---

## 3. Proposed Design: Direct TCP Over Mesh

### 3.1 Architecture

```
CLI Layer:
  truffle cp file.txt server:/tmp/     CLI parses args, connects to daemon
        |
        v
Daemon Layer (JSON-RPC over Unix socket):
  handle_cp_send()                     Daemon handler in handler.rs
        |
        v
Service Layer:
  FileTransferAdapter                  Control plane: OFFER/ACCEPT signaling
  FileTransferManager                  Data plane: HTTP PUT/HEAD on :9417
        |
        v
Transport Layer:
  DialFn -> bridge:dial -> Go sidecar  Establishes TCP to peer's :9417
        |
        v
Network Layer:
  tsnet.Dial("tcp", "peer:9417")       Raw TCP over WireGuard tunnel
        |
        v
Receiver:
  axum HTTP server on :9417            Receives PUT, streams to disk
```

### 3.2 Data Flow: Upload (`truffle cp file.txt server:/tmp/`)

```
Sender CLI                 Sender Daemon              Receiver Daemon
──────────                 ─────────────              ───────────────

1. Parse args
2. Connect to daemon
3. Send JSON-RPC:
   cp_send {
     local_path: "file.txt",
     target_node: "server",
     remote_path: "/tmp/"
   }
                           4. Resolve "server" to device_id
                           5. manager.prepare_file()
                              - stat file
                              - compute SHA-256
                              - emit Prepared event
                           6. Generate transfer_id, token
                           7. Send OFFER via MessageBus --> 8. Receive OFFER
                              {transfer_id, file info,       Auto-accept (CLI mode)
                               sender_addr, token}           No user confirmation needed
                                                          9. manager.register_receive()
                                                         10. Send ACCEPT via MessageBus
                           11. Receive ACCEPT
                           12. manager.register_send()
                           13. Spawn send_file() task
                              - dial_fn("peer:9417")
                              - HEAD for resume offset
                              - HTTP PUT with streaming body
                              - Progress events flow         14. Receive PUT
                                                                - Stream to .partial
                                                                - TeeReader for SHA-256
                                                                - Progress events
                                                             15. SHA-256 verify
                                                             16. Rename .partial -> final
                                                             17. HTTP 200 OK
                           18. Transfer complete
                           19. Emit Complete event
                           20. Stream result to CLI

21. Print "100% -- 45.2 MB/s"
```

### 3.3 Data Flow: Download (`truffle cp server:/tmp/file.txt ./`)

Downloads are trickier because the file is on the remote side. Two approaches:

**Option A: Reverse the roles (receiver-initiates)**

The CLI daemon sends a "pull request" message via MessageBus. The remote daemon (which has the file) becomes the sender and pushes the file back. This reuses the existing send infrastructure entirely.

```
Local CLI                  Local Daemon               Remote Daemon
─────────                  ────────────               ─────────────

1. cp_pull {
     node: "server",
     remote_path: "/tmp/file.txt",
     local_path: "./"
   }
                           2. Send PULL_REQUEST via     3. Receive PULL_REQUEST
                              MessageBus                4. prepare_file(remote_path)
                                                        5. Send OFFER back to local
                           6. Auto-accept OFFER
                           7. register_receive()
                           8. Send ACCEPT via MessageBus
                                                        9. register_send()
                                                       10. send_file() -> PUT to local:9417
                           11. Receive PUT, stream to disk
                           12. SHA-256 verify
                           13. Complete
```

**Option B: HTTP GET**

Add a GET endpoint to the receiver. Sender requests the file, receiver streams it. This is simpler but requires new HTTP endpoint code.

**Recommendation**: Option A (reverse roles). It requires only one new message type (`PULL_REQUEST`) and reuses 100% of the existing send/receive infrastructure. No new HTTP endpoints needed.

### 3.4 Auto-Accept for CLI Transfers

The existing `FileTransferAdapter` has an OFFER/ACCEPT flow designed for interactive UI (e.g., "Do you want to receive photo.jpg from laptop?"). For CLI transfers, this is unnecessary friction. The design adds a concept of **trusted CLI transfers**:

- When the OFFER includes a `cli_mode: true` flag and a `save_path`, the receiver auto-accepts without user confirmation
- The save_path is validated (absolute, no traversal, parent exists) before accepting
- Auto-accept can be disabled via config for security-sensitive deployments

### 3.5 The DialFn Bridge

The `FileTransferManager.send_file()` needs a `DialFn` to establish TCP connections. This must go through the Go sidecar because only the sidecar has `tsnet.Dial()`.

The bridge already supports `bridge:dial` with a request-response pattern:
1. Rust sends `{ "command": "bridge:dial", "data": { "requestId": "uuid", "target": "peer", "port": 9417 } }`
2. Go dials `tsnet.Dial("tcp", "peer:9417")`
3. Go bridges the resulting connection to a local TCP socket
4. Go responds with `{ "event": "bridge:dialResult", "data": { "requestId": "uuid", "success": true } }`
5. The bridge connection appears as a `TcpStream` in Rust

The `DialFn` wraps this:

```rust
let dial_fn: DialFn = Arc::new(move |addr: &str| {
    let bridge = bridge_manager.clone();
    Box::pin(async move {
        bridge.dial_tcp(addr, 9417).await
    })
});
```

This is already how the existing file transfer adapter is designed to work (see `adapter.rs:323`). The missing piece is the daemon wiring the bridge manager to the file transfer system.

### 3.6 Listening on :9417

The receiver needs an HTTP server on port 9417. Two options:

**Option A: Rust listens directly (current design)**

The `file_transfer_router()` (receiver.rs) creates an axum Router. The daemon starts a `TcpListener` on an ephemeral local port. The Go sidecar's `tsnet:listen` on port 9417 bridges incoming connections to this local port.

**Option B: Go handles HTTP**

The Go sidecar runs the HTTP server directly. This was RFC 002's original design (Go-native data plane).

**Recommendation**: Option A. The Rust implementation already exists, is fully tested, and is more maintainable. The bridge adds minimal overhead (~1 copy) for a file transfer that will be disk-I/O bound anyway.

Wiring:
1. On `truffle up`, the daemon starts the file transfer axum server on a local ephemeral port
2. The daemon tells the sidecar: `tsnet:listen { port: 9417, tls: false }`
3. Incoming connections from the tailnet on :9417 are bridged to the local axum server
4. The sidecar already handles this bridging pattern (same as :443 for WebSocket)

---

## 4. Protocol Details

### 4.1 MessageBus Messages (Control Plane)

Namespace: `file-transfer`

| Message Type | Direction | Payload | Purpose |
|---|---|---|---|
| `OFFER` | Sender -> Receiver | `{ transferId, senderDeviceId, senderAddr, file: {name, size, sha256}, token, cliMode?, savePath? }` | Propose a file transfer |
| `ACCEPT` | Receiver -> Sender | `{ transferId, receiverDeviceId, receiverAddr, token }` | Accept and provide address for data plane |
| `REJECT` | Receiver -> Sender | `{ transferId, reason }` | Decline the transfer |
| `CANCEL` | Either -> Either | `{ transferId, cancelledBy, reason }` | Abort in-progress transfer |
| `PULL_REQUEST` | Requester -> FileOwner | `{ requestId, remotePath, requesterAddr }` | Request a file (for downloads) |

### 4.2 HTTP Protocol (Data Plane)

Port 9417, plain TCP over WireGuard. Already implemented in `receiver.rs`.

| Method | Path | Purpose |
|---|---|---|
| `PUT /transfer/{transferId}` | Upload file bytes | Headers: `X-Transfer-Token`, `X-File-Name`, `X-File-SHA256`, `Content-Length`, `Content-Range` (resume) |
| `HEAD /transfer/{transferId}` | Query resume offset | Returns `Upload-Offset` header |

Response codes: 200 (success), 400 (bad request), 403 (invalid token), 404 (unknown transfer), 409 (conflict/state error), 413 (too large), 500 (disk error), 507 (no space).

### 4.3 Transfer State Machine

Already implemented in `types.rs`:

```
Registered ──> Transferring ──> Completed
    |               |
    v               |── (resumable error: stay in Transferring)
 Cancelled          |
                    v
                  Failed (non-resumable)
```

### 4.4 Resume Protocol

Already implemented in `sender.rs` and `receiver.rs`:

1. Sender sends `HEAD /transfer/{id}` with token
2. Receiver returns `Upload-Offset: <bytes_on_disk>`
3. Sender seeks file to offset, sends `PUT` with `Content-Range: bytes offset-end/total`
4. Receiver appends to `.partial` file, re-hashes existing bytes for SHA-256 continuity
5. On completion, receiver verifies whole-file SHA-256 and renames `.partial` to final path

### 4.5 Progress Reporting

Already implemented via `FileTransferEvent::Progress`:

```rust
FileTransferEvent::Progress {
    transfer_id: String,
    bytes_transferred: i64,
    total_bytes: i64,
    percent: f64,
    bytes_per_second: f64,
    eta: f64,
    direction: String,
}
```

Events are rate-limited by `progress_interval` (500ms) and `progress_bytes` (256KB) to avoid flooding.

For the CLI, progress events from the daemon are streamed back to the CLI process over the Unix socket as JSON-RPC notifications:

```json
{"jsonrpc": "2.0", "method": "cp.progress", "params": {"transfer_id": "ft-abc123", "percent": 45.2, "bytes_per_second": 47400000, "eta": 12.3}}
```

The CLI renders these as a progress bar with speed and ETA.

---

## 5. Handling Large Files (GB+)

### 5.1 Memory Efficiency

The existing implementation is already optimized for large files:

- **Streaming reads**: `ProgressReader` wraps `tokio::fs::File` with a 64KB buffer. No full-file buffering.
- **Streaming writes**: axum body is converted to `AsyncRead` via `StreamReader`. Data flows directly from network to disk.
- **SHA-256 streaming**: `TeeReader` pattern (RFC 002) / `HashingReader` (Rust) computes hash inline during transfer. No second pass.
- **Resume**: `.partial` files preserve progress across failures. For a 10GB file at 50MB/s, a failure at 80% loses only the last chunk, not 8GB of progress.

### 5.2 Large File Considerations

| Concern | Mitigation |
|---|---|
| **Memory usage** | 64KB read buffer + SHA-256 state (~100 bytes). Total: ~65KB per active transfer regardless of file size. |
| **Disk space** | Pre-flight `Statfs` check (receiver.rs line 278). Rejects transfer before any bytes flow if insufficient space. |
| **SHA-256 cost** | ~500 MB/s on modern hardware. A 10GB file takes ~20s to hash. For prepare, this runs in background with progress events. For receive, hashing is inline (zero extra cost). |
| **Resume re-hash** | When resuming, the receiver re-hashes the existing `.partial` to rebuild SHA-256 state. For a 9GB partial, this takes ~18s. Acceptable tradeoff vs. re-transferring 9GB. |
| **Timeout** | No hardcoded timeout on transfer duration. The only timeouts are: header read (10s), idle connection (60s), registered-but-never-started (2min). Active transfers run until completion or cancellation. |
| **Concurrent transfers** | Default `max_concurrent_recv = 5`. Each transfer uses ~65KB + disk I/O bandwidth. 5 concurrent 10GB transfers = ~325KB memory. |

### 5.3 Throughput Expectations

The data path is: disk read -> 64KB buffer -> hyper HTTP body -> TCP -> WireGuard -> TCP -> axum body -> 64KB buffer -> disk write.

| Bottleneck | Throughput |
|---|---|
| WireGuard (direct LAN) | ~400-900 Mbps (50-110 MB/s) |
| WireGuard (via DERP relay) | ~50-200 Mbps (6-25 MB/s) |
| Disk I/O (SSD) | ~500 MB/s+ (not the bottleneck) |
| Bridge overhead (1 extra TCP hop) | ~5-10% overhead |
| SHA-256 inline | ~500 MB/s (not the bottleneck on LAN) |
| **Expected real-world** | **30-80 MB/s on LAN, 5-20 MB/s via relay** |

This is substantially better than Taildrop (which has similar throughput but no resume, no progress, no target path control).

---

## 6. Implementation Plan

### Phase 1: Wire File Transfer to Daemon (Priority: High)

**Goal**: Make `truffle cp file.txt server:/tmp/` work end-to-end.

1. **Start file transfer listener on `truffle up`**
   - In daemon startup, create `FileTransferManager` + start its axum server on ephemeral port
   - Tell sidecar: `tsnet:listen { port: 9417 }`
   - Bridge incoming :9417 connections to the axum server

2. **Implement DialFn using bridge**
   - Create a `dial_fn` that calls `bridge:dial` with port 9417
   - Returns a `TcpStream` representing the bridged connection

3. **Wire daemon handler for `cp_send`**
   - Replace the `handle_push_file` stub
   - Resolve node name to device_id (already done in handle_ping)
   - Call `FileTransferAdapter.send_file()`
   - Stream progress events back to CLI as JSON-RPC notifications
   - Return final result (success/failure, bytes transferred, SHA-256)

4. **Wire auto-accept for incoming CLI transfers**
   - When an OFFER with `cli_mode: true` arrives, auto-accept with the provided `save_path`
   - Validate save_path before accepting

5. **Update CLI `cp` command**
   - Remove base64 encoding (the daemon handles the file directly now, not the CLI)
   - Add progress bar rendering using progress notifications
   - Support `--verify` flag (already parsed, just needs to check SHA-256 in result)

### Phase 2: Downloads (Priority: High)

**Goal**: Make `truffle cp server:/tmp/file.txt ./` work.

1. **Add `PULL_REQUEST` message type**
   - New MessageBus message in `file-transfer` namespace
   - Payload: `{ requestId, remotePath, requesterAddr }`

2. **Handle PULL_REQUEST on remote side**
   - When a PULL_REQUEST arrives, the remote daemon:
     - Validates the remote_path exists and is a file
     - Calls `prepare_file()` to stat and hash
     - Sends an OFFER back to the requester with the file metadata
   - The requester auto-accepts the OFFER (it initiated the pull)

3. **Wire daemon handler for `cp_pull`**
   - Send PULL_REQUEST via MessageBus
   - Wait for incoming OFFER
   - Auto-accept and receive via existing infrastructure

### Phase 3: Progress UX (Priority: Medium)

**Goal**: Beautiful CLI progress output.

1. **Streaming notifications over Unix socket**
   - Extend the daemon protocol with a notification mechanism (JSON-RPC notifications, no `id` field)
   - Progress events flow as notifications while the main request is pending

2. **CLI progress bar**
   - Render: `  file.txt  45.2%  [=========>          ]  47.4 MB/s  eta 12s`
   - Update in-place using terminal escape codes
   - Final line: `  file.txt  100%   47.4 MB/s  2.1s  SHA-256 verified`

### Phase 4: Multi-File and Directory Support (Priority: Low)

**Goal**: `truffle cp -r dir/ server:/tmp/`

1. **Walk directory, transfer each file sequentially**
   - Maintain order for predictability
   - Skip symlinks (or follow with flag)
   - Preserve relative paths

2. **Parallel file transfers (optional)**
   - Transfer N files concurrently (default: 3)
   - Already supported by the manager's semaphore

### Phase 5: Optimizations (Priority: Low)

1. **Skip SHA-256 for small files** -- hashing overhead matters more for small files relative to transfer time
2. **Compression** -- optional gzip/zstd for compressible files (text, source code). WireGuard does not compress.
3. **Delta transfer** -- for `truffle sync` (future), use Syncthing-style block hashing to send only changed blocks
4. **Parallel chunks** -- split large files into N chunks and transfer in parallel over N connections. Useful when single-connection throughput is limited by the DERP relay.

---

## 7. What We Are NOT Building

| Feature | Why Not |
|---|---|
| **Custom wire protocol** | HTTP PUT over TCP is simple, debuggable (`curl`), and handles framing, content-length, range requests natively. No need for a custom binary protocol for file data. |
| **Chunked transfer** | RFC 001's chunk-based approach (512KB chunks, sliding window, per-chunk ACK) added complexity without benefit. Streaming with Content-Range resume achieves the same result with simpler code. Syncthing needs chunks for delta-sync; truffle does not. |
| **Encryption layer** | WireGuard already encrypts all traffic. Adding TLS or PAKE on top would be double-encryption with zero security benefit. |
| **Relay server** | Tailscale's DERP servers handle relay when direct connections fail. No need for a truffle-specific relay. |
| **Browser File API** | Desktop/CLI only for v1. Browser clients can use the MessageBus to signal transfers but cannot receive files (no filesystem). |
| **Taildrop integration** | Taildrop is retained in the sidecar for potential future interop with non-truffle Tailscale nodes, but it is not the primary transfer mechanism. |

---

## 8. API Surface

### 8.1 CLI Commands

```
truffle cp <source> <dest> [flags]

  Upload:   truffle cp file.txt server:/tmp/
  Download: truffle cp server:/tmp/file.txt ./
  Rename:   truffle cp file.txt server:/tmp/renamed.txt
  Current:  truffle cp file.txt server:     (keeps filename, saves to default dir)

Flags:
  --verify       Verify SHA-256 after transfer (default: true for files > 1MB)
  --no-verify    Skip SHA-256 verification
  --progress     Show progress bar (default: true in TTY, false in pipe)
  --json         Output progress as JSON lines (for scripting)
```

### 8.2 Daemon JSON-RPC Methods

```
cp_send {
  local_path: string,      // Absolute path to local file
  target_node: string,      // Node name, hostname, IP, or device_id
  remote_path: string,      // Destination path on remote (or "" for default)
  verify: bool,             // Whether to verify SHA-256
}

cp_pull {
  node: string,             // Remote node identifier
  remote_path: string,      // Path on remote node
  local_path: string,       // Where to save locally
  verify: bool,
}

// Notification (daemon -> CLI, no id):
cp.progress {
  transfer_id: string,
  percent: f64,
  bytes_transferred: i64,
  total_bytes: i64,
  bytes_per_second: f64,
  eta: f64,
}
```

### 8.3 Existing Internal APIs (No Changes Needed)

The following are already implemented and require no modification:

- `FileTransferManager::prepare_file()`
- `FileTransferManager::register_receive()`
- `FileTransferManager::register_send()`
- `FileTransferManager::send_file()`
- `FileTransferManager::receive_file_bytes()`
- `FileTransferManager::cancel_transfer()`
- `FileTransferAdapter::send_file()`
- `FileTransferAdapter::accept_transfer()`
- `FileTransferAdapter::reject_transfer()`
- `file_transfer_router()` (axum PUT/HEAD endpoints)

---

## 9. Security Considerations

### 9.1 Threat Model

All truffle nodes are on the same Tailscale tailnet. Tailscale handles authentication (device identity via WireGuard keys) and encryption (all traffic is WireGuard-encrypted). Truffle adds:

- **Transfer tokens**: 256-bit random tokens (64 hex chars) for per-transfer authorization. Prevents a node from receiving files it didn't agree to receive. Constant-time comparison prevents timing attacks.
- **Path validation**: `validate_save_path()` rejects relative paths, path traversal (`../`), and non-existent parent directories.
- **File size limits**: `max_file_size` (default 4GB) prevents disk exhaustion attacks.
- **Concurrent transfer limits**: `max_concurrent_recv` (default 5) prevents resource exhaustion.
- **Disk space check**: Pre-flight `Statfs` check before accepting data.

### 9.2 Auto-Accept Risk

CLI-mode auto-accept (no user confirmation) is safe because:
1. Only nodes on the same tailnet can reach each other (Tailscale ACLs)
2. The receiver validates the save_path before accepting
3. The transfer token prevents replay attacks
4. The file size limit prevents disk filling

For security-sensitive deployments, auto-accept can be disabled in config, requiring manual confirmation via a separate mechanism (e.g., `truffle transfers accept <id>`).

---

## 10. Alternatives Considered

### 10.1 Taildrop as Primary

**Rejected.** See Section 1.1. Taildrop's queue model, lack of progress/resume, lack of target path control, and lack of download support make it unsuitable for a CLI tool. It would require building a polling layer, a path-mapping layer, and a progress-estimation layer on top -- more work than using the existing direct TCP system.

### 10.2 WebSocket Binary Frames

**Rejected.** RFC 001 explored sending file chunks over the WebSocket MessageBus. This saturates the control plane (device discovery, store sync, mesh health) with bulk data. RFC 002 correctly separated the control plane (MessageBus) from the data plane (HTTP on :9417). The separation is essential for truffle to function as a library where file transfer doesn't starve other features.

### 10.3 Go-Native Data Plane (RFC 002 Original)

**Deferred.** RFC 002 proposed the Go sidecar running the HTTP server directly. The Rust implementation now exists and works. Routing through the bridge adds one TCP hop but keeps all logic in Rust, where it's easier to maintain, test, and extend. If bridge overhead becomes a measured bottleneck (>10% throughput loss), the Go-native path can be revisited.

### 10.4 QUIC / HTTP/3

**Deferred.** QUIC provides multiplexed streams, 0-RTT resumption, and built-in congestion control. However, Tailscale uses WireGuard (UDP-based), and running QUIC over WireGuard creates UDP-over-UDP tunneling issues. Syncthing explored QUIC and found it performed worse than TCP over WireGuard. Revisit when/if Tailscale adds native QUIC support.

### 10.5 rsync / Delta Sync

**Deferred.** For `truffle sync` (continuous synchronization), Syncthing-style block hashing would allow transferring only changed blocks. This is a different product from `truffle cp` (one-shot transfer) and should be designed as a separate feature, likely as a `StoreSyncAdapter`-like service.

---

## 11. Open Questions

1. **Default receive directory**: When `truffle cp file.txt server:` has no path, where does the file go? Options: `~/Downloads/truffle/`, `~/.local/share/truffle/received/`, or configurable. Recommend configurable with default `~/Downloads/truffle/`.

2. **Overwrite behavior**: If the destination file exists, should truffle overwrite, fail, or prompt? Recommend: fail by default, `--force` to overwrite, matching `cp` behavior.

3. **Taildrop interop**: Should `truffle cp` be able to send files to non-truffle Tailscale nodes via Taildrop? This would require detecting whether the target has truffle running and falling back to Taildrop if not. Defer to a future RFC.

4. **Maximum file size**: The current default is 4GB. Should this be raised for the CLI use case? The implementation has no architectural limit (uses `i64` for sizes, streaming I/O). Recommend raising to 100GB or removing the limit entirely for CLI transfers.

5. **Transfer persistence**: Should in-progress transfers survive daemon restarts? The `.partial` and `.partial.meta` files already persist on disk, but the transfer registry is in-memory. For MVP, no persistence (restart = re-transfer). For v2, the meta files could be used to rebuild the registry on startup.

---

## 12. Summary

Truffle already has a complete, tested, production-quality file transfer system (manager, adapter, sender, receiver, types, hasher, progress, resume). The only work needed is:

1. **Wire the daemon** to start the file transfer listener and create a DialFn using the bridge
2. **Replace the stub handlers** in `handler.rs` with real implementations that drive the `FileTransferAdapter`
3. **Add PULL_REQUEST** for downloads (one new message type)
4. **Add auto-accept** for CLI-mode transfers (a flag on the OFFER)
5. **Stream progress** back to the CLI process

This is integration work, not protocol design work. The protocol is already designed and implemented. The estimated effort is ~500-800 lines of new code, primarily in `handler.rs` (daemon wiring) and `cp.rs` (CLI progress rendering).

---

## References

- RFC 001: File Transfer Primitive for Truffle (superseded by RFC 002)
- RFC 002: Native Go File Transfer (design basis for current Rust implementation)
- RFC 008: Truffle Vision -- Architecture & Public API
- RFC 009: Wire Protocol Redesign
- RFC 010: P2P Mesh Redesign + CLI Daemon Architecture
- [croc](https://github.com/schollz/croc) -- P2P file transfer with relay fallback
- [magic-wormhole](https://github.com/magic-wormhole/magic-wormhole) -- PAKE-based file transfer
- [Syncthing BEP](https://docs.syncthing.net/specs/bep-v1.html) -- Block Exchange Protocol
- [Tailscale Taildrop](https://tailscale.com/kb/1106/taildrop) -- Tailscale built-in file sharing
