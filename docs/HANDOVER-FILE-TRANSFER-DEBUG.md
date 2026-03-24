# Handover: File Transfer Debugging

> Written: 2026-03-23. For the next agent picking up the `truffle cp` file transfer bug.
>
> **UPDATE 2026-03-24: BUG RESOLVED in v0.2.5.** Root cause was `blocking_read()` on a tokio RwLock inside an async axum handler, which deadlocked the runtime. See commit `a631d73`. Additional fixes: Tailscale IP over DNS for dialing, 3x retry with backoff, relaxed heartbeat (10s/30s), Connection: close on HEAD response. 5/5 file transfers pass macOS↔EC2. Debug `eprintln!` logging is still in the codebase and should be removed.

---

## 1. Truffle CLI Networking Architecture (Brief)

Truffle is a P2P mesh networking CLI built on Tailscale's `tsnet`. It has 6 layers:

```
 CLI (clap)  -->  Daemon (JSON-RPC over Unix socket)  -->  TruffleRuntime
                                                              |
                                              +---------------+---------------+
                                              |                               |
                                        MeshNode                        BridgeManager
                                    (device tracking,               (TCP listener on 127.0.0.1,
                                     WS message routing)             binary header routing)
                                              |                               |
                                        ConnectionManager              GoShim (stdin/stdout)
                                    (WebSocket connections)                    |
                                                                        Go Sidecar (tsnet)
                                                                    (Tailscale embedded node)
```

### How connections work

1. **Go sidecar** creates a `tsnet.Server` (embedded Tailscale node)
2. Sidecar auto-listens on **port 443** (TLS) and **port 9417** (TCP) for mesh WebSocket connections
3. Sidecar also listens on **port 9418** (dynamic, for file transfer) via `shim.listen(9418, false)`
4. When a remote peer connects, the sidecar calls `bridgeToRust()` which:
   - Connects to Rust's local **bridge TCP port** (`127.0.0.1:<ephemeral>`)
   - Sends a binary header (magic `TRFF`, session token, direction, port, request_id)
   - Does bidirectional copy between the tsnet connection and the bridge connection
5. Rust's **BridgeManager** accepts the bridge TCP connection, reads the header, and routes by `(port, direction)`:
   - `(443, Incoming)` / `(9417, Incoming)` → HttpRouter → WebSocket `/ws` upgrade
   - `(9417, Outgoing)` → `pending_dials` map (for mesh connection dials)
   - `(9418, Incoming)` → `TcpProxyHandler` (forwards to local axum file transfer server)
   - `(9418, Outgoing)` → `pending_dials` map (for file transfer DialFn)

### How mesh connections are established

1. Runtime calls `shim.get_peers()` every 30 seconds
2. For each online peer with hostname starting with `"truffle"`, dial port 9417
3. Dial flow: insert oneshot into `pending_dials` → `shim.dial_raw(dns, 9417, rid)` → Go dials via tsnet → `bridgeToRust()` → BridgeManager finds `pending_dials[rid]` → delivers `BridgeConnection` via oneshot
4. Runtime upgrades the raw TCP stream to WebSocket (`handle_outgoing`)
5. Both peers exchange `device-announce` messages to identify each other

### Port map

| Port | Transport | Purpose |
|------|-----------|---------|
| 443 | TLS (ListenTLS) | Primary mesh WS (unused for dials due to double-TLS bug) |
| 9417 | TCP (Listen) | Mesh WS connections (what we actually use for dials) |
| 9418 | TCP (dynamic Listen) | File transfer HTTP (PUT/HEAD) |
| IPC | Unix socket | CLI ↔ daemon JSON-RPC |
| bridge | TCP 127.0.0.1:0 | Go sidecar ↔ Rust bridge |

---

## 2. File Transfer Architecture (RFC 011)

### Design

The file transfer uses existing infrastructure (~3,400 lines in `truffle-core/src/services/file_transfer/`):

- **FileTransferManager** — state machine, SHA-256 hashing, progress, cleanup
- **FileTransferAdapter** — OFFER/ACCEPT/REJECT signaling via mesh WebSocket messages
- **Sender** (`sender.rs`) — HTTP PUT client using `hyper`, with resume support
- **Receiver** (`receiver.rs`) — `axum` HTTP server with PUT/HEAD endpoints

### Upload flow (`truffle cp file.txt server:/tmp/`)

