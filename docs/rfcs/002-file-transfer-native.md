# RFC 002: Native Go File Transfer

**Status**: Proposed
**Date**: 2026-02-26
**Supersedes**: RFC 001 (File Transfer Primitive for Truffle)

---

## 1. Problem Statement

RFC 001 designed a file transfer system that routes all file data through Node.js: files are read in TypeScript, base64-encoded into JSON chunks, sent over stdin to the Go sidecar, forwarded over WebSocket, and reassembled on the remote side. Every byte traverses the JSON IPC boundary twice.

This architecture is fundamentally slow:

| Bottleneck | Impact |
|---|---|
| Base64 encoding | +33% data size |
| JSON wrapping | Serialization/parse overhead per chunk |
| stdin/stdout IPC | Single-threaded `readLoop()` in Go, ~64KB OS pipe buffer |
| Node.js memory | Full chunks buffered in V8 heap |
| WebSocket framing | TextMessage mode forces UTF-8 validation |

Measured throughput: **~5-15 MB/s** on LAN. The Go sidecar already has primitives that can achieve **~40-60 MB/s** (WireGuard-limited) with near-zero CPU overhead.

### Why Go Can Do Better

The Go sidecar embeds `tsnet.Server`, which provides:
- `node.Listen("tcp", ":port")` -- raw TCP listeners on the Tailscale network
- `node.Dial(ctx, "tcp", "addr:port")` -- raw TCP to other nodes
- WireGuard encrypts all traffic automatically -- no additional TLS needed
- `io.Copy` with `io.TeeReader` for streaming with near-zero memory overhead

The optimal design: **file bytes never touch Node.js**. Go reads from disk, streams over Tailscale TCP, remote Go writes to disk. Node.js is control plane only.

---

## 2. Goals

1. Transfer files between devices with near-wire-speed throughput
2. Keep Node.js as control plane only -- no file bytes in JavaScript
3. Support resume of interrupted transfers
4. Provide real-time progress events to the UI
5. Reuse existing infrastructure: MessageBus for signaling, tsnet for transport
6. Maintain security via WireGuard encryption and transfer tokens

## 3. Non-Goals

- Browser `File` API support (desktop/Node.js only for v1)
- Directory transfer (application layer concern)
- Compression (WireGuard handles this poorly; application can pre-compress)
- Multi-device broadcast (point-to-point only)

---

## 4. Architecture

```
CONTROL PLANE (existing infrastructure):
  Node.js <-> MessageBus <-> WebSocket <-> Remote Node.js
  (offer, accept, reject, cancel signaling)

DATA PLANE (new, native Go):
  Go sidecar <-> HTTP PUT on :9417 <-> Tailscale/WireGuard <-> Remote Go sidecar
  (raw file bytes, streamed disk-to-disk)

COORDINATION:
  Node.js <-> IPC commands/events <-> Go sidecar
  (file:prepare, file:send, file:accept, file:progress, file:complete)
```

### 4.1 Why HTTP (Not Raw TCP)

HTTP is used as a framing layer over tsnet TCP; there is no public internet exposure. Port :9417 is only reachable within the Tailscale network.

Following Taildrop's proven approach:
- `Content-Length` for progress calculation, `Content-Range` for resume, standard status codes for error handling
- Debuggable with curl: `curl -X PUT -H "X-Transfer-Token: ..." --data-binary @file http://peer:9417/transfer/abc`
- Negligible overhead for file-sized transfers (~100 bytes header vs megabytes of data)
- Go `net/http` handles concurrency natively -- each transfer is a goroutine, no manual connection management

### 4.2 Why Dedicated Port :9417 (Not Reuse :443)

- **Isolation**: File transfer backpressure cannot starve WebSocket control plane messages
- **Independent monitoring**: Rate limiting, metrics, and debugging are separate concerns
- **No double-TLS**: Plain TCP via `tsnet.Listen("tcp", ":9417")` -- WireGuard already encrypts all traffic on the Tailscale network. Port :443 uses `ListenTLS()` with Let's Encrypt certificates for browser access; file transfer between Go sidecars doesn't need that.
- **Port :9417**: Unregistered IANA port, mnemonic: "94" = transfer, "17" = Truffle

### 4.3 Separation of Concerns

| Layer | Responsibility | Technology |
|---|---|---|
| **UI** | Show offers, progress bars, accept/reject | React (existing) |
| **Control plane** | Signaling: offer, accept, reject, cancel | MessageBus over WebSocket (existing) |
| **Coordination** | Bridge UI decisions to Go actions | IPC commands/events (new) |
| **Data plane** | Read file, stream bytes, write file, verify | Go HTTP on Tailscale TCP (new) |
| **Encryption** | All traffic encrypted | WireGuard (existing, automatic) |

### 4.4 Address Discovery

The sender needs the receiver's address to dial `:9417`. This is exchanged via the MessageBus ACCEPT message:

1. **Receiver** knows its own address from `tsnet.Server` (the Tailscale hostname). The `FileTransferAdapter` is configured with `localAddr` at startup (e.g., `"myhost.tailnet-name.ts.net:9417"`).
2. **ACCEPT message** includes `receiverAddr` — the sender extracts this and passes it to `file:send`.
3. **Format**: Tailscale MagicDNS hostname + port, e.g., `"hostname.tailnet-name.ts.net:9417"`. This is a DNS name, not an IP — `tsnet.Dial()` resolves it via MagicDNS within the tailnet.
4. **Stability**: The MagicDNS hostname is stable across sessions (tied to the device's Tailscale identity). The address does not change unless the device is removed and re-added to the tailnet.
5. **No public DNS**: MagicDNS names are only resolvable within the Tailscale network. `tsnet.Dial()` handles resolution internally — standard `net.Dial()` would fail.

---

## 5. Transfer Lifecycle

```
 Sender Side                                              Receiver Side
 ──────────                                               ─────────────

 1. Node.js: sidecar.filePrepare(transferId, filePath)
 2. Go: stats file, computes SHA-256, emits file:prepared
 3. Node.js: sends OFFER via MessageBus ──────────────────> Node.js: shows accept UI
 4.                                                         User clicks "Accept"
 5.                                     <────────────────── Node.js: sidecar.fileAccept(transferId, savePath, token)
 6.                                                         Go: registers transfer, starts HTTP server handler
 7.                                     <────────────────── Node.js: sends ACCEPT via MessageBus (with token)
 8. Node.js: receives ACCEPT
 9. Node.js: sidecar.fileSend(transferId, filePath, targetAddr, token)
10. Go: dials target:9417 via tsnet.Dial()
11. Go: HTTP PUT with file bytes ─────────────────────────> Go: accepts PUT, streams to savePath.partial
12. Both Go sidecars: emit file:progress events periodically
13.                                                         Go: verifies SHA-256, renames .partial -> final
14. Go: receives HTTP 200 <─────────────────────────────── Go: emits file:complete
15. Go: emits file:complete
```

### Critical Ordering

Step 5 (receiver registers in Go) happens **BEFORE** step 7 (accept signal sent to sender). This guarantees the receiver's HTTP handler is ready before the sender can possibly dial. Without this ordering, the sender could dial before the receiver is listening, causing a connection refused error.

### 5.1 State Machine

Each `Transfer` has a `state` field (accessed atomically) and an `inFlight` flag (separate atomic). The `inFlight` flag is 1 only while a `handlePUT` goroutine is actively running.

#### Allowed Transitions (Receiver Side)

```
                    ┌──────────────────────────────────┐
                    │                                  │
  ┌─────────────┐   │  ┌──────────────┐   ┌───────────▼──┐
  │ Registered  │───┼─▶│ Transferring │──▶│  Completed   │
  └──────┬──────┘   │  └──────┬───────┘   └──────────────┘
         │          │         │
         │          │         │  (non-resumable error)
         │          │         ├─────────────────────┐
         │          │         │                     │
         │          │         │  (resumable error   ▼
         │          │         │   or connection   ┌──────────────┐
         │          │         │   drop: stay in   │    Failed     │
         │          │         │   Transferring)   │  (terminal)  │
         │          │         ▼                   └──────────────┘
         │          │  ┌──────────────┐
         │          └──│ Transferring │
         │             │ (retry PUT)  │
         │             └──────────────┘
         │
         │  (cancel / timeout)
         ▼
  ┌──────────────┐
  │  Cancelled   │
  │(partial del) │
  └──────────────┘
```

| From | To | Trigger | Condition |
|---|---|---|---|
| Registered | Transferring | First PUT arrives | CAS succeeds |
| Registered | Cancelled | `file:cancel` IPC or `RegisteredTimeoutTTL` sweep | Any time |
| Transferring | Completed | SHA-256 verified, file renamed | `io.Copy` finished successfully |
| Transferring | Failed | Hash mismatch, integrity error, disk setup error | Non-resumable error (`failTransfer`) |
| Transferring | Transferring | Resumable error (WRITE_ERROR, connection drop) | `softFailTransfer` or handler exits; `inFlight` cleared by defer |
| Transferring | Transferring | Retry PUT after soft-fail or disconnect | `inFlight == 0` (previous handler exited) |
| Transferring | Cancelled | `file:cancel` IPC | `cancellableReader` breaks `io.Copy` |
| Failed | *(terminal)* | — | Non-resumable only. Partial file preserved but no retry via same transferId. |
| Completed | *(terminal)* | — | Swept after `CompletedRetainTTL` |
| Cancelled | *(terminal)* | — | Partial file deleted |

**Key invariants:**
- At most one `handlePUT` goroutine is active per transfer at any time, enforced by the `inFlight` CAS. The `state` persists across handler lifetimes; `inFlight` does not.
- `StateFailed` is only set for non-resumable errors (integrity, protocol, auth). Resumable errors (WRITE_ERROR, connection drop) keep the transfer in `StateTransferring` via `softFailTransfer`, allowing the sender to retry without re-accepting.

#### Sender Side

| From | To | Trigger |
|---|---|---|
| Transferring | Completed | HTTP 200 from receiver |
| Transferring | Failed | HTTP error, network error, context cancelled |
| Transferring | Cancelled | `file:cancel` IPC |

### 5.2 Cancel Behavior

Cancel is both **local** (stops the data plane) and **remote** (notifies the peer via MessageBus):

1. **`cancelTransfer(transferId)`** in TypeScript:
   - Calls `sidecar.fileCancel(transferId)` — Go sets state to `StateCancelled`, cancels context, `cancellableReader` breaks `io.Copy`
   - Publishes `ft:cancel` via MessageBus to the peer — remote UI dismisses the offer or stops showing progress

2. **Cleanup on cancel:**
   - Receiver: deletes `.partial` and `.partial.meta` (cancel = discard)
   - Sender: goroutine exits, context cancelled

3. **Cancel before data connection:**
   - If cancel happens during the "offering" state (before sender dials), the MessageBus CANCEL is the only way the peer learns about it. Without it, the remote UI would show a stale offer indefinitely.

4. **Disconnect is NOT cancel:**
   - A TCP connection drop (network blip, WiFi roaming) is handled by `softFailTransfer` — state stays `Transferring`, partials are preserved for resume. See §6.4.

### Data Flow Comparison

**RFC 001 (Node.js data plane):**
```
File -> fs.read -> Buffer -> base64 -> JSON.stringify -> stdin -> Go readLoop
  -> WebSocket TextMessage -> WireGuard -> remote WebSocket -> Go IPC event
  -> stdout -> JSON.parse -> base64 decode -> Buffer -> fs.write
```
Data copies: **~8**, all bytes through Node.js twice.

**RFC 002 (Go native data plane):**
```
Sender:   File -> os.Open -> Seek(offset) -> progressReader -> HTTP PUT body -> WireGuard
Receiver: WireGuard -> http.Handler -> io.TeeReader(body, sha256) -> os.Create -> file
```
Sender does NOT hash during send — the hash was pre-computed in `file:prepare`. Only the receiver computes SHA-256 inline via `TeeReader`, covering both fresh and resumed transfers correctly.

Data copies: **~2** (kernel read buffer -> network, network -> kernel write buffer).

---

## 6. Wire Protocol: HTTP on :9417

All HTTP traffic flows over Tailscale TCP, encrypted by WireGuard.

All non-200 responses use a stable JSON error body so the sender can map errors to `FileTransferErrorEvent` codes without string parsing:

```json
{
  "code": "TRANSFER_NOT_FOUND",
  "message": "transfer not found"
}
```

### 6.1 Send File

```http
PUT /transfer/{transferId}
Content-Length: 104857600
X-Transfer-Token: a1b2c3d4e5f6...  (32 random bytes, 64 hex chars)
X-File-Name: photo.jpg
X-File-SHA256: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855

<raw file bytes>
```

**Responses:**
- `200 OK` -- transfer complete, SHA-256 verified
- `403 Forbidden` -- invalid or missing transfer token
- `404 Not Found` -- no registered transfer for this ID
- `409 Conflict` -- `TRANSFER_NOT_ACCEPTING` (terminal state or CAS race), `TRANSFER_IN_FLIGHT` (handler already active), or `OFFSET_MISMATCH` (Content-Range start != partial size)
- `413 Payload Too Large` -- exceeds configured size limit
- `500 Internal Server Error` -- disk write failure or other error

### 6.2 Query Resume Offset

```http
HEAD /transfer/{transferId}
X-Transfer-Token: a1b2c3d4e5f6...

200 OK
Upload-Offset: 52428800
```

If no partial data exists, returns `Upload-Offset: 0`.

`Upload-Offset` is derived from the `.partial` file size on disk (the authoritative source). The `.partial.meta` file is advisory metadata for crash recovery — it is NOT used to determine the resume offset.

### 6.3 Resume Transfer

```http
PUT /transfer/{transferId}
Content-Range: bytes 52428800-104857599/104857600
X-Transfer-Token: a1b2c3d4e5f6...

<remaining bytes from offset 52428800>
```

### 6.4 Cancel vs Disconnect

**Cancel** is an explicit user action, driven by IPC (`file:cancel`), not inferred from the HTTP connection:

- **Cancel (IPC-driven):** `file:cancel` sets state to `StateCancelled`, cancels context, breaks `io.Copy` via `cancellableReader`, and **deletes** `.partial` and `.partial.meta`. The peer is notified via MessageBus `ft:cancel`. This means "discard this transfer."

- **Disconnect (TCP drop):** The HTTP connection closes unexpectedly (network blip, WiFi roaming, DERP relay timeout). The receiver's `io.Copy` returns an error. The handler exits via `softFailTransfer`, keeping state as `StateTransferring` and **preserving** `.partial` for resume. This means "transient failure, retry later."

These are deliberately different: closing the HTTP connection is **not** a cancel. Treating TCP EOF as cancel would destroy resumable transfers on flaky networks.

### 6.5 Validation Rules

Every PUT/HEAD request is validated by the receiver:

| Check | Rejection |
|---|---|
| Missing or invalid `X-Transfer-Token` (format or value) | 403 Forbidden |
| Unknown `transferId` in path | 404 Not Found |
| Transfer already in progress, completed, or cancelled (CAS fails) | 409 Conflict |
| File size exceeds `MaxFileSize` config (receiver-side enforcement) | 413 Payload Too Large |
| File size <= 0 | 400 Bad Request |
| Missing `Content-Range` on resume (state is Transferring) | 400 Bad Request |
| `Content-Range` total != expected file size | 400 Bad Request |
| `Content-Range` end != total - 1 | 400 Bad Request |
| `Content-Range` start out of bounds | 400 Bad Request |
| `Content-Range` start != actual `.partial` size | 409 Conflict |
| `Content-Length` != expected remaining bytes | 400 Bad Request |
| Partial file size < resume offset (subsumed by OFFSET_MISMATCH) | 409 Conflict |
| `X-File-SHA256` header differs from registered hash | 400 Bad Request |
| SHA-256 mismatch on completion (against registered hash) | 409 Conflict |
| Insufficient disk space for remaining bytes (pre-flight `Statfs`) | 507 Insufficient Storage |
| Received fewer bytes than expected (undersend) | 400 Bad Request |
| Disk write failure | 500 Internal Server Error |

### 6.6 Error Code Reference

Single source of truth for all error codes across Go, TypeScript, and UI layers:

| Code | HTTP Status | Resumable | Origin | Typical UI Action |
|---|---|---|---|---|
| `FORBIDDEN` | 403 | No | Receiver | Show "authentication failed" |
| `TRANSFER_NOT_FOUND` | 404 | No | Receiver | Show "transfer expired" |
| `TRANSFER_NOT_ACCEPTING` | 409 | Yes* | Receiver | Auto-retry or show "transfer unavailable" |
| `TRANSFER_IN_FLIGHT` | 409 | Yes | Receiver | Auto-retry after backoff |
| `OFFSET_MISMATCH` | 409 | No | Receiver | Show "resume offset mismatch" |
| `INTEGRITY_MISMATCH` | 409 | No | Receiver | Show "file corrupted during transfer" |
| `HASH_HEADER_MISMATCH` | 400 | No | Receiver | Show "file hash mismatch" |
| `MALFORMED_RANGE` | 400 | No | Sender bug | Show generic error |
| `RANGE_SIZE_MISMATCH` | 400 | No | Sender bug | Show generic error |
| `RANGE_END_INVALID` | 400 | No | Sender bug | Show generic error |
| `RANGE_START_INVALID` | 400 | No | Sender bug | Show generic error |
| `CONTENT_LENGTH_MISMATCH` | 400 | No | Sender bug | Show generic error |
| `MISSING_RANGE_FOR_RESUME` | 400 | No | Sender bug | Show generic error |
| `INCOMPLETE_BODY` | 400 | Yes | Receiver | Auto-retry (sender disconnected early) |
| `INVALID_SIZE` | 400 | No | Receiver | Show "invalid file" |
| `FILE_TOO_LARGE` | 413 | No | Either | Show "file too large (max: X)" |
| `TOO_MANY_TRANSFERS` | 503 | Yes | Receiver | Auto-retry after backoff |
| `INSUFFICIENT_DISK_SPACE` | 507 | No | Receiver | Show "not enough disk space" |
| `WRITE_ERROR` | 500 | Yes | Receiver | Auto-retry |
| `DISK_ERROR` | 500 | No | Receiver | Show "disk error" |
| `HASH_ERROR` | 500 | No | Receiver | Show "verification error" |
| `RENAME_ERROR` | 500 | No | Receiver | Show "could not save file" |
| `SEND_ERROR` | — | Yes | Sender | Auto-retry |
| `REMOTE_ERROR` | — | Yes | Sender | Auto-retry |
| `RESUME_QUERY_ERROR` | — | Yes | Sender | Auto-retry |
| `FILE_OPEN_ERROR` | — | No | Sender | Show "file not accessible" |
| `SEEK_ERROR` | — | No | Sender | Show generic error |
| `REQUEST_ERROR` | — | No | Sender | Show generic error |
| `INVALID_COMMAND` | — | No | IPC | Log error (developer bug) |
| `INVALID_PATH` | — | No | IPC | Show "invalid save path" |
| `FILE_NOT_FOUND` | — | No | IPC | Show "file not found" |
| `IS_DIRECTORY` | — | No | IPC | Show "cannot transfer directories" |
| `REJECTED` | — | No | MessageBus | Show "transfer rejected" |
| `PREPARE_TIMEOUT` | — | No | IPC | Show "preparation timed out" |

\* `TRANSFER_NOT_ACCEPTING` is resumable for the sender (receiver may be in a transient state), except when the receiver is in a terminal state — in that case the sender's special-case logic in `sendFile` checks if all bytes were already sent and treats it as success (see §14.6).

---

## 7. Go Implementation

### 7.1 Package Structure

```
packages/sidecar/internal/filetransfer/
  types.go       -- TransferID, TransferState, FileInfo, config constants
  manager.go     -- TransferManager: IPC handlers, orchestration, transfer tracking
  sender.go      -- HTTP PUT client: open file -> Seek(offset) -> progressReader -> HTTP PUT
  receiver.go    -- HTTP server on :9417: accept PUT -> stream to .partial -> verify -> rename
  progress.go    -- ProgressReader/ProgressWriter: rate-limited IPC progress events
  resume.go      -- .partial.meta manifest, HEAD endpoint, Content-Range handling
```

### 7.2 types.go

```go
package filetransfer

import (
	"context"
	"fmt"
	"io"
	"sync"
	"sync/atomic"
	"time"
)

// TransferID is a unique identifier for a file transfer.
// Format: "ft-<random-hex-16>" (64 bits of randomness; 32 bits was insufficient —
// birthday collision probability reaches 1% at ~9300 concurrent transfers)
type TransferID string

// TransferState represents the current state of a transfer.
type TransferState int32

const (
	StateRegistered   TransferState = 0 // Receiver registered, waiting for sender
	StateTransferring TransferState = 1 // Bytes flowing
	StateCompleted    TransferState = 2 // SHA-256 verified, file renamed
	StateFailed       TransferState = 3 // Error occurred
	StateCancelled    TransferState = 4 // Cancelled by either side
)

func (s TransferState) String() string {
	switch s {
	case StateRegistered:
		return "registered"
	case StateTransferring:
		return "transferring"
	case StateCompleted:
		return "completed"
	case StateFailed:
		return "failed"
	case StateCancelled:
		return "cancelled"
	default:
		return "unknown"
	}
}

// FileInfo contains metadata about a file being transferred.
type FileInfo struct {
	Name   string `json:"name"`
	Size   int64  `json:"size"`
	SHA256 string `json:"sha256"`
}

// Transfer tracks the state of a single file transfer on either side.
type Transfer struct {
	ID        TransferID `json:"id"`
	File      FileInfo   `json:"file"`
	SavePath  string     `json:"savePath,omitempty"`  // Receiver only
	FilePath  string     `json:"filePath,omitempty"`  // Sender only
	Token     string     `json:"token"`
	Direction string     `json:"direction"` // "send" or "receive"

	// State is accessed atomically (hot path in cancellableReader).
	// Use atomic.LoadInt32/StoreInt32 via the helper methods below.
	state int32

	// inFlight is 1 when a handlePUT goroutine is actively processing this transfer.
	// Separate from state so that Transferring can persist across connection drops
	// while still preventing concurrent handlers from interleaving writes.
	// On handler exit (success, error, EOF), defer clears this to 0, allowing retry PUTs.
	inFlight int32

	// Progress tracking (guarded by mu)
	BytesTransferred int64     `json:"bytesTransferred"`
	RegisteredAt     time.Time `json:"registeredAt"` // When the transfer was created/registered
	StartedAt        time.Time `json:"startedAt"`    // When bytes started flowing
	LastProgressAt   time.Time `json:"-"`

	// Internal
	cancel context.CancelFunc `json:"-"`
	mu     sync.Mutex         `json:"-"`
}

// State returns the current transfer state atomically.
func (t *Transfer) State() TransferState {
	return TransferState(atomic.LoadInt32(&t.state))
}

// SetState sets the transfer state atomically.
func (t *Transfer) SetState(s TransferState) {
	atomic.StoreInt32(&t.state, int32(s))
}

// Config holds file transfer configuration.
type Config struct {
	MaxFileSize          int64         // Default: 4GB
	MaxConcurrentRecv    int           // Max concurrent incoming transfers. Default: 5. 0 = unlimited.
	ProgressInterval     time.Duration // Default: 500ms
	ProgressBytes        int64         // Default: 256KB
	ShutdownTimeout      time.Duration // Default: 30s
	CompletedRetainTTL   time.Duration // How long to keep completed/failed transfers in map. Default: 5min
	RegisteredTimeoutTTL time.Duration // How long a registered (not yet started) transfer lives. Default: 2min
}

// DefaultConfig returns the default configuration.
func DefaultConfig() Config {
	return Config{
		MaxFileSize:          4 * 1024 * 1024 * 1024, // 4GB
		MaxConcurrentRecv:    5,
		ProgressInterval:     500 * time.Millisecond,
		ProgressBytes:        256 * 1024, // 256KB
		ShutdownTimeout:      30 * time.Second,
		CompletedRetainTTL:   5 * time.Minute,
		RegisteredTimeoutTTL: 2 * time.Minute,
	}
}

// cancellableReader wraps an io.Reader and checks the transfer state on every Read.
// If the transfer has been cancelled, Read returns an error immediately,
// breaking any blocking io.Copy on the receiver side.
// Uses atomic load for state — no mutex on the hot path.
type cancellableReader struct {
	reader   io.Reader
	transfer *Transfer
}

func (cr *cancellableReader) Read(p []byte) (int, error) {
	if cr.transfer.State() == StateCancelled {
		return 0, fmt.Errorf("transfer cancelled")
	}
	return cr.reader.Read(p)
}
```

### 7.3 manager.go

The `TransferManager` is the central orchestrator. It owns the HTTP server, tracks all transfers, and bridges IPC commands/events.

```go
package filetransfer

// Requires Go 1.22+ for ServeMux method patterns ("PUT /transfer/{transferId}").

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/user/truffle/packages/sidecar/internal/ipc"
)

// TransferManager manages file transfers over the Tailscale network.
type TransferManager struct {
	protocol *ipc.Protocol
	dialFunc func(ctx context.Context, network, addr string) (net.Conn, error)
	config   Config

	transfers map[TransferID]*Transfer
	mu        sync.RWMutex

	// recvSem limits concurrent incoming transfers. Buffered channel acts as a
	// counting semaphore: acquire = send to channel, release = receive from channel.
	// Capacity is MaxConcurrentRecv. Race-free by construction — no TOCTOU.
	recvSem chan struct{}

	server   *http.Server
	listener net.Listener

	ctx    context.Context
	cancel context.CancelFunc
}

// NewTransferManager creates a new TransferManager.
func NewTransferManager(
	protocol *ipc.Protocol,
	dialFunc func(ctx context.Context, network, addr string) (net.Conn, error),
	config Config,
) *TransferManager {
	ctx, cancel := context.WithCancel(context.Background())

	// If MaxConcurrentRecv <= 0, disable the limit (nil semaphore).
	// With 0 capacity, make(chan struct{}, 0) would create an unbuffered channel
	// where the non-blocking select always hits default, rejecting every transfer.
	var recvSem chan struct{}
	if config.MaxConcurrentRecv > 0 {
		recvSem = make(chan struct{}, config.MaxConcurrentRecv)
	}

	return &TransferManager{
		protocol:  protocol,
		dialFunc:  dialFunc,
		config:    config,
		transfers: make(map[TransferID]*Transfer),
		recvSem:   recvSem,
		ctx:       ctx,
		cancel:    cancel,
	}
}

// Start begins listening for incoming file transfers on the given listener.
func (m *TransferManager) Start(ln net.Listener) error {
	m.listener = ln

	mux := http.NewServeMux()
	mux.HandleFunc("PUT /transfer/{transferId}", m.handlePUT)
	mux.HandleFunc("HEAD /transfer/{transferId}", m.handleHEAD)

	m.server = &http.Server{
		Handler: mux,
		// ReadHeaderTimeout protects against slow-loris attacks: if a client connects
		// but sends headers very slowly, the connection is closed after this timeout.
		// Does NOT affect body reading (which can take minutes for large files).
		ReadHeaderTimeout: 10 * time.Second,
		// IdleTimeout limits how long a keep-alive connection sits idle between requests.
		// Not critical (each transfer is a single request) but prevents leaked connections.
		IdleTimeout: 60 * time.Second,
	}

	go func() {
		log.Printf("[FileTransfer] Listening on %s", ln.Addr())
		if err := m.server.Serve(ln); err != nil && err != http.ErrServerClosed {
			log.Printf("[FileTransfer] Server error: %v", err)
		}
	}()

	// Background goroutine to sweep completed/failed/cancelled transfers from the map.
	// Without this, every transfer leaks a Transfer struct for the lifetime of the process.
	go m.cleanupLoop()

	return nil
}

// cleanupLoop periodically removes terminal-state transfers from the map.
func (m *TransferManager) cleanupLoop() {
	ticker := time.NewTicker(1 * time.Minute)
	defer ticker.Stop()

	for {
		select {
		case <-m.ctx.Done():
			return
		case <-ticker.C:
			m.sweepCompletedTransfers()
		}
	}
}

// sweepCompletedTransfers removes transfers in terminal states (completed, failed, cancelled)
// that have been idle for longer than CompletedRetainTTL, and registered transfers that were
// never started within RegisteredTimeoutTTL.
func (m *TransferManager) sweepCompletedTransfers() {
	now := time.Now()
	m.mu.Lock()
	defer m.mu.Unlock()

	for id, t := range m.transfers {
		state := t.State() // atomic read, no lock needed

		t.mu.Lock()
		lastActivity := t.LastProgressAt
		if lastActivity.IsZero() {
			lastActivity = t.StartedAt
		}
		registeredAt := t.RegisteredAt
		t.mu.Unlock()

		// Sweep terminal-state transfers after CompletedRetainTTL
		if (state == StateCompleted || state == StateFailed || state == StateCancelled) &&
			now.Sub(lastActivity) > m.config.CompletedRetainTTL {
			delete(m.transfers, id)
			log.Printf("[FileTransfer] Swept transfer %s (state=%s)", id, state)
			continue
		}

		// Sweep registered transfers that were never started (sender never dialed).
		// Without this, a receiver that accepts but the sender disappears leaks forever.
		// Use the most recent of RegisteredAt and LastProgressAt — a valid HEAD request
		// from the sender extends the TTL (proves liveness even if transfer hasn't started).
		if state == StateRegistered {
			lastSeen := registeredAt
			if lastActivity.After(lastSeen) {
				lastSeen = lastActivity
			}
			if now.Sub(lastSeen) > m.config.RegisteredTimeoutTTL {
				if t.cancel != nil {
					t.cancel()
				}
				delete(m.transfers, id)
				log.Printf("[FileTransfer] Swept stale registered transfer %s (last seen %s ago)",
					id, now.Sub(lastSeen))
			}
		}
	}
}

// Stop gracefully shuts down the transfer manager.
func (m *TransferManager) Stop() error {
	m.cancel()

	// Cancel all active transfers
	m.mu.RLock()
	for _, t := range m.transfers {
		if t.cancel != nil {
			t.cancel()
		}
	}
	m.mu.RUnlock()

	// Shut down HTTP server
	if m.server != nil {
		ctx, cancel := context.WithTimeout(context.Background(), m.config.ShutdownTimeout)
		defer cancel()
		return m.server.Shutdown(ctx)
	}
	return nil
}

// RegisterHandlers registers IPC command handlers on the protocol.
// Uses protocol.OnCommand() to match existing handler registration pattern in main.go.
func (m *TransferManager) RegisterHandlers() {
	m.protocol.OnCommand("file:prepare", m.handlePrepare)
	m.protocol.OnCommand("file:send", m.handleSend)
	m.protocol.OnCommand("file:accept", m.handleAccept)
	m.protocol.OnCommand("file:reject", m.handleReject)
	m.protocol.OnCommand("file:cancel", m.handleCancel)
	m.protocol.OnCommand("file:list", m.handleList)
}

// handlePrepare stats a local file and computes its SHA-256.
// IPC: file:prepare { transferId, filePath }
// Emits: file:prepared { transferId, name, size, sha256 }
//
// SHA-256 computation runs in a background goroutine to avoid blocking the IPC
// read loop for large files (multi-GB files take several seconds to hash).
func (m *TransferManager) handlePrepare(data json.RawMessage) {
	var cmd struct {
		TransferID string `json:"transferId"`
		FilePath   string `json:"filePath"`
	}
	if err := json.Unmarshal(data, &cmd); err != nil {
		m.sendEvent("file:error", map[string]interface{}{
			"transferId": cmd.TransferID,
			"code":       "INVALID_COMMAND",
			"message":    err.Error(),
		})
		return
	}

	// Stat the file (fast, safe to do inline)
	info, err := os.Stat(cmd.FilePath)
	if err != nil {
		m.sendEvent("file:error", map[string]interface{}{
			"transferId": cmd.TransferID,
			"code":       "FILE_NOT_FOUND",
			"message":    err.Error(),
		})
		return
	}

	if info.IsDir() {
		m.sendEvent("file:error", map[string]interface{}{
			"transferId": cmd.TransferID,
			"code":       "IS_DIRECTORY",
			"message":    "cannot transfer directories",
		})
		return
	}

	if info.Size() > m.config.MaxFileSize {
		m.sendEvent("file:error", map[string]interface{}{
			"transferId": cmd.TransferID,
			"code":       "FILE_TOO_LARGE",
			"message":    fmt.Sprintf("file size %d exceeds limit %d", info.Size(), m.config.MaxFileSize),
		})
		return
	}

	// Compute SHA-256 in a goroutine (streaming, ~500MB/s on modern hardware).
	// For a 4GB file this takes ~8 seconds — must not block the IPC read loop.
	// Emits file:preparing_progress events so the UI can show "Preparing... 45%"
	// instead of silence during long hashes.
	fileName := info.Name()
	fileSize := info.Size()
	go func() {
		hash, err := hashFile(cmd.FilePath, fileSize, func(bytesHashed int64) {
			var percent float64
			if fileSize > 0 {
				percent = float64(bytesHashed) / float64(fileSize) * 100
			}
			m.sendEvent("file:preparing_progress", map[string]interface{}{
				"transferId":  cmd.TransferID,
				"bytesHashed": bytesHashed,
				"totalBytes":  fileSize,
				"percent":     percent,
			})
		})
		if err != nil {
			m.sendEvent("file:error", map[string]interface{}{
				"transferId": cmd.TransferID,
				"code":       "HASH_ERROR",
				"message":    err.Error(),
			})
			return
		}

		m.sendEvent("file:prepared", map[string]interface{}{
			"transferId": cmd.TransferID,
			"name":       fileName,
			"size":       fileSize,
			"sha256":     hash,
		})
	}()
}

// handleAccept registers a transfer the local node is willing to receive.
// IPC: file:accept { transferId, savePath, token, fileInfo }
// The HTTP server will accept PUT requests matching this transferId + token.
func (m *TransferManager) handleAccept(data json.RawMessage) {
	var cmd struct {
		TransferID string   `json:"transferId"`
		SavePath   string   `json:"savePath"`
		Token      string   `json:"token"`
		FileInfo   FileInfo `json:"fileInfo"`
	}
	if err := json.Unmarshal(data, &cmd); err != nil {
		m.sendEvent("file:error", map[string]interface{}{
			"transferId": cmd.TransferID,
			"code":       "INVALID_COMMAND",
			"message":    err.Error(),
		})
		return
	}

	// Validate and normalize save path (prevent path traversal)
	cleanedPath, err := validateSavePath(cmd.SavePath)
	if err != nil {
		m.sendEvent("file:error", map[string]interface{}{
			"transferId": cmd.TransferID,
			"code":       "INVALID_PATH",
			"message":    err.Error(),
		})
		return
	}

	// Create context at registration time. The context itself is not stored on the
	// Transfer — receiver-side cancellation works via atomic state checks in
	// cancellableReader, which is faster than select on ctx.Done() in the hot path.
	// We keep the cancel func for two purposes:
	//   1. handleCancel calls it alongside setting StateCancelled (belt + suspenders).
	//   2. On TransferManager shutdown, Stop() iterates all transfers and calls cancel()
	//      to unblock any pending operations that select on m.ctx.
	_, cancel := context.WithCancel(m.ctx)

	transfer := &Transfer{
		ID:           TransferID(cmd.TransferID),
		File:         cmd.FileInfo,
		SavePath:     cleanedPath,
		Token:        cmd.Token,
		Direction:    "receive",
		RegisteredAt: time.Now(),
		cancel:       cancel,
	}
	transfer.SetState(StateRegistered)

	m.mu.Lock()
	m.transfers[transfer.ID] = transfer
	m.mu.Unlock()

	log.Printf("[FileTransfer] Registered receive for %s -> %s", cmd.TransferID, cmd.SavePath)
}

// handleSend initiates sending a file to a remote node.
// IPC: file:send { transferId, filePath, targetAddr, token, fileInfo }
func (m *TransferManager) handleSend(data json.RawMessage) {
	var cmd struct {
		TransferID string   `json:"transferId"`
		FilePath   string   `json:"filePath"`
		TargetAddr string   `json:"targetAddr"` // e.g., "hostname.tailnet.ts.net:9417"
		Token      string   `json:"token"`
		FileInfo   FileInfo `json:"fileInfo"`
	}
	if err := json.Unmarshal(data, &cmd); err != nil {
		m.sendEvent("file:error", map[string]interface{}{
			"transferId": cmd.TransferID,
			"code":       "INVALID_COMMAND",
			"message":    err.Error(),
		})
		return
	}

	ctx, cancel := context.WithCancel(m.ctx)
	now := time.Now()
	transfer := &Transfer{
		ID:           TransferID(cmd.TransferID),
		File:         cmd.FileInfo,
		FilePath:     cmd.FilePath,
		Token:        cmd.Token,
		Direction:    "send",
		RegisteredAt: now,
		StartedAt:    now,
		cancel:       cancel,
	}
	transfer.SetState(StateTransferring)

	m.mu.Lock()
	m.transfers[transfer.ID] = transfer
	m.mu.Unlock()

	// Send in background goroutine
	go m.sendFile(ctx, transfer, cmd.TargetAddr)
}

// handleCancel cancels an active transfer.
// IPC: file:cancel { transferId }
func (m *TransferManager) handleCancel(data json.RawMessage) {
	var cmd struct {
		TransferID string `json:"transferId"`
	}
	if err := json.Unmarshal(data, &cmd); err != nil {
		return
	}

	m.mu.RLock()
	t, ok := m.transfers[TransferID(cmd.TransferID)]
	m.mu.RUnlock()

	if ok {
		t.SetState(StateCancelled) // atomic write; cancellableReader sees this immediately
		if t.cancel != nil {
			t.cancel() // cancels sender's HTTP request context
		}
	}

	if ok {
		m.sendEvent("file:cancelled", map[string]interface{}{
			"transferId": cmd.TransferID,
		})
	}
}

// handleReject removes a registered (not yet started) transfer.
// IPC: file:reject { transferId }
func (m *TransferManager) handleReject(data json.RawMessage) {
	var cmd struct {
		TransferID string `json:"transferId"`
	}
	if err := json.Unmarshal(data, &cmd); err != nil {
		return
	}

	m.mu.Lock()
	delete(m.transfers, TransferID(cmd.TransferID))
	m.mu.Unlock()
}

// handleList returns all active transfers.
// IPC: file:list {}
// Emits: file:list { transfers: [...] }
func (m *TransferManager) handleList(data json.RawMessage) {
	m.mu.RLock()
	type transferJSON struct {
		ID               TransferID `json:"id"`
		State            string     `json:"state"`
		File             FileInfo   `json:"file"`
		Direction        string     `json:"direction"`
		BytesTransferred int64      `json:"bytesTransferred"`
	}
	list := make([]transferJSON, 0, len(m.transfers))
	for _, t := range m.transfers {
		t.mu.Lock()
		list = append(list, transferJSON{
			ID:               t.ID,
			State:            t.State().String(),
			File:             t.File,
			Direction:        t.Direction,
			BytesTransferred: t.BytesTransferred,
		})
		t.mu.Unlock()
	}
	m.mu.RUnlock()

	m.sendEvent("file:list", map[string]interface{}{
		"transfers": list,
	})
}

// sendEvent wraps protocol.Send() to match the existing pattern in protocol.go.
// The protocol uses Send(Event{Event: "type", Data: data}) rather than a direct SendEvent method.
func (m *TransferManager) sendEvent(eventType string, data interface{}) {
	m.protocol.Send(ipc.Event{Event: eventType, Data: data})
}

// getTransfer retrieves a transfer by ID, validating the token format and value.
func (m *TransferManager) getTransfer(id TransferID, token string) (*Transfer, error) {
	// Validate token format: must be exactly 64 hex characters (32 random bytes)
	if len(token) != 64 {
		return nil, fmt.Errorf("invalid token format for transfer: %s", id)
	}
	for _, c := range token {
		if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')) {
			return nil, fmt.Errorf("invalid token format for transfer: %s", id)
		}
	}

	m.mu.RLock()
	defer m.mu.RUnlock()

	t, ok := m.transfers[id]
	if !ok {
		return nil, fmt.Errorf("transfer not found: %s", id)
	}
	if t.Token != token {
		return nil, fmt.Errorf("invalid token for transfer: %s", id)
	}
	return t, nil
}

// hashFile computes the SHA-256 of a file, streaming in 32KB chunks.
// The optional onProgress callback is called periodically with bytesHashed so far,
// rate-limited to avoid flooding (every 256KB or 200ms, whichever comes first).
func hashFile(path string, totalSize int64, onProgress func(bytesHashed int64)) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()

	h := sha256.New()
	buf := make([]byte, 32*1024)
	var hashed int64
	var lastReport int64
	lastReportTime := time.Now()

	for {
		n, err := f.Read(buf)
		if n > 0 {
			h.Write(buf[:n])
			hashed += int64(n)

			if onProgress != nil {
				now := time.Now()
				if hashed-lastReport >= 256*1024 || now.Sub(lastReportTime) >= 200*time.Millisecond {
					onProgress(hashed)
					lastReport = hashed
					lastReportTime = now
				}
			}
		}
		if err != nil {
			if err == io.EOF {
				break
			}
			return "", err
		}
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

// validateSavePath checks for path traversal attacks.
// Design decision: we allow arbitrary absolute paths (the user chose this path in a save dialog).
// We reject relative paths and paths containing ".." components after cleaning. We do NOT
// require filepath.Clean(p) == p, because OS file dialogs can produce paths with trailing
// slashes, double slashes, or "." segments that are benign.
// Symlink-based attacks are out of scope because the Node.js caller controls the path
// (it comes from the UI, not from the network).
// If we later need to restrict to a specific directory, use filepath.EvalSymlinks + prefix check.
func validateSavePath(p string) (string, error) {
	if !filepath.IsAbs(p) {
		return "", fmt.Errorf("path must be absolute: %q", p)
	}
	cleaned := filepath.Clean(p)
	// Reject if the cleaned path still contains ".." (traversal beyond root)
	for _, part := range strings.Split(cleaned, string(filepath.Separator)) {
		if part == ".." {
			return "", fmt.Errorf("path contains traversal: %q", p)
		}
	}
	// Ensure parent directory exists and is a directory (fail fast rather than at write time).
	// We do not check write permissions here — that will fail naturally at os.Create time.
	// Checking permissions is unreliable (ACLs, SELinux, network mounts, etc.).
	dir := filepath.Dir(cleaned)
	info, err := os.Stat(dir)
	if err != nil {
		return "", fmt.Errorf("parent directory does not exist: %q", dir)
	}
	if !info.IsDir() {
		return "", fmt.Errorf("parent is not a directory: %q", dir)
	}
	return cleaned, nil
}
```

### 7.4 sender.go

```go
package filetransfer

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"strconv"
	"time"
)

// sendFile opens the local file and streams it to the remote receiver via HTTP PUT.
// The sender does NOT compute SHA-256 during send — the hash was already computed in
// file:prepare and is sent in X-File-SHA256 for the receiver to verify. This avoids
// the resume bug where TeeReader after Seek(offset) would only hash the suffix.
func (m *TransferManager) sendFile(ctx context.Context, t *Transfer, targetAddr string) {
	// Always clean up the context when done, regardless of success or failure.
	// Without this, the context tree leaks until the TransferManager shuts down.
	defer t.cancel()

	// Open the file
	f, err := os.Open(t.FilePath)
	if err != nil {
		m.failTransfer(t, "FILE_OPEN_ERROR", err.Error())
		return
	}
	defer f.Close()

	// Check for resume: query receiver's offset via HEAD.
	// Errors are propagated (not swallowed) so 403/404 fail fast instead of
	// silently starting from 0 and then failing on the PUT anyway.
	offset, err := m.queryResumeOffset(ctx, t, targetAddr)
	if err != nil {
		m.failTransfer(t, "RESUME_QUERY_ERROR", err.Error())
		return
	}

	if offset > 0 {
		if _, err := f.Seek(offset, io.SeekStart); err != nil {
			m.failTransfer(t, "SEEK_ERROR", err.Error())
			return
		}
		log.Printf("[FileTransfer] Resuming %s from offset %d", t.ID, offset)
	}

	t.mu.Lock()
	t.BytesTransferred = offset
	t.StartedAt = time.Now()
	t.mu.Unlock()

	// Build progress-reporting reader: file -> progress -> HTTP body
	// No hash computation here — receiver is responsible for SHA-256 verification.
	progressReader := newProgressReader(f, t.File.Size, offset, func(bytesRead int64) {
		m.emitProgress(t, bytesRead)
	}, m.config)

	// Build HTTP request
	url := fmt.Sprintf("http://%s/transfer/%s", targetAddr, t.ID)
	req, err := http.NewRequestWithContext(ctx, http.MethodPut, url, progressReader)
	if err != nil {
		m.failTransfer(t, "REQUEST_ERROR", err.Error())
		return
	}

	req.Header.Set("X-Transfer-Token", t.Token)
	req.Header.Set("X-File-Name", t.File.Name)
	req.Header.Set("X-File-SHA256", t.File.SHA256) // Pre-computed in file:prepare

	// Set Content-Length via req.ContentLength; net/http manages the header.
	// Do NOT also set the Content-Length header manually — they can conflict.
	req.ContentLength = t.File.Size - offset

	if offset > 0 {
		req.Header.Set("Content-Range",
			fmt.Sprintf("bytes %d-%d/%d", offset, t.File.Size-1, t.File.Size))
	}

	// Create HTTP client that dials through Tailscale.
	// DialContext ignores the addr parameter and always dials targetAddr because
	// the HTTP URL host is the Tailscale address (no redirects/proxies in this path).
	// Client.Timeout is left empty because large files can take a long time.
	// ResponseHeaderTimeout ensures we fail fast if the peer accepts TCP but
	// never sends HTTP headers (e.g., deadlocked or crashed after accept).
	// TCP keep-alive (30s) ensures dead connections tear down promptly instead of
	// hanging for the OS default (~2 hours) if the remote peer vanishes mid-transfer.
	client := &http.Client{
		Transport: &http.Transport{
			DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
				conn, err := m.dialFunc(ctx, "tcp", targetAddr)
				if err != nil {
					return nil, err
				}
				// Enable TCP keep-alive if the connection supports it.
				// tsnet connections are net.Conn; if the underlying type supports
				// SetKeepAlive, use it. Otherwise, keep-alive relies on OS defaults.
				if tc, ok := conn.(interface {
					SetKeepAlive(bool) error
					SetKeepAlivePeriod(time.Duration) error
				}); ok {
					tc.SetKeepAlive(true)
					tc.SetKeepAlivePeriod(30 * time.Second)
				}
				return conn, nil
			},
			ResponseHeaderTimeout: 30 * time.Second,
		},
	}

	log.Printf("[FileTransfer] Sending %s (%d bytes, offset %d) to %s",
		t.ID, t.File.Size, offset, targetAddr)

	resp, err := client.Do(req)
	if err != nil {
		if ctx.Err() != nil {
			// Cancelled by user or shutdown
			return
		}
		m.failTransfer(t, "SEND_ERROR", err.Error())
		return
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		// Parse structured JSON error from receiver (see writeJSONError in receiver.go)
		var errResp struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		}
		if jsonErr := json.NewDecoder(io.LimitReader(resp.Body, 1024)).Decode(&errResp); jsonErr == nil {
			// Special case: if receiver says TRANSFER_NOT_ACCEPTING and we already
			// sent all bytes, the transfer likely completed but we missed the 200
			// (e.g., network hiccup on the response path). Treat as success.
			if errResp.Code == "TRANSFER_NOT_ACCEPTING" && offset+req.ContentLength >= t.File.Size {
				log.Printf("[FileTransfer] %s: receiver returned TRANSFER_NOT_ACCEPTING after full send — treating as success", t.ID)
				t.SetState(StateCompleted)
				m.sendEvent("file:complete", map[string]interface{}{
					"transferId": string(t.ID),
					"sha256":     t.File.SHA256,
					"size":       t.File.Size,
					"duration":   time.Since(t.StartedAt).Milliseconds(),
					"direction":  "send",
				})
				return
			}
			m.failTransfer(t, errResp.Code, fmt.Sprintf("HTTP %d: %s", resp.StatusCode, errResp.Message))
		} else {
			body, _ := io.ReadAll(io.LimitReader(resp.Body, 1024))
			m.failTransfer(t, "REMOTE_ERROR", fmt.Sprintf("HTTP %d: %s", resp.StatusCode, body))
		}
		return
	}

	// Transfer complete
	t.SetState(StateCompleted)

	m.sendEvent("file:complete", map[string]interface{}{
		"transferId": string(t.ID),
		"sha256":     t.File.SHA256,
		"size":       t.File.Size,
		"duration":   time.Since(t.StartedAt).Milliseconds(),
		"direction":  "send",
	})

	log.Printf("[FileTransfer] Send complete: %s (%d bytes in %s)",
		t.ID, t.File.Size, time.Since(t.StartedAt))
}

// queryResumeOffset asks the receiver how many bytes it already has.
func (m *TransferManager) queryResumeOffset(ctx context.Context, t *Transfer, targetAddr string) (int64, error) {
	url := fmt.Sprintf("http://%s/transfer/%s", targetAddr, t.ID)
	req, err := http.NewRequestWithContext(ctx, http.MethodHead, url, nil)
	if err != nil {
		return 0, err
	}
	req.Header.Set("X-Transfer-Token", t.Token)

	client := &http.Client{
		Transport: &http.Transport{
			DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
				return m.dialFunc(ctx, "tcp", targetAddr)
			},
			ResponseHeaderTimeout: 10 * time.Second,
		},
		Timeout: 10 * time.Second,
	}

	resp, err := client.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()

	// Propagate meaningful errors instead of silently starting from 0.
	// 403/404 mean the receiver isn't ready — failing fast avoids a wasted PUT round-trip.
	switch resp.StatusCode {
	case http.StatusOK:
		// Continue to parse offset below
	case http.StatusForbidden:
		return 0, fmt.Errorf("resume query: forbidden (invalid token)")
	case http.StatusNotFound:
		return 0, fmt.Errorf("resume query: transfer not registered on receiver")
	default:
		return 0, fmt.Errorf("resume query: unexpected status %d", resp.StatusCode)
	}

	offsetStr := resp.Header.Get("Upload-Offset")
	if offsetStr == "" {
		return 0, nil
	}

	offset, err := strconv.ParseInt(offsetStr, 10, 64)
	if err != nil {
		return 0, fmt.Errorf("invalid Upload-Offset header: %q", offsetStr)
	}
	return offset, nil
}

// isResumableError returns whether the given error code allows the transfer to be
// retried from the last offset without re-accepting.
//
// Used by the sender to decide whether to retry. On the receiver side, resumable
// errors go through softFailTransfer (which keeps StateTransferring) — this function
// is not consulted on the receiver path.
//
// Resumable (network/transient):
//   SEND_ERROR, REMOTE_ERROR, RESUME_QUERY_ERROR, TOO_MANY_TRANSFERS,
//   TRANSFER_NOT_ACCEPTING, TRANSFER_IN_FLIGHT, WRITE_ERROR, INCOMPLETE_BODY
//
// Not resumable (integrity/protocol/auth):
//   INTEGRITY_MISMATCH, HASH_HEADER_MISMATCH, HASH_ERROR, FORBIDDEN,
//   INVALID_COMMAND, INVALID_PATH, INVALID_SIZE, FILE_NOT_FOUND,
//   IS_DIRECTORY, FILE_TOO_LARGE, FILE_OPEN_ERROR, SEEK_ERROR,
//   MALFORMED_RANGE, RANGE_SIZE_MISMATCH, RANGE_END_INVALID,
//   RANGE_START_INVALID, CONTENT_LENGTH_MISMATCH, RENAME_ERROR,
//   INSUFFICIENT_DISK_SPACE, RESUME_MISMATCH, REJECTED, PREPARE_TIMEOUT,
//   MISSING_RANGE_FOR_RESUME, OFFSET_MISMATCH
func isResumableError(code string) bool {
	switch code {
	case "SEND_ERROR", "REMOTE_ERROR", "RESUME_QUERY_ERROR",
		"TOO_MANY_TRANSFERS", "TRANSFER_NOT_ACCEPTING", "TRANSFER_IN_FLIGHT",
		"WRITE_ERROR", "INCOMPLETE_BODY":
		return true
	default:
		return false
	}
}

// failTransfer marks a transfer as terminally failed and emits an error event.
// On the receiver side, use this ONLY for non-resumable errors (integrity mismatch,
// protocol violations). For resumable receiver errors, use softFailTransfer instead.
// On the sender side, this is always used (sender state doesn't block receiver retries).
func (m *TransferManager) failTransfer(t *Transfer, code, message string) {
	t.SetState(StateFailed) // atomic — terminal, no retry allowed

	m.sendEvent("file:error", map[string]interface{}{
		"transferId": string(t.ID),
		"code":       code,
		"message":    message,
		"resumable":  isResumableError(code),
	})

	log.Printf("[FileTransfer] Failed %s: %s: %s", t.ID, code, message)
}

// softFailTransfer emits a resumable error WITHOUT changing state.
// The transfer stays in StateTransferring so a retry PUT can proceed once
// the inFlight defer releases the lock. Use this for transient/network errors
// on the receiver side (WRITE_ERROR, DISK_ERROR from transient I/O, etc.).
func (m *TransferManager) softFailTransfer(t *Transfer, code, message string) {
	// State intentionally NOT changed — remains Transferring for retry.
	// inFlight is cleared by the caller's defer, allowing the next PUT.

	m.sendEvent("file:error", map[string]interface{}{
		"transferId": string(t.ID),
		"code":       code,
		"message":    message,
		"resumable":  true,
	})

	log.Printf("[FileTransfer] Soft-failed %s (resumable): %s: %s", t.ID, code, message)
}
```

### 7.5 receiver.go

```go
package filetransfer

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync/atomic"
	"syscall"
	"time"
)

// httpError is a stable JSON error response body for non-200 HTTP responses.
// This allows the sender to map errors to FileTransferErrorEvent codes without string parsing.
type httpError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

// writeJSONError writes a structured JSON error response.
func writeJSONError(w http.ResponseWriter, status int, code, message string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(httpError{Code: code, Message: message}) // best-effort; client may have disconnected
}

// handlePUT receives file bytes from a sender.
func (m *TransferManager) handlePUT(w http.ResponseWriter, r *http.Request) {
	transferId := TransferID(r.PathValue("transferId"))
	token := r.Header.Get("X-Transfer-Token")

	// Validate transfer and token
	t, err := m.getTransfer(transferId, token)
	if err != nil {
		if strings.Contains(err.Error(), "not found") {
			writeJSONError(w, http.StatusNotFound, "TRANSFER_NOT_FOUND", "transfer not found")
		} else {
			writeJSONError(w, http.StatusForbidden, "FORBIDDEN", "invalid or missing transfer token")
		}
		return
	}

	// Enforce max concurrent incoming transfers via channel semaphore.
	// Non-blocking acquire: if the channel is full, all slots are taken.
	// Race-free by construction — no TOCTOU window between count and start.
	// If recvSem is nil, concurrency limiting is disabled (MaxConcurrentRecv <= 0).
	if m.recvSem != nil {
		select {
		case m.recvSem <- struct{}{}:
			// Acquired slot — release when handler exits
			defer func() { <-m.recvSem }()
		default:
			writeJSONError(w, http.StatusServiceUnavailable, "TOO_MANY_TRANSFERS",
				fmt.Sprintf("max concurrent transfers (%d) reached", m.config.MaxConcurrentRecv))
			return
		}
	}

	// Enforce MaxFileSize on the receive path (defense-in-depth; sender checks in handlePrepare)
	if t.File.Size > m.config.MaxFileSize {
		writeJSONError(w, http.StatusRequestEntityTooLarge, "FILE_TOO_LARGE",
			fmt.Sprintf("file size %d exceeds limit %d", t.File.Size, m.config.MaxFileSize))
		return
	}
	if t.File.Size <= 0 {
		writeJSONError(w, http.StatusBadRequest, "INVALID_SIZE", "invalid file size")
		return
	}

	// Guard against concurrent handlers and determine if this is a fresh start or resume.
	//
	// State machine allows two entry paths:
	//   1. Registered -> Transferring (first PUT, fresh transfer)
	//   2. Transferring + !inFlight    (retry PUT after connection drop, resume)
	//
	// The inFlight flag is separate from state so that Transferring persists across
	// connection drops (preserving partial file + metadata), while still preventing
	// two handlers from writing to the same file simultaneously.
	state := t.State()
	switch {
	case state == StateRegistered:
		// Fresh transfer: transition to Transferring
		if !atomic.CompareAndSwapInt32(&t.state, int32(StateRegistered), int32(StateTransferring)) {
			// Another goroutine beat us — race between two PUTs
			writeJSONError(w, http.StatusConflict, "TRANSFER_NOT_ACCEPTING",
				"transfer not accepting data (state changed concurrently)")
			return
		}
	case state == StateTransferring:
		// Possible resume after connection drop — allowed only if no handler is active
		// (inFlight check below will gate this)
	default:
		// Terminal state (Completed, Failed, Cancelled)
		writeJSONError(w, http.StatusConflict, "TRANSFER_NOT_ACCEPTING",
			fmt.Sprintf("transfer in terminal state: %s", state))
		return
	}

	// Acquire in-flight lock. Only one handler can actively write at a time.
	// If CAS fails, another handler is already running — return 409.
	if !atomic.CompareAndSwapInt32(&t.inFlight, 0, 1) {
		writeJSONError(w, http.StatusConflict, "TRANSFER_IN_FLIGHT",
			"another handler is actively processing this transfer")
		return
	}
	// Release in-flight on exit (success, error, disconnect, cancel).
	// This allows a retry PUT to proceed after this handler exits.
	defer atomic.StoreInt32(&t.inFlight, 0)

	t.mu.Lock()
	t.StartedAt = time.Now()
	t.mu.Unlock()
	// Note: t.cancel was already set in handleAccept at registration time.
	// handleCancel sets state to StateCancelled (atomic) and calls t.cancel().
	// cancellableReader checks the atomic state on every Read() to break io.Copy.

	// Parse and validate Content-Range header.
	// If this is a resume (state was already Transferring), Content-Range is REQUIRED.
	// Without it, offset defaults to 0 and os.Create would truncate the existing .partial
	// file, destroying all previously received data. If the sender genuinely wants to
	// restart from scratch, it should use a new transferId.
	var offset int64
	cr := r.Header.Get("Content-Range")
	if state == StateTransferring && cr == "" {
		writeJSONError(w, http.StatusBadRequest, "MISSING_RANGE_FOR_RESUME",
			"Content-Range header required when resuming a transfer")
		return
	}
	if cr != "" {
		var start, end, total int64
		n, _ := fmt.Sscanf(cr, "bytes %d-%d/%d", &start, &end, &total)
		if n != 3 {
			writeJSONError(w, http.StatusBadRequest, "MALFORMED_RANGE", "malformed Content-Range header")
			return
		}
		if total != t.File.Size {
			writeJSONError(w, http.StatusBadRequest, "RANGE_SIZE_MISMATCH",
				fmt.Sprintf("Content-Range total %d != expected file size %d", total, t.File.Size))
			return
		}
		if end != total-1 {
			writeJSONError(w, http.StatusBadRequest, "RANGE_END_INVALID",
				"Content-Range end must be total-1 for full remainder")
			return
		}
		if start < 0 || start > total {
			writeJSONError(w, http.StatusBadRequest, "RANGE_START_INVALID",
				"Content-Range start out of bounds")
			return
		}
		offset = start

		// Validate that the sender's offset matches our actual .partial size.
		// This prevents corruption from out-of-sync senders (e.g., sender thinks
		// 100MB was sent but receiver only has 50MB on disk).
		actual := currentOffset(t.SavePath)
		if start != actual {
			writeJSONError(w, http.StatusConflict, "OFFSET_MISMATCH",
				fmt.Sprintf("Content-Range start %d != actual partial size %d", start, actual))
			return
		}
	}

	// Validate Content-Length matches expected remaining bytes
	expectedLen := t.File.Size - offset
	if r.ContentLength > 0 && r.ContentLength != expectedLen {
		writeJSONError(w, http.StatusBadRequest, "CONTENT_LENGTH_MISMATCH",
			fmt.Sprintf("Content-Length %d != expected remaining %d", r.ContentLength, expectedLen))
		return
	}

	// Pre-flight disk space check. Uses syscall.Statfs to query available space.
	// Placed after Content-Range parsing so we check only the REMAINING bytes needed,
	// not the total file size. On resume at 50GB of a 100GB file, we only need 50GB free.
	// Not atomic (another process could fill the disk), but catches the 90% case early.
	remainingBytes := t.File.Size - offset
	if err := checkDiskSpace(filepath.Dir(t.SavePath), remainingBytes); err != nil {
		writeJSONError(w, http.StatusInsufficientStorage, "INSUFFICIENT_DISK_SPACE", err.Error())
		m.failTransfer(t, "INSUFFICIENT_DISK_SPACE", err.Error())
		return
	}

	// Open or create partial file
	partialPath := t.SavePath + ".partial"
	// Ensure parent directory exists
	if err := os.MkdirAll(filepath.Dir(partialPath), 0755); err != nil {
		writeJSONError(w, http.StatusInternalServerError, "DISK_ERROR", "failed to create directory")
		m.failTransfer(t, "DISK_ERROR", err.Error())
		return
	}

	var f *os.File
	if offset > 0 {
		// O_WRONLY|O_CREATE: open existing or create if first resume attempt finds no partial
		f, err = os.OpenFile(partialPath, os.O_WRONLY|os.O_CREATE, 0644)
		if err != nil {
			writeJSONError(w, http.StatusInternalServerError, "DISK_ERROR", "failed to open partial file")
			m.failTransfer(t, "DISK_ERROR", err.Error())
			return
		}
		// No need to re-validate file size vs offset here — currentOffset() already
		// verified that Content-Range start matches the actual .partial size.
		if _, err := f.Seek(offset, io.SeekStart); err != nil {
			f.Close()
			writeJSONError(w, http.StatusInternalServerError, "DISK_ERROR", "failed to seek in partial file")
			m.failTransfer(t, "DISK_ERROR", err.Error())
			return
		}
	} else {
		f, err = os.Create(partialPath)
		if err != nil {
			writeJSONError(w, http.StatusInternalServerError, "DISK_ERROR", "failed to create file")
			m.failTransfer(t, "DISK_ERROR", err.Error())
			return
		}
	}
	defer f.Close()

	// Write manifest for resume
	if err := writePartialMeta(t, offset); err != nil {
		log.Printf("[FileTransfer] Warning: failed to write manifest: %v", err)
	}

	// Stream body to disk with SHA-256 computation and progress reporting
	hash := sha256.New()

	// If resuming, re-hash existing partial data to rebuild SHA-256 state.
	// This takes ~offset/500MB per second on modern hardware. For a 10GB partial that's ~20s.
	// Log the duration so we can monitor if it becomes a UX bottleneck on flaky networks.
	if offset > 0 {
		hashStart := time.Now()
		if err := hashPartialFile(partialPath, offset, hash); err != nil {
			writeJSONError(w, http.StatusInternalServerError, "HASH_ERROR", "failed to hash partial file")
			m.failTransfer(t, "HASH_ERROR", err.Error())
			return
		}
		log.Printf("[FileTransfer] Re-hashed %d bytes of partial %s in %s",
			offset, t.ID, time.Since(hashStart))
	}

	// Wrap r.Body in cancellableReader so that handleCancel can break the io.Copy.
	// On every Read(), it checks t.State() (atomic); if cancelled, returns error immediately.
	// Then cap with io.LimitReader to enforce exact byte count — prevents a buggy or
	// malicious sender from writing more bytes than expected (which would corrupt the file).
	// If Content-Length was -1 (unknown/chunked), this is the ONLY size enforcement.
	body := &cancellableReader{reader: r.Body, transfer: t}
	limitedBody := io.LimitReader(body, expectedLen)
	teeReader := io.TeeReader(limitedBody, hash)
	progressReader := newProgressReader(teeReader, t.File.Size, offset, func(bytesRead int64) {
		m.emitProgress(t, bytesRead)
	}, m.config)

	written, err := io.Copy(f, progressReader)
	if err != nil {
		// Check if this was a cancellation
		if t.State() == StateCancelled {
			// Clean up partial file on cancel
			f.Close()
			os.Remove(partialPath)
			os.Remove(t.SavePath + ".partial.meta")
			return
		}
		// Soft-fail: keep StateTransferring so the sender can retry after the
		// inFlight defer releases the lock. WRITE_ERROR is typically a connection
		// drop or transient I/O — the partial file is preserved for resume.
		m.softFailTransfer(t, "WRITE_ERROR", err.Error())
		return
	}

	// Verify exact byte count. LimitReader caps oversend; this catches undersend.
	// Without this check, a sender that disconnects early would produce a truncated
	// file that fails SHA-256 verification — but detecting it here gives a clearer error.
	if written != expectedLen {
		writeJSONError(w, http.StatusBadRequest, "INCOMPLETE_BODY",
			fmt.Sprintf("received %d bytes, expected %d", written+offset, t.File.Size))
		m.softFailTransfer(t, "INCOMPLETE_BODY",
			fmt.Sprintf("received %d bytes, expected %d", written, expectedLen))
		return
	}

	log.Printf("[FileTransfer] Received %d bytes for %s", written+offset, t.ID)

	// Verify SHA-256 against the registered value from file:accept (not the HTTP header).
	// The receiver pinned the expected hash at accept time; trusting X-File-SHA256 from the
	// sender would let a compromised sender supply a different hash to "pass" verification.
	computedHash := hex.EncodeToString(hash.Sum(nil))
	expectedHash := t.File.SHA256

	// If sender provided X-File-SHA256 header, reject if it differs from registered value
	if headerHash := r.Header.Get("X-File-SHA256"); headerHash != "" && headerHash != expectedHash {
		writeJSONError(w, http.StatusBadRequest, "HASH_HEADER_MISMATCH",
			"X-File-SHA256 header does not match accepted file hash")
		m.failTransfer(t, "HASH_HEADER_MISMATCH",
			fmt.Sprintf("header %s != registered %s", headerHash, expectedHash))
		return
	}

	if expectedHash != "" && computedHash != expectedHash {
		// 409 Conflict: integrity issue, not a server error
		writeJSONError(w, http.StatusConflict, "INTEGRITY_MISMATCH",
			fmt.Sprintf("SHA-256 mismatch: expected %s, got %s", expectedHash, computedHash))
		m.failTransfer(t, "INTEGRITY_MISMATCH",
			fmt.Sprintf("expected %s, got %s", expectedHash, computedHash))
		return
	}

	// Rename .partial -> final path
	if err := os.Rename(partialPath, t.SavePath); err != nil {
		writeJSONError(w, http.StatusInternalServerError, "RENAME_ERROR", "failed to finalize file")
		m.failTransfer(t, "RENAME_ERROR", err.Error())
		return
	}

	// Clean up manifest
	os.Remove(t.SavePath + ".partial.meta")

	// Mark complete
	t.SetState(StateCompleted) // atomic
	t.mu.Lock()
	t.BytesTransferred = written + offset
	t.mu.Unlock()

	m.sendEvent("file:complete", map[string]interface{}{
		"transferId": string(t.ID),
		"path":       t.SavePath,
		"sha256":     computedHash,
		"size":       written + offset,
		"duration":   time.Since(t.StartedAt).Milliseconds(),
		"direction":  "receive",
	})

	w.WriteHeader(http.StatusOK)

	log.Printf("[FileTransfer] Receive complete: %s -> %s (%d bytes in %s)",
		t.ID, t.SavePath, written+offset, time.Since(t.StartedAt))
}

// handleHEAD returns the current offset for resume.
func (m *TransferManager) handleHEAD(w http.ResponseWriter, r *http.Request) {
	transferId := TransferID(r.PathValue("transferId"))
	token := r.Header.Get("X-Transfer-Token")

	t, err := m.getTransfer(transferId, token)
	if err != nil {
		if strings.Contains(err.Error(), "not found") {
			writeJSONError(w, http.StatusNotFound, "TRANSFER_NOT_FOUND", "transfer not found")
		} else {
			writeJSONError(w, http.StatusForbidden, "FORBIDDEN", "invalid or missing transfer token")
		}
		return
	}

	// A valid HEAD proves the sender is alive and attempting to connect.
	// Extend the registration TTL so the sweep loop doesn't delete it prematurely
	// (e.g., sender on slow DERP relay, wake-from-sleep, etc.).
	t.mu.Lock()
	t.LastProgressAt = time.Now()
	t.mu.Unlock()

	// Upload-Offset is derived from the .partial file size on disk (the authoritative
	// source of how many bytes have been received). The .partial.meta file is advisory
	// metadata for crash recovery — it is NOT used to determine the resume offset.
	offset := currentOffset(t.SavePath)
	w.Header().Set("Upload-Offset", strconv.FormatInt(offset, 10))
	w.WriteHeader(http.StatusOK)
}

// hashPartialFile hashes the first `length` bytes of a file into the provided hash.
func hashPartialFile(path string, length int64, h io.Writer) error {
	f, err := os.Open(path)
	if err != nil {
		return err
	}
	defer f.Close()

	_, err = io.CopyN(h, f, length)
	return err
}

// currentOffset returns the size of the .partial file for a transfer, or 0 if no partial exists.
// This is the authoritative resume offset — .partial.meta is advisory (for crash recovery metadata)
// but the actual byte count on disk is what matters.
func currentOffset(savePath string) int64 {
	info, err := os.Stat(savePath + ".partial")
	if err != nil {
		return 0
	}
	return info.Size()
}

// checkDiskSpace checks that the filesystem at dir has enough free space for fileSize bytes.
// Uses syscall.Statfs (unix) to query available blocks. Not atomic — another process could
// fill the disk between check and write — but catches the 90% case early.
func checkDiskSpace(dir string, fileSize int64) error {
	var stat syscall.Statfs_t
	if err := syscall.Statfs(dir, &stat); err != nil {
		// If we can't stat, don't block the transfer — let it fail naturally on write
		return nil
	}
	available := int64(stat.Bavail) * int64(stat.Bsize)
	if available < fileSize {
		return fmt.Errorf("insufficient disk space: need %d bytes, have %d available", fileSize, available)
	}
	return nil
}
```

### 7.6 progress.go

```go
package filetransfer

import (
	"io"
	"sync"
	"time"
)

// progressReader wraps an io.Reader and calls a callback with progress updates,
// rate-limited by either bytes or time interval.
type progressReader struct {
	reader   io.Reader
	total    int64
	offset   int64 // Starting offset (for resume)
	read     int64
	callback func(bytesRead int64)
	config   Config

	lastCallbackAt    time.Time
	lastCallbackBytes int64
	mu                sync.Mutex
}

func newProgressReader(
	r io.Reader,
	total int64,
	offset int64,
	callback func(bytesRead int64),
	config Config,
) *progressReader {
	return &progressReader{
		reader:   r,
		total:    total,
		offset:   offset,
		callback: callback,
		config:   config,
	}
}

func (pr *progressReader) Read(p []byte) (int, error) {
	n, err := pr.reader.Read(p)
	if n > 0 {
		pr.mu.Lock()
		pr.read += int64(n)
		totalRead := pr.offset + pr.read
		now := time.Now()

		shouldReport := false
		if now.Sub(pr.lastCallbackAt) >= pr.config.ProgressInterval {
			shouldReport = true
		} else if totalRead-pr.lastCallbackBytes >= pr.config.ProgressBytes {
			shouldReport = true
		}

		if shouldReport {
			pr.lastCallbackAt = now
			pr.lastCallbackBytes = totalRead
			pr.mu.Unlock()
			pr.callback(totalRead)
		} else {
			pr.mu.Unlock()
		}
	}
	return n, err
}

// emitProgress sends a progress IPC event for a transfer.
func (m *TransferManager) emitProgress(t *Transfer, bytesTransferred int64) {
	t.mu.Lock()
	t.BytesTransferred = bytesTransferred
	t.LastProgressAt = time.Now()

	elapsed := time.Since(t.StartedAt).Seconds()
	var speed float64
	if elapsed > 0 {
		speed = float64(bytesTransferred) / elapsed
	}

	var percent float64
	if t.File.Size > 0 {
		percent = float64(bytesTransferred) / float64(t.File.Size) * 100
	}

	var eta float64
	if speed > 0 {
		eta = float64(t.File.Size-bytesTransferred) / speed
	}
	t.mu.Unlock()

	m.sendEvent("file:progress", map[string]interface{}{
		"transferId":       string(t.ID),
		"bytesTransferred": bytesTransferred,
		"totalBytes":       t.File.Size,
		"percent":          percent,
		"bytesPerSecond":   speed,
		"eta":              eta,
		"direction":        t.Direction,
	})
}
```

### 7.7 resume.go

```go
package filetransfer

import (
	"encoding/json"
	"os"
)

// PartialMeta stores metadata about an in-progress transfer for resume.
type PartialMeta struct {
	TransferID string   `json:"transferId"`
	File       FileInfo `json:"file"`
	SavePath   string   `json:"savePath"`
	Offset     int64    `json:"offset"`
}

// writePartialMeta writes a .partial.meta JSON file alongside the partial file.
func writePartialMeta(t *Transfer, offset int64) error {
	meta := PartialMeta{
		TransferID: string(t.ID),
		File:       t.File,
		SavePath:   t.SavePath,
		Offset:     offset,
	}

	data, err := json.MarshalIndent(meta, "", "  ")
	if err != nil {
		return err
	}

	return os.WriteFile(t.SavePath+".partial.meta", data, 0644)
}

// readPartialMeta reads a .partial.meta file, if it exists.
func readPartialMeta(savePath string) (*PartialMeta, error) {
	data, err := os.ReadFile(savePath + ".partial.meta")
	if err != nil {
		return nil, err
	}

	var meta PartialMeta
	if err := json.Unmarshal(data, &meta); err != nil {
		return nil, err
	}
	return &meta, nil
}
```

### 7.8 Integration into main.go

The `TransferManager` integrates into the App lifecycle following the same pattern as `Server` and `Dialer`:

```go
// In App struct (main.go):
type App struct {
	// ... existing fields ...
	transferManager *filetransfer.TransferManager
}

// In setupHandlers() (main.go):
func (a *App) setupHandlers() {
	// ... existing handlers ...

	// File transfer
	a.transferManager = filetransfer.NewTransferManager(
		a.protocol,
		a.node.Dial,   // tsnet.Dial function
		filetransfer.DefaultConfig(),
	)
	a.transferManager.RegisterHandlers()
}

// In handleStart(), after node is running and server is listening:
func (a *App) handleStart(data json.RawMessage) {
	// ... existing startup ...

	// Start file transfer listener on :9417 (plain TCP, WireGuard encrypts)
	ftLn, err := a.node.Listen("tcp", ":9417")
	if err != nil {
		log.Printf("Failed to listen on :9417 for file transfer: %v", err)
		// Non-fatal: file transfer won't work but everything else will
	} else {
		if err := a.transferManager.Start(ftLn); err != nil {
			log.Printf("Failed to start file transfer manager: %v", err)
		}
	}
}

// In shutdown():
func (a *App) shutdown() {
	// ... existing shutdown ...

	// Stop file transfer manager (cancels active transfers, shuts down HTTP server)
	if a.transferManager != nil {
		if err := a.transferManager.Stop(); err != nil {
			log.Printf("Error stopping file transfer manager: %v", err)
		}
	}
}
```

### 7.9 IPC Protocol Additions

New command and event constants for `protocol.go`:

```go
// Commands (Node.js -> Go)
const (
	// ... existing commands ...
	CmdFilePrepare = "file:prepare"
	CmdFileSend    = "file:send"
	CmdFileAccept  = "file:accept"
	CmdFileReject  = "file:reject"
	CmdFileCancel  = "file:cancel"
	CmdFileList    = "file:list"
)

// Events (Go -> Node.js)
const (
	// ... existing events ...
	EvtFilePreparingProgress = "file:preparing_progress"
	EvtFilePrepared          = "file:prepared"
	EvtFileProgress          = "file:progress"
	EvtFileComplete          = "file:complete"
	EvtFileError             = "file:error"
	EvtFileCancelled         = "file:cancelled"
	EvtFileList              = "file:list"
)
```

---

## 8. TypeScript Implementation

### 8.1 SidecarClient Additions (`packages/sidecar-client/src/index.ts`)

Following the existing pattern of `sendCommand()` methods and `routeEvent()` cases:

```typescript
// New methods on SidecarClient

filePrepare(transferId: string, filePath: string): void {
  this.sendCommand('file:prepare', { transferId, filePath });
}

fileSend(transferId: string, filePath: string, targetAddr: string, token: string, fileInfo: FileTransferFileInfo): void {
  this.sendCommand('file:send', { transferId, filePath, targetAddr, token, fileInfo });
}

fileAccept(transferId: string, savePath: string, token: string, fileInfo: FileTransferFileInfo): void {
  this.sendCommand('file:accept', { transferId, savePath, token, fileInfo });
}

fileReject(transferId: string): void {
  this.sendCommand('file:reject', { transferId });
}

fileCancel(transferId: string): void {
  this.sendCommand('file:cancel', { transferId });
}

fileList(): void {
  this.sendCommand('file:list', {});
}

// New cases in routeEvent()
case 'file:preparing_progress': {
  const d = event.data as FileTransferPreparingProgressEvent;
  this.emit('filePreparingProgress', d);
  break;
}
case 'file:prepared': {
  const d = event.data as FileTransferPreparedEvent;
  this.emit('filePrepared', d);
  break;
}
case 'file:progress': {
  const d = event.data as FileTransferProgressEvent;
  this.emit('fileProgress', d);
  break;
}
case 'file:complete': {
  const d = event.data as FileTransferCompleteEvent;
  this.emit('fileComplete', d);
  break;
}
case 'file:error': {
  const d = event.data as FileTransferErrorEvent;
  this.emit('fileError', d);
  break;
}
case 'file:cancelled': {
  const d = event.data as FileTransferCancelledEvent;
  this.emit('fileCancelled', d);
  break;
}
```

### 8.2 Shared Types (`packages/types/src/index.ts`)

```typescript
// File transfer IPC event types
export interface FileTransferFileInfo {
  name: string;
  size: number;
  sha256: string;
}

export interface FileTransferPreparingProgressEvent {
  transferId: string;
  bytesHashed: number;
  totalBytes: number;
  percent: number;
}

export interface FileTransferPreparedEvent {
  transferId: string;
  name: string;
  size: number;
  sha256: string;
}

export interface FileTransferProgressEvent {
  transferId: string;
  bytesTransferred: number;
  totalBytes: number;
  percent: number;
  bytesPerSecond: number;
  eta: number;
  direction: 'send' | 'receive';
}

export interface FileTransferCompleteEvent {
  transferId: string;
  path?: string;       // Receiver only
  sha256: string;
  size: number;
  duration: number;    // Milliseconds
  direction: 'send' | 'receive';
}

export interface FileTransferErrorEvent {
  transferId: string;
  code: string;
  message: string;
  resumable: boolean;
}

export interface FileTransferCancelledEvent {
  transferId: string;
}

// File transfer signaling types (sent via MessageBus)
export type FileTransferState = 'offering' | 'accepted' | 'transferring' | 'completed' | 'failed' | 'cancelled';
export type FileTransferDirection = 'send' | 'receive';

export interface FileTransferOffer {
  transferId: string;
  senderDeviceId: string;
  senderAddr: string;        // e.g., "hostname.tailnet.ts.net:9417"
  file: FileTransferFileInfo;
  token: string;             // 32-byte hex token for auth
}

export interface FileTransferAccept {
  transferId: string;
  receiverDeviceId: string;
  receiverAddr: string;      // e.g., "hostname.tailnet.ts.net:9417"
  token: string;
}

export interface FileTransferReject {
  transferId: string;
  reason: string;
}

export interface FileTransferCancel {
  transferId: string;
  cancelledBy: string;
  reason: string;
}

export interface FileTransferInfo {
  transferId: string;
  direction: FileTransferDirection;
  state: FileTransferState;
  peerDeviceId: string;
  file: FileTransferFileInfo;
  bytesTransferred: number;
  percent: number;
  bytesPerSecond: number;
  eta: number;
  startedAt: number;
}

export const FILE_TRANSFER_NAMESPACE = 'file-transfer';

export const FILE_TRANSFER_MESSAGE_TYPES = {
  OFFER:  'ft:offer',
  ACCEPT: 'ft:accept',
  REJECT: 'ft:reject',
  CANCEL: 'ft:cancel',
} as const;
```

### 8.3 FileTransferAdapter (`packages/mesh/src/file-transfer-adapter.ts`)

A thin adapter (~100-150 lines) that coordinates MessageBus signaling and IPC commands. It does **not** touch file bytes -- Go handles all data transfer.

```typescript
import { EventEmitter } from 'events';
import { randomBytes } from 'crypto';
import type { IMessageBus, BusMessage } from '@vibecook/truffle-protocol';
import type { ISidecarClient } from '@vibecook/truffle-sidecar-client';
import type {
  FileTransferFileInfo,
  FileTransferOffer,
  FileTransferAccept,
  FileTransferReject,
  FileTransferCancel,
  FileTransferInfo,
  FileTransferProgressEvent,
  FileTransferCompleteEvent,
  FileTransferErrorEvent,
  Logger,
} from '@vibecook/truffle-types';
import {
  FILE_TRANSFER_NAMESPACE,
  FILE_TRANSFER_MESSAGE_TYPES,
} from '@vibecook/truffle-types';

export interface FileTransferAdapterConfig {
  localDeviceId: string;
  localAddr: string;            // e.g., "hostname.tailnet.ts.net:9417"
  messageBus: IMessageBus;
  sidecar: ISidecarClient;
  outputDir: string;            // Default save directory
  logger?: Logger;
}

export interface FileTransferEvents {
  preparingProgress: (info: FileTransferPreparingProgressEvent) => void;
  offer: (offer: FileTransferOffer) => void;
  progress: (info: FileTransferProgressEvent) => void;
  completed: (info: FileTransferCompleteEvent) => void;
  failed: (transferId: string, error: FileTransferErrorEvent) => void;
  cancelled: (transferId: string) => void;
}

export class FileTransferAdapter extends EventEmitter {
  private config: FileTransferAdapterConfig;
  private log: Logger;
  private unsubscribe?: () => void;

  // Track active transfers for state management and UI queries.
  private transfers = new Map<string, FileTransferInfo>();

  // Track original file paths for sender-side transfers (needed when ACCEPT arrives).
  // FileTransferInfo.file.name is the display name, not the disk path.
  private filePaths = new Map<string, string>();

  // Bind handler references once so .off() can remove the exact same function.
  // Every .bind(this) creates a new reference — calling .off() with a second .bind()
  // would NOT remove the original listener, causing memory leaks and duplicate handlers.
  private onPreparingProgress = this.handlePreparingProgress.bind(this);
  private onProgress = this.handleProgress.bind(this);
  private onComplete = this.handleComplete.bind(this);
  private onError = this.handleError.bind(this);
  private onCancelled = this.handleCancelled.bind(this);

  constructor(config: FileTransferAdapterConfig) {
    super();
    this.config = config;
    this.log = config.logger ?? console;
  }

  start(): void {
    // Subscribe to file transfer signaling on MessageBus
    this.unsubscribe = this.config.messageBus.subscribe(
      FILE_TRANSFER_NAMESPACE,
      this.handleMessage.bind(this),
    );

    // Listen for IPC events from Go sidecar (using pre-bound references)
    this.config.sidecar.on('filePreparingProgress', this.onPreparingProgress);
    this.config.sidecar.on('fileProgress', this.onProgress);
    this.config.sidecar.on('fileComplete', this.onComplete);
    this.config.sidecar.on('fileError', this.onError);
    this.config.sidecar.on('fileCancelled', this.onCancelled);
  }

  stop(): void {
    this.unsubscribe?.();
    this.config.sidecar.off('filePreparingProgress', this.onPreparingProgress);
    this.config.sidecar.off('fileProgress', this.onProgress);
    this.config.sidecar.off('fileComplete', this.onComplete);
    this.config.sidecar.off('fileError', this.onError);
    this.config.sidecar.off('fileCancelled', this.onCancelled);
  }

  // --- Public API ---

  async sendFile(targetDeviceId: string, filePath: string): Promise<string> {
    const transferId = `ft-${randomBytes(8).toString('hex')}`; // 8 bytes = 16 hex chars (64 bits)
    const token = randomBytes(32).toString('hex'); // 32 bytes = 64 hex chars

    // 1. Ask Go to prepare (stat + hash)
    this.config.sidecar.filePrepare(transferId, filePath);

    // 2. Wait for file:prepared event (SidecarClient emits 'filePrepared')
    const prepared = await this.waitForEvent('filePrepared', transferId);
    const fileInfo: FileTransferFileInfo = {
      name: prepared.name,
      size: prepared.size,
      sha256: prepared.sha256,
    };

    // 3. Track this transfer and store the original file path
    this.filePaths.set(transferId, filePath);
    this.transfers.set(transferId, {
      transferId,
      direction: 'send',
      state: 'offering',
      peerDeviceId: targetDeviceId,
      file: fileInfo,
      bytesTransferred: 0,
      percent: 0,
      bytesPerSecond: 0,
      eta: 0,
      startedAt: Date.now(),
    });

    // 4. Send OFFER via MessageBus
    const offer: FileTransferOffer = {
      transferId,
      senderDeviceId: this.config.localDeviceId,
      senderAddr: this.config.localAddr,
      file: fileInfo,
      token,
    };
    this.config.messageBus.publish(
      targetDeviceId,
      FILE_TRANSFER_NAMESPACE,
      FILE_TRANSFER_MESSAGE_TYPES.OFFER,
      offer,
    );

    this.log.info(`[FileTransfer] Sent offer ${transferId} to ${targetDeviceId}`);
    return transferId;
  }

  acceptTransfer(offer: FileTransferOffer, savePath?: string): void {
    const finalPath = savePath ?? `${this.config.outputDir}/${offer.file.name}`;

    // Track this transfer on receiver side
    this.transfers.set(offer.transferId, {
      transferId: offer.transferId,
      direction: 'receive',
      state: 'accepted',
      peerDeviceId: offer.senderDeviceId,
      file: offer.file,
      bytesTransferred: 0,
      percent: 0,
      bytesPerSecond: 0,
      eta: 0,
      startedAt: Date.now(),
    });

    // 1. Register in Go BEFORE signaling accept (critical ordering)
    this.config.sidecar.fileAccept(
      offer.transferId,
      finalPath,
      offer.token,
      offer.file,
    );

    // 2. Signal acceptance via MessageBus
    const accept: FileTransferAccept = {
      transferId: offer.transferId,
      receiverDeviceId: this.config.localDeviceId,
      receiverAddr: this.config.localAddr,
      token: offer.token,
    };
    this.config.messageBus.publish(
      offer.senderDeviceId,
      FILE_TRANSFER_NAMESPACE,
      FILE_TRANSFER_MESSAGE_TYPES.ACCEPT,
      accept,
    );
  }

  rejectTransfer(offer: FileTransferOffer, reason = 'rejected'): void {
    this.config.sidecar.fileReject(offer.transferId);

    const reject: FileTransferReject = {
      transferId: offer.transferId,
      reason,
    };
    this.config.messageBus.publish(
      offer.senderDeviceId,
      FILE_TRANSFER_NAMESPACE,
      FILE_TRANSFER_MESSAGE_TYPES.REJECT,
      reject,
    );
  }

  cancelTransfer(transferId: string): void {
    // 1. Cancel locally in Go sidecar (stops active data transfer)
    this.config.sidecar.fileCancel(transferId);

    // 2. Notify the remote peer via MessageBus so they can dismiss the offer/clean up.
    // Without this, cancelling before the sender dials would leave the remote UI
    // showing a stale offer forever (the receiver only learns via socket close
    // if a data connection was established).
    const transfer = this.transfers.get(transferId);
    if (transfer) {
      const cancel: FileTransferCancel = {
        transferId,
        cancelledBy: this.config.localDeviceId,
        reason: 'cancelled by user',
      };
      this.config.messageBus.publish(
        transfer.peerDeviceId,
        FILE_TRANSFER_NAMESPACE,
        FILE_TRANSFER_MESSAGE_TYPES.CANCEL,
        cancel,
      );
    }
  }

  getTransfers(): FileTransferInfo[] {
    return Array.from(this.transfers.values());
  }

  // --- MessageBus Handlers ---

  private handleMessage(msg: BusMessage): void {
    switch (msg.type) {
      case FILE_TRANSFER_MESSAGE_TYPES.OFFER:
        this.emit('offer', msg.payload as FileTransferOffer);
        break;

      case FILE_TRANSFER_MESSAGE_TYPES.ACCEPT: {
        const accept = msg.payload as FileTransferAccept;
        // Sender receives accept -> tell Go to start sending
        const originalPath = this.filePaths.get(accept.transferId);
        const transfer = this.transfers.get(accept.transferId);
        if (transfer && originalPath) {
          transfer.state = 'transferring';
          this.config.sidecar.fileSend(
            accept.transferId,
            originalPath,          // The actual disk path, NOT file.name
            accept.receiverAddr,
            accept.token,
            transfer.file,
          );
        }
        break;
      }

      case FILE_TRANSFER_MESSAGE_TYPES.REJECT: {
        const reject = msg.payload as FileTransferReject;
        this.transfers.delete(reject.transferId);
        this.filePaths.delete(reject.transferId);
        this.emit('failed', reject.transferId, {
          transferId: reject.transferId,
          code: 'REJECTED',
          message: reject.reason,
          resumable: false,
        });
        break;
      }

      case FILE_TRANSFER_MESSAGE_TYPES.CANCEL: {
        const cancel = msg.payload as FileTransferCancel;
        this.transfers.delete(cancel.transferId);
        this.filePaths.delete(cancel.transferId);
        this.emit('cancelled', cancel.transferId);
        break;
      }
    }
  }

  // --- IPC Event Handlers ---

  private handlePreparingProgress(event: FileTransferPreparingProgressEvent): void {
    this.emit('preparingProgress', event);
  }

  private handleProgress(event: FileTransferProgressEvent): void {
    const transfer = this.transfers.get(event.transferId);
    if (transfer) {
      transfer.bytesTransferred = event.bytesTransferred;
      transfer.percent = event.percent;
      transfer.bytesPerSecond = event.bytesPerSecond;
      transfer.eta = event.eta;
      transfer.state = 'transferring';
    }
    this.emit('progress', event);
  }

  private handleComplete(event: FileTransferCompleteEvent): void {
    const transfer = this.transfers.get(event.transferId);
    if (transfer) {
      transfer.state = 'completed';
    }
    this.emit('completed', event);
    // Clean up after a short delay so UI can show completion state
    setTimeout(() => {
      this.transfers.delete(event.transferId);
      this.filePaths.delete(event.transferId);
    }, 5000);
  }

  private handleError(event: FileTransferErrorEvent): void {
    const transfer = this.transfers.get(event.transferId);
    if (transfer) {
      transfer.state = 'failed';
    }
    this.emit('failed', event.transferId, event);
    if (!event.resumable) {
      this.transfers.delete(event.transferId);
      this.filePaths.delete(event.transferId);
    }
  }

  private handleCancelled(event: { transferId: string }): void {
    this.transfers.delete(event.transferId);
    this.filePaths.delete(event.transferId);
    this.emit('cancelled', event.transferId);
  }

  private waitForEvent(eventName: string, transferId: string): Promise<any> {
    return new Promise((resolve, reject) => {
      const cleanup = () => {
        clearTimeout(timeout);
        this.config.sidecar.off(eventName, handler);
        this.config.sidecar.off('fileError', errorHandler);
      };

      const timeout = setTimeout(() => {
        cleanup();
        // Clean up the transfer on timeout so the UI doesn't show a stuck offer
        this.transfers.delete(transferId);
        this.filePaths.delete(transferId);
        this.emit('failed', transferId, {
          transferId,
          code: 'PREPARE_TIMEOUT',
          message: `Timeout waiting for ${eventName}`,
          resumable: false,
        });
        reject(new Error(`Timeout waiting for ${eventName} on ${transferId}`));
      }, 60_000);

      const handler = (data: any) => {
        if (data.transferId === transferId) {
          cleanup();
          resolve(data);
        }
      };

      // Also listen for errors during the wait (e.g., file not found, hash error)
      const errorHandler = (data: any) => {
        if (data.transferId === transferId) {
          cleanup();
          this.transfers.delete(transferId);
          this.filePaths.delete(transferId);
          reject(new Error(`${data.code}: ${data.message}`));
        }
      };

      this.config.sidecar.on(eventName, handler);
      this.config.sidecar.on('fileError', errorHandler);
    });
  }
}
```

---

## 9. Resume Mechanism

### 9.1 Partial File Storage

During transfer, bytes are written to `savePath.partial`:
```
/Users/alice/Downloads/photo.jpg.partial      -- raw file bytes (growing)
/Users/alice/Downloads/photo.jpg.partial.meta  -- JSON manifest
```

### 9.2 Manifest Format

```json
{
  "transferId": "ft-a1b2c3d4e5f67890",
  "file": {
    "name": "photo.jpg",
    "size": 104857600,
    "sha256": "e3b0c44..."
  },
  "savePath": "/Users/alice/Downloads/photo.jpg",
  "offset": 52428800
}
```

### 9.3 Resume Flow

1. **Sender queries offset**: `HEAD /transfer/{id}` returns `Upload-Offset: 52428800`
2. **Receiver re-hashes partial**: Streams the existing `.partial` file through SHA-256 (~500MB/s, <1s for 500MB) to rebuild the hash state
3. **Sender seeks and sends**: `Content-Range: bytes 52428800-104857599/104857600`
4. **Receiver appends**: Writes incoming bytes after the partial, continues SHA-256 computation
5. **Verification**: Final SHA-256 covers the entire file (partial + remainder)

### 9.4 Cleanup

- **On complete**: Both `.partial` and `.partial.meta` are deleted
- **On cancel**: `.partial` and `.partial.meta` are deleted. Cancel is an explicit user action meaning "discard this transfer."
- **On error** (network failure, disk error, etc.): `.partial` and `.partial.meta` are preserved for possible resume. The `resumable` flag in `file:error` tells the UI whether retry is possible.
- **On startup**: Scan output directory for orphaned `.partial.meta` files without matching active transfers; delete stale partials older than a configurable TTL

---

## 10. Security

| Concern | Mitigation |
|---|---|
| **Transport encryption** | WireGuard encrypts all Tailscale traffic. Port :9417 is plain TCP, but WireGuard provides equivalent-to-TLS encryption automatically. |
| **Authentication** | 32 random bytes (64 hex chars) transfer token exchanged via encrypted MessageBus channel. HTTP server validates token value and format (64-char hex) on every PUT/HEAD request (403 without). |
| **Authorization** | Port :9417 only accessible within Tailscale network (`tsnet.Listen`). Not exposed to the public internet. |
| **Path traversal** | `validateSavePath()` rejects non-absolute paths and paths that contain `..` traversal components after cleaning. Returns the cleaned path for downstream use. |
| **File size limits** | Configurable `MaxFileSize` (default 4GB). HTTP server returns 413 if exceeded. |
| **Integrity** | SHA-256 computed during streaming (no extra pass). Verified on completion. Mismatch fails the transfer. |
| **Resource exhaustion** | Each transfer is a single goroutine + 32KB buffer. `MaxConcurrentRecv` (default: 5) limits incoming transfers. `RegisteredTimeoutTTL` (default: 2min) sweeps stale registrations. `TransferManager` tracks all active transfers for cleanup on shutdown. |
| **Token confidentiality** | Transfer tokens are exchanged via MessageBus, which runs over WebSocket over Tailscale (WireGuard-encrypted). Token confidentiality depends on MessageBus messages not being readable by other nodes. In STAR topology, the primary relays messages and could theoretically read tokens. This is acceptable: the primary is a trusted device in the user's tailnet. |

---

## 11. Expected Throughput

| Scenario | RFC 001 (Node.js data plane) | RFC 002 (Go native data plane) |
|---|---|---|
| **LAN (gigabit)** | ~5-15 MB/s | ~40-60 MB/s typical (WireGuard limited) |
| **WiFi** | ~5-10 MB/s | ~20-50 MB/s |
| **DERP relay** | ~3-5 MB/s | ~10-12 MB/s |
| **CPU usage** | High (base64 + JSON + V8 GC) | Low (io.Copy + SHA-256, hardware-accelerated) |
| **Memory** | Full chunks in Node.js heap | 32KB buffer in Go |
| **Data copies** | File -> Node -> Base64 -> JSON -> stdin -> Go -> TCP | File -> TCP (near zero-copy via io.Copy) |
| **Latency** | Per-chunk ACK round-trip (sliding window of 4) | Streaming, no ACK overhead |
| **Resume** | Chunk-level (512KB granularity) | Byte-level (resume from exact offset) |

LAN throughput is limited by WireGuard tunnel overhead (~300-500 Mbps on modern hardware, ~40-60 MB/s typical). These are realistic figures; the previous "100-800 MB/s" estimate was overly optimistic. The bottleneck shifts from Node.js to WireGuard, which is a much higher ceiling.

---

## 12. Implementation Plan

### Files to Create

**Go:**
- `packages/sidecar/internal/filetransfer/types.go`
- `packages/sidecar/internal/filetransfer/manager.go`
- `packages/sidecar/internal/filetransfer/sender.go`
- `packages/sidecar/internal/filetransfer/receiver.go`
- `packages/sidecar/internal/filetransfer/progress.go`
- `packages/sidecar/internal/filetransfer/resume.go`

**TypeScript:**
- `packages/mesh/src/file-transfer-adapter.ts`

### Files to Modify

**Go:**
- `packages/sidecar/main.go` -- add `transferManager` field to `App`, integrate lifecycle
- `packages/sidecar/internal/ipc/protocol.go` -- add `file:*` command/event constants

**TypeScript:**
- `packages/sidecar-client/src/index.ts` -- add `filePrepare()`, `fileSend()`, `fileAccept()`, `fileReject()`, `fileCancel()`, `fileList()` methods; add event routing for `file:*` events
- `packages/types/src/index.ts` -- add file transfer types and constants
- `packages/mesh/src/mesh-node.ts` -- expose `FileTransferAdapter` integration point

### Reference Patterns

- `packages/sidecar/internal/server/reverseproxy.go` -- how a service integrates with IPC + tsnet listeners (ProxyManager pattern)
- `packages/sidecar/internal/server/dialer.go` -- how outgoing connections use `tsnet.Dial()` (DialConnection pattern)
- `packages/store-sync/src/index.ts` -- the canonical adapter pattern (DI, lifecycle, MessageBus)
- `packages/mesh/src/mesh-message-bus.ts` -- how MessageBus routes messages by namespace

### Implementation Order

1. Go: `types.go` (data structures)
2. Go: `progress.go` (shared utility)
3. Go: `resume.go` (shared utility)
4. Go: `receiver.go` (HTTP server)
5. Go: `sender.go` (HTTP client)
6. Go: `manager.go` (orchestration)
7. Go: `protocol.go` changes (constants)
8. Go: `main.go` changes (integration)
9. TypeScript: `types/src/index.ts` additions
10. TypeScript: `sidecar-client/src/index.ts` additions
11. TypeScript: `mesh/src/file-transfer-adapter.ts`

---

## 13. Testing Strategy

### Go Tests

- **Unit tests** for each file (`types_test.go`, `sender_test.go`, `receiver_test.go`, `progress_test.go`, `resume_test.go`)
- **Integration test** (`manager_test.go`): spin up two `TransferManager` instances with `net.Pipe()`, transfer a file end-to-end, verify SHA-256
- **Resume test**: transfer half a file, stop, resume, verify complete file
- **Concurrency test**: multiple simultaneous transfers
- **Error cases**: invalid token (403), missing transfer (404), cancelled mid-stream

### TypeScript Tests

- **FileTransferAdapter**: mock `IMessageBus` + mock `ISidecarClient`, verify signaling flow
- **Integration**: two adapters connected via shared mock bus, verify offer -> accept -> complete lifecycle

---

## 14. Protocol Edge Cases

Behaviors that must be specified to avoid ambiguous implementation:

### 14.1 Duplicate PUT

Two scenarios:

1. **Retry while handler is active**: If the sender retries while a previous PUT handler is still running, the `inFlight` CAS fails and the receiver returns **409 Conflict** with code `TRANSFER_IN_FLIGHT`.

2. **Retry after connection drop**: If the previous handler exited (connection dropped, error), `inFlight` is cleared by `defer`. The transfer remains in `StateTransferring` with partial data on disk. The retry PUT passes the `inFlight` CAS and proceeds as a resume. The sender should query HEAD for the current offset and send with `Content-Range`.

### 14.2 Crash Recovery

On startup, `TransferManager` does **not** automatically resume transfers from `.partial.meta` files. Partial files are preserved on disk, but resuming requires the Node.js control plane to re-offer and re-accept (re-exchanging a fresh token). The `.partial.meta` file allows the receiver to skip re-downloading already-received bytes.

A future enhancement could add a `file:recover` IPC command that scans for `.partial.meta` files and re-registers them.

### 14.3 Resume After Mid-Stream Disconnect

If the TCP connection drops during a transfer (network blip, WiFi roaming):

1. **Receiver side:** `io.Copy` returns error. `handlePUT` exits, `defer` clears `inFlight` to 0. State remains `StateTransferring`. Partial file and `.partial.meta` are preserved on disk.
2. **Sender side:** `client.Do(req)` returns error. `failTransfer` is called with `SEND_ERROR` (resumable). The sender can retry.
3. **On retry:** Sender calls `file:send` again (or the UI auto-retries). The new `sendFile` goroutine queries `HEAD` for the current offset, seeks the local file, and sends with `Content-Range`. The receiver's `handlePUT` sees `StateTransferring` + `inFlight == 0`, acquires the in-flight lock, and resumes writing.

This works **within the same process lifetime** — the transfer registration (token, file info) persists in memory. For crash recovery across process restarts, see section 14.2.

### 14.4 X-File-Name Header

The `X-File-Name` header is informational only. The receiver **ignores it for path decisions** — the save path is determined by `fileAccept()` in the control plane, not by the sender's HTTP headers. This prevents a malicious sender from writing to an unexpected path.

### 14.5 Progress Emission Under Load

With multiple concurrent transfers, progress events could flood the IPC channel. Mitigations:
- Rate-limited per transfer (every 256KB or 500ms, whichever comes first)
- Consider adding a per-transfer "stall ticker" that emits progress even when no bytes flow, so the UI can distinguish "slow" from "stuck"

### 14.6 Already-Completed Retry

If the sender retries a PUT after the transfer has already completed (e.g., the sender didn't receive the 200 response due to a network hiccup), `StateCompleted` is a terminal state and the receiver returns **409 Conflict** with code `TRANSFER_NOT_ACCEPTING`. The sender handles this automatically: if the error code is `TRANSFER_NOT_ACCEPTING` and the sender already sent all expected bytes (`offset + ContentLength >= File.Size`), it treats the response as success and emits `file:complete`.

### 14.7 Registered Transfer Timeout

If a receiver calls `file:accept` but the sender never dials (sender crashed, user cancelled on sender side without sending CANCEL), the registered transfer sits idle. The `sweepCompletedTransfers` cleanup loop removes `StateRegistered` transfers that have been idle for longer than `RegisteredTimeoutTTL` (default: 2 minutes).

### 14.8 Go Version Requirement

The `ServeMux` method patterns (`"PUT /transfer/{transferId}"`) require **Go 1.22+**. This must be enforced in `go.mod` or documented as a build requirement.

---

## 15. Known Limitations (v1)

- **Resume hashing is O(offset):** On resume, the receiver re-hashes the entire `.partial` file to rebuild SHA-256 state (~500MB/s on modern hardware). For a 50GB partial, this takes ~100s. Repeated resumes on flaky networks pay this cost each time. Future optimization: store incremental SHA-256 state checkpoints every N MB (Go's `crypto/sha256` implements `encoding.BinaryMarshaler` since Go 1.13, so this is feasible). Alternative: defer SHA-256 to a single post-write verification pass, trading inline integrity for simpler resume.
- **StartedAt resets on resume:** Each resume attempt resets `StartedAt`, so duration/speed metrics reflect "this attempt" not "total elapsed." A future `FirstStartedAt` field could track total wall time.
- **Token trust model:** Transfer tokens are plaintext within MessageBus messages. In STAR topology, the primary relay could read them. Future hardening: derive per-transfer tokens using device public keys, or use Noise-based key agreement. Tokens should also be explicitly single-use (invalidated after terminal state) and rate-limited per source.
- **Symlink path hardening:** `validateSavePath` does not resolve symlinks. A symlink in the parent directory could redirect writes. Acceptable for desktop (path comes from OS save dialog), but future hardening could `EvalSymlinks` the parent and verify it matches an allowed base directory.
- **Orphaned partial cleanup:** On startup, orphaned `.partial.meta` files from crashed processes are not automatically cleaned up. Future: add a `RecoverOrphans()` method that scans for stale `.partial.meta` older than a TTL and surfaces them to the UI or deletes them.
- **Platform constraints for MaxFileSize:** The 4GB default is configurable, but the RFC does not call out filesystem constraints (FAT32 4GB limit, path length limits on Windows). Implementation should document these.

## 16. Future Enhancements

- **Directory transfer**: Tar/stream a directory as a single transfer, extract on receiver
- **Multi-device broadcast**: Send same file to multiple receivers (fan-out from Go)
- **Compression**: Optional gzip/zstd for compressible file types
- **Bandwidth throttling**: Configurable rate limiting in Go
- **Transfer queue**: Queue multiple transfers with priority and bandwidth sharing
- **Web UI**: React hook (`useFileTransfer`) for progress bars and accept/reject UI
- **Semaphore ordering optimization**: Move `inFlight` CAS before semaphore acquire to reject duplicate PUTs without consuming a concurrency slot

---

## 17. Acceptance Criteria

- [ ] Go HTTP server listens on `:9417` via `tsnet.Listen("tcp", ":9417")`
- [ ] Files transfer disk-to-disk without Node.js touching file bytes
- [ ] SHA-256 verified on receiver after complete transfer
- [ ] Resume works: partial transfer -> stop -> resume -> complete with correct hash
- [ ] Progress events emitted via IPC (every 256KB or 500ms)
- [ ] Transfer token validated on every HTTP request
- [ ] Cancel stops both sender and receiver, cleans up partial files
- [ ] `FileTransferAdapter` coordinates signaling via MessageBus
- [ ] TypeScript types exported from `@vibecook/truffle-types`
- [ ] All existing tests continue to pass

---

## Appendix A: Comparison with RFC 001

| Aspect | RFC 001 | RFC 002 |
|---|---|---|
| **Data plane** | Node.js (base64 chunks via WebSocket) | Go (raw bytes via HTTP) |
| **File I/O** | TypeScript `fs.createReadStream` + `Chunker` | Go `os.Open` + `io.Copy` |
| **Encoding** | Base64 (+33% overhead) | None (raw bytes) |
| **Transfer protocol** | Custom chunk/ACK over WebSocket | HTTP PUT (standard) |
| **Resume granularity** | Chunk-level (512KB) | Byte-level |
| **Concurrency model** | Sliding window (4 chunks) | Single HTTP stream per transfer |
| **New packages** | `@vibecook/truffle-file-transfer` (~500 lines TS) | ~400 lines Go + ~150 lines TS adapter |
| **Node.js involvement** | Full data path | Control plane only |
| **Debuggability** | Custom protocol, needs custom tooling | `curl` works |

### Why RFC 001 is Superseded

RFC 001 is a correct design, but it routes file bytes through a path (JSON IPC + WebSocket) that was designed for small control messages. This creates an inherent throughput ceiling. RFC 002 uses the Go sidecar's direct access to Tailscale TCP for file data, achieving near-wire-speed performance with less code and lower complexity. The control plane (offer/accept/reject signaling) remains in TypeScript via the existing MessageBus, keeping the architecture clean.