```
1. CLI sends `push_file` JSON-RPC to daemon
2. Handler prepares file (stat + SHA-256)
3. Handler calls `ensure_connected(device_id)` to establish mesh WS
4. Handler sends OFFER (with cli_mode=true) via mesh WS to remote
5. Remote's adapter auto-accepts (Phase 5), sends ACCEPT back via mesh WS
6. Local adapter receives ACCEPT, spawns `send_file()` task
7. send_file uses DialFn to connect to remote's port 9418
8. send_file does HTTP HEAD (resume check) then HTTP PUT (data transfer)
9. Remote's axum receiver writes file to disk, verifies SHA-256
10. Manager emits Complete event, handler responds to CLI
```

### The DialFn

The DialFn is a closure that dials a remote address through the Go sidecar:

```
DialFn(addr) → parse host:port → insert oneshot into pending_dials
             → shim.dial_raw(host, port, request_id) → Go dials via tsnet
             → bridgeToRust() → BridgeManager delivers via pending_dials oneshot
             → return raw TcpStream
```

It's created once during daemon startup in `server.rs:create_dial_fn()` and stored in the adapter config.

---

## 3. Bugs Fixed So Far

### Fixed: Peer discovery (shared prefix)
- **Root cause**: Each node used its OS hostname as the tsnet prefix (e.g., `Jamess-MacBook-Pro-6.local-cli-xxx`), so peer filtering rejected all cross-machine peers
- **Fix**: All nodes use shared `"truffle"` prefix → `truffle-cli-{uuid}`

### Fixed: No periodic peer polling
- **Root cause**: `get_peers()` called only once at startup
- **Fix**: 30-second polling interval after going online

### Fixed: Double-TLS on port 443
- **Root cause**: Go sidecar's `handleDial` wraps port 443 with TLS client, but remote's `ListenTLS` already provides TLS
- **Fix**: Dial on port 9417 (plain TCP) instead of 443

### Fixed: Pending dials Map A vs Map B
- **Root cause**: `TruffleRuntime` had its own `bridge_pending_dials` (Map A) separate from `BridgeManager.pending_dials` (Map B). `dial_peer()` inserted into Map A but BridgeManager looked up Map B.
- **Fix**: Added `bridge_manager_pending` field to runtime. `pending_dials()` returns Map B after `start()`. Verified both DialFn and dial_peer use the same pointer.

### Fixed: OFFER→ACCEPT flow not working
- **Root cause**: `send_envelope()` couldn't find connection by device_id (announce not exchanged yet). Also, OFFER was sent via the async bus_tx pump which could silently fail.
- **Fix**: `ensure_connected()` establishes mesh WS before sending OFFER. Handler sends OFFER directly via `runtime.send_envelope()` with delivery verification.

### Fixed: DialFn lock contention (partial)
- **Root cause**: The `?` operator in DialFn could return early while holding the shim lock. Peer dial tasks held shim lock while also accessing pending_dials.
- **Fix**: Replaced `?` with explicit `match` blocks. Restructured peer dial tasks to insert pending_dials BEFORE acquiring shim lock.

### Fixed: TcpProxyHandler hanging
- **Root cause**: `tokio::join!(copy_a_to_b, copy_b_to_a)` waits for both to EOF, but HTTP keep-alive connections never EOF
- **Fix**: Each copy direction in its own `tokio::spawn` with explicit `shutdown()` on the write half when the read side completes

### Fixed: HTTP Connection: close
- **Root cause**: Without `Connection: close`, the axum server keeps the connection open after responding
- **Fix**: Added `Connection: close` header to both HEAD and PUT requests in sender.rs

---

## 4. Current Problem

### Symptom
`truffle cp` works ~50% of the time. When it fails, the error is either:
- `"dial cancelled (sender dropped)"` — the oneshot sender was dropped before the bridge connection arrived
- `"File transfer timed out"` — the 60s handler timeout fires without receiving a Complete/Error event

### What we know for certain

1. **The OFFER→ACCEPT control plane works**: Confirmed with debug logging. Remote receives OFFER, auto-accepts, sends ACCEPT back. Local adapter receives ACCEPT and spawns `send_file`.

2. **The DialFn can succeed**: In one test run, the DialFn successfully dialed port 9418, the BridgeManager found the pending_dial entry, and delivered the bridge connection. Full trace:
   ```
   [DialFn] shim lock acquired → dial_raw returned Ok(()) → shim lock released
   [DialFn] waiting for bridge connection...
   [BridgeManager] header parsed: dir=outgoing port=9418 rid=XXX
   [BridgeManager] found pending dial for rid=XXX, delivering...
   [DialFn] bridge connection received!
   ```

3. **The DialFn can also fail**: In the very next test run (same code, same binary), the DialFn hangs with "dial cancelled (sender dropped)". The failure is timing-dependent.

4. **`runtime.dial_peer()` always works**: `truffle tcp node:9418 --check` (which uses `dial_peer`) consistently succeeds. Both dial_peer and DialFn use the same pending_dials Arc (verified with pointer comparison: `0x103a9d010`).

5. **The TcpProxyHandler works**: When the DialFn succeeds and the connection reaches the remote, the proxy connects to the local axum server. But the HTTP exchange then fails with "connection closed before message completed" (even with Connection: close).

### Reasoning about the root cause

The intermittent failure points to a **race condition**, not a deterministic deadlock. The most likely scenario:

**Hypothesis A: Lifecycle handler's peer dial loop steals the bridge connection**

The lifecycle handler (runtime.rs ~line 860) runs a peer dial loop every 30 seconds. It also dials peers via `shim.dial_raw()` and inserts into the SAME `pending_dials` map. If the lifecycle handler happens to dial the same peer at the same time as the DialFn, there could be:
- Two entries in pending_dials for the same peer (different request_ids — should be fine)
- But the BridgeManager might deliver the wrong connection to the wrong oneshot

Actually, this shouldn't happen because each dial has a unique UUID request_id. The BridgeManager matches by request_id, not by peer.

**Hypothesis B: The BridgeManager processes the connection but the oneshot receiver is already dropped**

If the handler's 60s timeout fires BEFORE the bridge connection arrives, the handler returns, which drops the event subscription. But the `send_file` task (and its DialFn) is in a separate spawned task — the oneshot receiver should still be alive. Unless the DialFn's `tokio::time::timeout(10s, rx)` fires first and drops the rx, then the BridgeManager finds the entry but `tx.send(conn)` fails (receiver dropped).

But we see "dial cancelled (sender dropped)" which means the **tx** was dropped, not the rx. The tx is in the pending_dials map. It can only be dropped if:
- Someone removes it from the map and drops the tx
- The map itself is dropped

**Hypothesis C: The Go sidecar sends the bridge connection to the BridgeManager but with the wrong request_id**

This would cause the BridgeManager to log "unknown outgoing request_id" and drop the connection. The DialFn's oneshot never fires, eventually timing out.

**Hypothesis D: The Go sidecar's dial to port 9418 fails silently**

If `s.server.Dial()` fails, Go sends a `bridge:dialResult` event with `success: false`. This is processed by the Rust event loop as `ShimLifecycleEvent::DialFailed`. The handler in runtime.rs removes the pending_dials entry (line ~786). This would cause "dial cancelled (sender dropped)" on the DialFn's oneshot.

**This is the most likely cause.** The lifecycle handler at runtime.rs line 786 handles `DialFailed` events:
```rust
Ok(ShimLifecycleEvent::DialFailed { request_id, error }) => {
    bridge_pending.lock().await.remove(&request_id);
}
```
This removes from `bridge_pending` which is the BridgeManager's Arc (Map B). If Go reports a dial failure for the DialFn's request_id, the oneshot tx is removed and dropped → "sender dropped" error.

### Potential fixes

1. **Don't remove DialFn entries on DialFailed**: The lifecycle handler should only remove entries that IT created (for mesh dials), not entries created by the DialFn. Add a prefix to request_ids to distinguish them (e.g., `mesh-{uuid}` vs `ft-{uuid}`).

2. **Retry in send_file**: Add a retry loop (2-3 attempts) around the dial in `query_resume_offset`. This would mask the race condition.

3. **Separate pending_dials for file transfer**: Use a completely separate map for file transfer dials, so there's zero interaction with the lifecycle handler.

4. **Investigate Go dial failures**: Add logging in the Go sidecar's `handleDial` to see if dials to port 9418 are actually failing and why. Check `s.server.Dial()` error messages.

---

## 5. How to Test

### Prerequisites
- Rust toolchain with `x86_64-unknown-linux-musl` target and `musl-cross` linker
- SSH key at `~/Downloads/ohio1-dev.pem`
- Both nodes on the same Tailscale account

### Build both binaries
```bash
cd /Users/jamesyong/Projects/project100/p008/truffle

# macOS (local)
cargo build --release -p truffle-cli

# Linux (cross-compile for EC2)
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
CC_x86_64_unknown_linux_musl=x86_64-linux-musl-gcc \
cargo build --release -p truffle-cli --target x86_64-unknown-linux-musl
```

### Deploy to both nodes
```bash
# macOS
cp target/release/truffle ~/.config/truffle/bin/truffle

# Linux EC2 (Ohio, us-east-2)
ssh -i ~/Downloads/ohio1-dev.pem jasper@ec2-3-19-208-105.us-east-2.compute.amazonaws.com \
  'export PATH="$HOME/.config/truffle/bin:$PATH" && truffle down 2>/dev/null; pkill -f "truffle up" 2>/dev/null; sleep 1'

scp -i ~/Downloads/ohio1-dev.pem \
  target/x86_64-unknown-linux-musl/release/truffle \
  jasper@ec2-3-19-208-105.us-east-2.compute.amazonaws.com:~/.config/truffle/bin/truffle
```

### Start both nodes
```bash
# macOS — foreground mode captures stderr (debug logs)
truffle up --foreground > /tmp/debug.log 2>&1 &

# Wait a few seconds for the macOS node to start
sleep 5

# Linux — foreground mode
ssh -i ~/Downloads/ohio1-dev.pem jasper@ec2-3-19-208-105.us-east-2.compute.amazonaws.com \
  'export PATH="$HOME/.config/truffle/bin:$PATH" && nohup truffle up --foreground > /tmp/debug.log 2>&1 &'

# Wait 40-45 seconds for peer discovery + mesh connection
sleep 45
```

### Verify connectivity
```bash
# Check both see each other
truffle ls
truffle ping ip-172-31-45-100.us-east-2.compute.internal

# Verify port 9418 is reachable
truffle tcp ip-172-31-45-100.us-east-2.compute.internal:9418 --check
```

### Test file transfer
```bash
echo "test content" > /tmp/test.txt
truffle cp /tmp/test.txt ip-172-31-45-100.us-east-2.compute.internal:/tmp/test.txt

# Verify on remote
ssh -i ~/Downloads/ohio1-dev.pem jasper@ec2-3-19-208-105.us-east-2.compute.amazonaws.com \
  'cat /tmp/test.txt'
```

### Read debug logs
```bash
# macOS
strings /tmp/debug.log | grep -E '\[DialFn\]|\[BridgeManager\]|\[TcpProxy|\[FT-|\[ft-|\[send_file\]|\[query_resume' | head -30

# Linux
ssh -i ~/Downloads/ohio1-dev.pem jasper@ec2-3-19-208-105.us-east-2.compute.amazonaws.com \
  'strings /tmp/debug.log | grep -E "\[DialFn\]|\[BridgeManager\]|\[TcpProxy|\[FT-|\[ft-" | head -30'
```

### Stop both nodes
```bash
truffle down
ssh -i ~/Downloads/ohio1-dev.pem jasper@ec2-3-19-208-105.us-east-2.compute.amazonaws.com \
  'export PATH="$HOME/.config/truffle/bin:$PATH" && truffle down'
```

### Run unit tests
```bash
cargo test --workspace --exclude truffle-tauri-plugin --lib
# Expected: 787+ tests pass
```

---

## 6. Key Files

| File | What it does |
|------|-------------|
| `crates/truffle-cli/src/daemon/server.rs` | Daemon startup, `create_dial_fn()`, file transfer bootstrap |
| `crates/truffle-cli/src/daemon/handler.rs` | `handle_push_file()`, `handle_get_file()`, `ensure_connected()` |
| `crates/truffle-core/src/runtime.rs` | `TruffleRuntime`, `dial_peer()`, lifecycle handler, peer dial loop |
| `crates/truffle-core/src/bridge/manager.rs` | `BridgeManager`, `TcpProxyHandler`, `pending_dials` routing |
| `crates/truffle-core/src/bridge/shim.rs` | `GoShim`, stdin/stdout event loop, `send_command()`, `dial_raw()` |
| `crates/truffle-core/src/services/file_transfer/sender.rs` | `send_file()`, `query_resume_offset()`, HTTP PUT/HEAD |
| `crates/truffle-core/src/services/file_transfer/adapter.rs` | OFFER/ACCEPT/REJECT, auto-accept for cli_mode |
| `crates/truffle-core/src/services/file_transfer/receiver.rs` | axum PUT/HEAD endpoints |
| `packages/sidecar-slim/main.go` | Go sidecar: `handleDial`, `handleListen`, `bridgeToRust` |
| `docs/architecture/networking-flows.md` | Full networking architecture analysis with sequence diagrams |

---

## 7. Debug logging currently in place

The codebase has `eprintln!` debug prints (prefixed with `[DialFn]`, `[BridgeManager]`, `[TcpProxyHandler]`, `[FT-Adapter]`, `[FT-Sender]`, `[GoShim]`, etc.) in these files:
- `server.rs` (DialFn)
- `manager.rs` (BridgeManager accept loop, TcpProxyHandler)
- `shim.rs` (send_command, stdin forwarding)
- `adapter.rs` (ACCEPT handling)
- `sender.rs` (query_resume_offset, send_file)

These should be removed once the bug is fixed. They print to stderr which is captured when running `truffle up --foreground > /tmp/debug.log 2>&1`.
