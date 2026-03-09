# RFC 001: File Transfer Primitive for Truffle

**Status**: Superseded by [RFC 002](./002-file-transfer-native.md)
**Date**: 2026-02-24
**Package**: `@vibecook/truffle-file-transfer`

### Revision Notes

- **2026-02-24 Review**: All code references verified against codebase. 12 issues fixed, 4 external feedback points evaluated. Key corrections: FrameCodec not on WebSocket path, StoreSyncAdapter pattern alignment, single-side dial for direct connections, lazy SHA-256 computation, persistent FileHandle for chunking. Added Known Limitations & Risks section and External Feedback appendix.

---

## 1. Problem Statement

Truffle provides device discovery, namespace-based message bus, and cross-device state sync -- but no way to transfer files between devices. The existing message bus has no chunking support, and large payloads are impractical: `sendEnvelopeDirect()` (mesh-node.ts:384) uses plain `JSON.stringify()` through stdin/stdout IPC to the Go sidecar, so multi-megabyte JSON strings cause memory pressure, saturate the single-threaded IPC `readLoop()`, and risk hitting the Go sidecar's 256-message channel buffer limits (which drops connections on overflow). Applications built on Truffle (e.g., Cheeseboard - cross-device clipboard + file drop) need a first-class file transfer primitive with good throughput, progress tracking, and resume support.

> **Note**: The `MAX_MESSAGE_SIZE` (16MB) in `packages/protocol/src/frame-codec.ts` applies to the length-prefixed binary frame codec, which is **not used on the WebSocket path**. The WebSocket path is purely JSON strings through sidecar stdin/stdout.

Additionally, the Go sidecar has performance bottlenecks that limit throughput for ALL Truffle features:
- 1KB WebSocket read/write buffers (`packages/sidecar/internal/server/websocket.go:15-16`)
- 256-message send channel buffers (`websocket.go:73`, `dialer.go:23`)
- TextMessage mode forcing UTF-8 validation on every byte (`websocket.go:205`, `dialer.go:261`)

---

## 2. Goals

1. Add a `@vibecook/truffle-file-transfer` package as a first-class Truffle primitive
2. Support files of any size with chunked transfer
3. Resume interrupted transfers without re-sending completed chunks
4. Provide real-time progress events (bytes, percentage, speed, ETA)
5. Transfer data peer-to-peer (bypass the STAR primary relay for bulk data)
6. Follow the established `StoreSyncAdapter` pattern exactly (DI, lifecycle, MessageBus)
7. Fix sidecar performance bottlenecks (benefits all Truffle features)

## 3. Non-Goals

- Browser `File` API support (Node.js file paths and Buffers only for v1)
- Go-native file streaming (future optimization, documented in Section 12)
- Encryption beyond what Tailscale/WireGuard already provides
- Directory transfer (can be built on top by the application)

---

## 4. Background: How Truffle Works

### Architecture Layers
```
Layer 2: Application    → MessageBus (pub/sub), StoreSyncAdapter, [NEW] FileTransferAdapter
Layer 1: Mesh Routing   → MeshNode, DeviceManager, PrimaryElection
Layer 0: Transport      → WebSocketTransport over Tailscale
         IPC            → SidecarClient (JSON over stdin/stdout)
         Network        → Go sidecar (embeds Tailscale tsnet, WireGuard)
```

### Key Mechanism: STAR Topology Routing

`MeshNode.sendEnvelope()` (`packages/mesh/src/mesh-node.ts:317-349`) routes messages:
1. If a direct WebSocket connection to the target exists → sends directly
2. If no direct connection AND sender is not primary → routes through the primary device
3. Primary relays to the target

**Critical insight**: By calling `MeshNode.connectToDevice(targetId)` before starting a file transfer, all subsequent `messageBus.publish(targetId, ...)` calls bypass the primary and go peer-to-peer.

### The StoreSyncAdapter Pattern (to follow)

See `packages/store-sync/src/index.ts` — the canonical pattern for Truffle adapters:
- Constructor injection: `{ localDeviceId, messageBus, stores, logger }` (no MeshNode)
- Lifecycle: `start()` subscribes to MessageBus namespace, `stop()` unsubscribes, `dispose()` cleans up
- Uses `IMessageBus` from `@vibecook/truffle-protocol`
- Subscribes to a dedicated namespace (store-sync uses `'sync'`)
- Exposes `handleDeviceOffline()` / `handleDeviceDiscovered()` methods for the **caller** to wire to MeshNode events (adapter doesn't subscribe to MeshNode directly)

### Current Data Path (and bottlenecks)

```
Node.js → JSON.stringify → SidecarClient.sendCommand → stdin (JSON line)
  → Go IPC readLoop → route to dialer/connMgr
  → websocket.Conn.WriteMessage(TextMessage, []byte)
  → [WireGuard tunnel] → remote ReadMessage()
  → string(message) → JSON IPC event → stdout
  → Node.js readline → JSON.parse
```

Every byte of data traverses this path. The sidecar converts all WebSocket data to strings for JSON IPC.

---

## 5. Sidecar Performance Fixes (Prerequisite)

These are 5 line changes in Go that 3-5x throughput for ALL Truffle features. Must be done first.

### Changes

**`packages/sidecar/internal/server/websocket.go`**:
```go
// Line 15-16: Increase WebSocket buffers from 1KB to 64KB
ReadBufferSize:  65536,   // was 1024
WriteBufferSize: 65536,   // was 1024

// Line 73: Increase send channel from 256 to 1024
Send: make(chan []byte, 1024),   // was 256

// Line 205: Use BinaryMessage instead of TextMessage
err := c.Conn.WriteMessage(websocket.BinaryMessage, message)   // was TextMessage
```

**`packages/sidecar/internal/server/dialer.go`**:
```go
// Line 23: Increase buffer from 256 to 1024
dialBufferSize = 1024   // was 256

// Line 261: Use BinaryMessage instead of TextMessage
if err := c.conn.WriteMessage(websocket.BinaryMessage, message); err != nil {   // was TextMessage
```

### Why this is safe

- `ReadMessage()` handles both TextMessage and BinaryMessage transparently
- The receiving side does `string(message)` regardless of frame type
- Larger buffers only affect memory allocation (64KB per connection instead of 1KB)
- Larger channel prevents `"buffer full"` disconnections under load

### Expected impact

| Before | After |
|---|---|
| ~1-5 MB/s throughput | ~15-40 MB/s throughput (theoretical) |
| Buffer-full disconnections under burst load | Handles sustained transfer |
| UTF-8 validation on every byte (TextMessage) | No validation overhead (BinaryMessage) |

> **Note**: The ~15-40 MB/s figures are theoretical maximums on direct LAN connections. Actual throughput depends on: Tailscale DERP relay latency (if no direct WireGuard tunnel), stdin/stdout JSON parsing overhead in the Go `readLoop()`, base64 encoding adding ~33% to payload size, and the single-threaded IPC bottleneck. Benchmark after implementation to establish real-world numbers.

---

## 6. Package Structure

```
packages/file-transfer/
  package.json
  tsconfig.json
  src/
    index.ts                          # Public API exports
    types.ts                          # All types, interfaces, events
    constants.ts                      # Namespace, message types, defaults
    file-transfer-adapter.ts          # Main adapter class
    transfer-manager.ts               # Transfer state machines + sliding window
    chunker.ts                        # File → base64 chunks with SHA-256
    assembler.ts                      # Chunks → file on disk with verification
    checksum.ts                       # SHA-256 utilities
    __tests__/
      file-transfer-adapter.test.ts
      transfer-manager.test.ts
      chunker.test.ts
      assembler.test.ts
      integration.test.ts
```

### package.json

```json
{
  "name": "@vibecook/truffle-file-transfer",
  "version": "0.1.0",
  "description": "Peer-to-peer file transfer for Truffle mesh networking",
  "type": "module",
  "main": "dist/index.js",
  "types": "dist/index.d.ts",
  "exports": {
    ".": {
      "types": "./dist/index.d.ts",
      "import": "./dist/index.js"
    }
  },
  "license": "MIT",
  "files": ["dist", "!dist/__tests__"],
  "engines": { "node": ">=18" },
  "scripts": {
    "build": "tsc",
    "clean": "rm -rf dist",
    "typecheck": "tsc --noEmit",
    "test": "vitest run"
  },
  "dependencies": {
    "@vibecook/truffle-types": "workspace:*",
    "@vibecook/truffle-protocol": "workspace:*",
    "@vibecook/truffle-mesh": "workspace:*"
  },
  "devDependencies": {
    "typescript": "^5.7.0",
    "vitest": "^3.0.0",
    "@types/node": "^22.0.0"
  }
}
```

### tsconfig.json

```json
{
  "extends": "../../tsconfig.base.json",
  "compilerOptions": {
    "outDir": "dist",
    "rootDir": "src"
  },
  "include": ["src"]
}
```

> **Note**: No `references` array — the project removed all `references` from package tsconfigs during initial setup because they caused build failures with the workspace setup.

### Monorepo wiring

- Add `{ "path": "./packages/file-transfer" }` to root `tsconfig.json` references
- Automatic workspace inclusion via `packages/*` glob in `pnpm-workspace.yaml`

---

## 7. Constants and Types

### constants.ts

```typescript
export const FILE_TRANSFER_NAMESPACE = 'file-transfer';

export const FILE_TRANSFER_MESSAGE_TYPES = {
  OFFER:           'ft:offer',
  ACCEPT:          'ft:accept',
  REJECT:          'ft:reject',
  CHUNK:           'ft:chunk',
  CHUNK_ACK:       'ft:chunk-ack',
  RESUME_REQUEST:  'ft:resume-request',
  RESUME_RESPONSE: 'ft:resume-response',
  CANCEL:          'ft:cancel',
  COMPLETE:        'ft:complete',
  VERIFIED:        'ft:verified',
  ERROR:           'ft:error',
} as const;

export const DEFAULT_CHUNK_SIZE = 512 * 1024;       // 512KB raw → ~683KB base64
export const MAX_CHUNK_SIZE = 4 * 1024 * 1024;      // 4MB raw → ~5.3MB base64, practical upper bound for IPC
export const DEFAULT_WINDOW_SIZE = 4;                // Sliding window: 4 chunks in-flight
export const CHUNK_ACK_TIMEOUT_MS = 30_000;          // 30s per chunk ACK
export const OFFER_TIMEOUT_MS = 60_000;              // 60s for offer to be accepted
export const TRANSFER_STALL_TIMEOUT_MS = 60_000;     // 60s with no progress → cancel
export const PAUSED_TRANSFER_TTL_MS = 24 * 60 * 60 * 1000;  // 24h before cleanup
```

### types.ts (key types)

```typescript
// Transfer identity
export type TransferId = string;
export type TransferDirection = 'send' | 'receive';
export type TransferState =
  | 'offering' | 'accepted' | 'transferring' | 'completing'
  | 'verifying' | 'completed' | 'cancelled' | 'failed' | 'paused';

// File metadata (sent in OFFER)
export interface FileMetadata {
  name: string;
  mimeType?: string;          // Default: 'application/octet-stream' (Node.js has no reliable built-in MIME detection)
  size: number;               // Total bytes
  sha256: string;             // Whole-file SHA-256 (hex), may be '' in OFFER if computed lazily
  totalChunks: number;
  chunkSize: number;          // Bytes per chunk (last may be smaller)
  metadata?: Record<string, unknown>;  // App-defined
}

// Message payloads
export interface OfferPayload {
  transferId: TransferId;
  file: FileMetadata;
  senderDeviceId: string;
}

export interface AcceptPayload {
  transferId: TransferId;
  receiverDeviceId: string;
  receivedChunks?: number[];   // For resume: chunks already verified
}

export interface RejectPayload {
  transferId: TransferId;
  reason: string;
}

export interface ChunkPayload {
  transferId: TransferId;
  index: number;              // Zero-based chunk index
  data: string;               // Base64-encoded chunk bytes
  sha256: string;             // SHA-256 of raw (pre-encoding) bytes
}

export interface ChunkAckPayload {
  transferId: TransferId;
  index: number;
  valid: boolean;             // false = checksum mismatch, resend
}

export interface ResumeRequestPayload {
  transferId: TransferId;
  receivedChunks: number[];   // Verified chunk indices
}

export interface ResumeResponsePayload {
  transferId: TransferId;
  file: FileMetadata;
  resumeFromIndex: number;
}

export interface CancelPayload {
  transferId: TransferId;
  reason: string;
  cancelledBy: string;        // Device ID
}

export interface CompletePayload {
  transferId: TransferId;
  sha256: string;             // Whole-file hash for final verification
}

export interface VerifiedPayload {
  transferId: TransferId;
  valid: boolean;
}

export interface ErrorPayload {
  transferId: TransferId;
  code: string;               // e.g., 'DISK_FULL', 'INTEGRITY_MISMATCH'
  message: string;
}

// Transfer info (for progress tracking / UI)
export interface TransferInfo {
  transferId: TransferId;
  direction: TransferDirection;
  state: TransferState;
  peerDeviceId: string;
  file: FileMetadata;
  chunksCompleted: number;
  bytesTransferred: number;
  bytesPerSecond: number;
  estimatedSecondsRemaining: number;
  progress: number;           // 0-100
  startedAt: number;
  lastActivityAt: number;
  error?: string;
}

// Adapter config (follows StoreSyncAdapterConfig pattern — no MeshNode injection)
export interface FileTransferAdapterConfig {
  localDeviceId: string;
  messageBus: IMessageBus;     // from @vibecook/truffle-protocol
  connectToDevice?: (deviceId: string) => Promise<void>;  // optional, for direct P2P connections
  chunkSize?: number;          // default 512KB
  maxConcurrentChunks?: number; // default 4 (sliding window)
  outputDir?: string;          // where received files are saved
  logger?: Logger;
}

// Events emitted by FileTransferAdapter
export interface FileTransferEvents {
  offer: (offer: OfferPayload) => void;
  accepted: (transferId: TransferId) => void;
  rejected: (transferId: TransferId, reason: string) => void;
  progress: (info: TransferInfo) => void;
  completed: (transferId: TransferId, info: TransferInfo) => void;
  cancelled: (transferId: TransferId, reason: string) => void;
  failed: (transferId: TransferId, error: string) => void;
  stateChanged: (transferId: TransferId, state: TransferState) => void;
}
```

Also add `FILE_TRANSFER_NAMESPACE` and `FILE_TRANSFER_MESSAGE_TYPES` to `packages/types/src/index.ts` following the existing `STORE_SYNC_*` pattern at lines 347-464.

---

## 8. Transfer Protocol

### 8.1 Happy Path

```
Sender                                  Receiver
  |                                       |
  |── OFFER {transferId, file} ─────────→|  (via messageBus.publish)
  |                                       |
  |←── ACCEPT {transferId} ─────────────|  (via messageBus.publish)
  |                                       |
  |  [receiver: connectToDevice(sender)]  |  ← ensures direct P2P
  |                                       |
  |── CHUNK {idx:0, data, sha256} ─────→|  ┐
  |── CHUNK {idx:1, data, sha256} ─────→|  │ sliding window
  |── CHUNK {idx:2, data, sha256} ─────→|  │ (up to 4 in-flight)
  |── CHUNK {idx:3, data, sha256} ─────→|  ┘
  |←── CHUNK_ACK {idx:0, valid:true} ──|  → sender sends CHUNK {idx:4}
  |←── CHUNK_ACK {idx:1, valid:true} ──|  → sender sends CHUNK {idx:5}
  |  ... continues ...                    |
  |── COMPLETE {sha256} ───────────────→|
  |←── VERIFIED {valid:true} ──────────|
  |                                       |
  [Both: state → 'completed']
```

### 8.2 Resume (after disconnect)

When MeshNode emits `deviceDiscovered` for a peer with a paused transfer:

```
Receiver                                  Sender
  |── RESUME_REQUEST {receivedChunks} ──→|
  |←── RESUME_RESPONSE {resumeFromIdx} ─|
  |  [connectToDevice(peer)]              |
  |←── CHUNK {idx: resumeFromIdx} ──────|
  |  ... continues from there ...         |
```

### 8.3 Error Cases

| Scenario | Behavior |
|---|---|
| Checksum mismatch on chunk | ACK with `valid: false`, sender resends that chunk |
| Receiver offline mid-transfer | Sender pauses, resumes when peer rediscovered |
| Final hash mismatch | VERIFIED with `valid: false`, transfer marked failed |
| No ACK within 30s | Sender resends chunk (up to 3 retries, then fail) |
| Offer no response in 60s | Auto-cancel |
| Disk full on receiver | Transfer fails with `DISK_FULL` error |
| Duplicate offer for same file | Reject with `TRANSFER_ALREADY_ACTIVE` |

### 8.4 Sliding Window

The sender maintains a window of `maxConcurrentChunks` (default 4) in-flight chunks:
1. Send chunks 0-3 immediately
2. When ACK for chunk 0 arrives, send chunk 4
3. When ACK for chunk 1 arrives, send chunk 5
4. If NACK (valid: false), resend that chunk within the window
5. If no ACK within timeout, resend

This keeps the WireGuard tunnel saturated and masks ACK round-trip latency.

### 8.5 Why Base64

The current data path is: `JSON.stringify()` in `sendEnvelopeDirect()` (mesh-node.ts:384) → JSON IPC over stdin/stdout → Go sidecar → WebSocket. Binary data cannot survive JSON serialization, so chunks must be base64-encoded. This adds ~33% overhead but is unavoidable with the current architecture.

The chunk size (512KB) accounts for this: 512KB raw → ~683KB base64 → ~700KB with JSON wrapping, well within practical IPC limits.

### 8.6 Lazy SHA-256 Computation

The whole-file SHA-256 in `FileMetadata` should be computed **lazily**, not before the OFFER is sent. Computing a 1GB file hash takes 5-10+ seconds, which wastes the 60-second offer timeout and blocks the sender.

**Approach**: Send the OFFER with `sha256: ''` (pending). Compute the hash during chunk reading (accumulate into a streaming hash) or in parallel after OFFER is sent. The hash is included in the COMPLETE message, where it's already required for final verification. The receiver verifies against the COMPLETE hash, not the OFFER hash.

---

## 9. Component Specifications

### 9.1 FileTransferAdapter (`file-transfer-adapter.ts`)

The main class. Follows `StoreSyncAdapter` pattern (DI via config, caller-wired device events):

> **Pattern note**: Like `StoreSyncAdapter`, the adapter does **not** inject `MeshNode` directly. Instead, it exposes `handleDeviceDiscovered()` and `handleDeviceOffline()` methods that the caller wires to MeshNode events. Direct P2P connection is handled via an optional `connectToDevice` callback in config. This keeps the adapter testable with just a mock `IMessageBus`.

```typescript
class FileTransferAdapter extends TypedEventEmitter<FileTransferEvents> {
  constructor(config: FileTransferAdapterConfig)

  // Lifecycle (matches StoreSyncAdapter)
  start(): void
    // 1. Subscribe to FILE_TRANSFER_NAMESPACE on messageBus
    // 2. Clean up stale paused transfers

  stop(): void
    // 1. Cancel all active transfers (send CANCEL to peers)
    // 2. Unsubscribe from messageBus

  dispose(): void
    // 1. stop()
    // 2. Clean up temp files via assembler (including in-progress .ft-*.tmp files)
    // 3. Mark as disposed

  // Sending
  async sendFile(targetDeviceId: string, source: string | Buffer, metadata?: Record<string, unknown>): Promise<TransferId>
    // 1. Get file size and name
    // 2. Calculate totalChunks = Math.ceil(fileSize / chunkSize)
    // 3. Generate transferId: `ft-${deviceId.slice(0,8)}-${Date.now().toString(36)}-${randomHex(4)}`
    // 4. Create SendTransferState in TransferManager (sha256 computed lazily, see §8.6)
    // 5. Publish OFFER to target via messageBus.publish(targetDeviceId, ...)
    // 6. Start offer timeout (60s)
    // 7. Return transferId

  // Receiving
  acceptTransfer(transferId: TransferId): void
    // 1. Mark transfer as 'accepted' in TransferManager
    // 2. Call config.connectToDevice?.(senderDeviceId) for direct P2P (only receiver dials)
    // 3. Publish ACCEPT to sender

  rejectTransfer(transferId: TransferId, reason?: string): void
    // Publish REJECT to sender, remove from TransferManager

  // Control
  cancelTransfer(transferId: TransferId, reason?: string): void
  getTransfer(transferId: TransferId): TransferInfo | undefined
  getActiveTransfers(): TransferInfo[]

  // Device event handlers (caller wires these to MeshNode events)
  handleDeviceDiscovered(deviceId: string): void
    // Check for paused transfers with this peer
    // If found, send RESUME_REQUEST with verified chunks

  handleDeviceOffline(deviceId: string): void
    // Move active transfers with this peer to 'paused'

  // Internal message handler
  private handleMessage(msg: BusMessage): void
    // Route by msg.type to:
    //   OFFER → handleOffer(): emit 'offer' event
    //   ACCEPT → handleAccept(): start sending chunks (no dial — receiver already dialed)
    //   REJECT → handleReject(): emit 'rejected', cleanup
    //   CHUNK → handleChunk(): verify, write to assembler, send ACK
    //   CHUNK_ACK → handleChunkAck(): advance window, send next chunk
    //   RESUME_REQUEST → handleResumeRequest(): validate, send RESUME_RESPONSE, resume sending
    //   RESUME_RESPONSE → handleResumeResponse(): update state, resume receiving
    //   CANCEL → handleCancel(): cleanup (including temp files), emit 'cancelled'
    //   COMPLETE → handleComplete(): finalize assembler, verify hash, send VERIFIED
    //   VERIFIED → handleVerified(): mark completed, emit 'completed'
    //   ERROR → handleError(): mark failed, emit 'failed'
```

### 9.2 TransferManager (`transfer-manager.ts`)

Pure state machine logic, no messaging:

```typescript
class TransferManager {
  private sends: Map<TransferId, SendTransferState>
  private receives: Map<TransferId, ReceiveTransferState>

  // Creation
  createSend(transferId, file, peerDeviceId, sourcePath): SendTransferState
  createReceive(transferId, file, peerDeviceId): ReceiveTransferState

  // State transitions
  transitionState(transferId, newState): boolean  // returns false if invalid transition

  // Chunk tracking (sender)
  markChunkAcked(transferId, chunkIndex): void
  getNextChunksToSend(transferId, windowSize): number[]  // returns chunk indices to send
  shouldResendChunk(transferId, chunkIndex): boolean

  // Chunk tracking (receiver)
  markChunkReceived(transferId, chunkIndex): void
  getReceivedChunks(transferId): number[]
  getMissingChunks(transferId): number[]

  // Progress
  getTransferInfo(transferId): TransferInfo
  updateBytesPerSecond(transferId): void  // rolling average

  // Cleanup
  getTransfersForPeer(deviceId): TransferId[]
  getPausedTransfersForPeer(deviceId): TransferId[]
  removeTransfer(transferId): void
  removeStalePaused(maxAgeMs): TransferId[]

  // Query
  getActiveTransfers(): TransferInfo[]
  getSendState(transferId): SendTransferState | undefined
  getReceiveState(transferId): ReceiveTransferState | undefined
}

// Internal state types
interface SendTransferState {
  transferId: TransferId;
  file: FileMetadata;
  sourcePath: string;
  sourceHandle?: FileHandle;   // Persistent handle from fs.promises.open(), closed on complete/cancel/error
  receiverDeviceId: string;
  state: TransferState;
  ackedChunks: Set<number>;
  inFlightChunks: Set<number>;
  nextChunkIndex: number;      // next chunk to send
  runningHash?: Hash;          // Streaming SHA-256 accumulator for lazy whole-file hash (see §8.6)
  startedAt: number;
  lastActivityAt: number;
  retryCount: Map<number, number>;  // chunkIndex → retry count
}

interface ReceiveTransferState {
  transferId: TransferId;
  file: FileMetadata;
  senderDeviceId: string;
  state: TransferState;
  receivedChunks: Set<number>;
  startedAt: number;
  lastActivityAt: number;
}
```

### 9.3 Chunker (`chunker.ts`)

Reads specific chunks from a file source using a **persistent `FileHandle`** for file paths (avoids opening/closing the FD per chunk — for a 1GB file that's 2,048 open/close cycles otherwise):

```typescript
class Chunker {
  // Read a single chunk using a persistent handle, return base64 data + SHA-256
  static async readChunk(
    source: FileHandle | Buffer,  // persistent handle (from fs.promises.open()) or Buffer
    chunkIndex: number,
    chunkSize: number,
    fileSize: number,
  ): Promise<{ data: string; sha256: string }>
    // For FileHandle:
    //   const start = chunkIndex * chunkSize
    //   const length = Math.min(chunkSize, fileSize - start)
    //   const buf = Buffer.alloc(length)
    //   await handle.read(buf, 0, length, start)  // random access, no seek/reopen
    // For Buffer:
    //   buffer.subarray(start, start + length)
    // Then: base64 encode, compute SHA-256 of raw bytes

  // Compute whole-file SHA-256 (streaming, low memory)
  static async computeFileHash(source: string | Buffer): Promise<string>
    // For string: stream through crypto.createHash('sha256')
    // For Buffer: crypto.createHash('sha256').update(buffer).digest('hex')

  // Get file size
  static async getFileSize(source: string | Buffer): Promise<number>

  // Get file name from path
  static getFileName(source: string | Buffer): string
}
```

> **Resource lifecycle**: The `FileHandle` is opened by the adapter when sending begins (stored in `SendTransferState.fileHandle`) and closed on COMPLETE, CANCEL, or ERROR. The chunker itself is stateless — it receives the handle per call.

### 9.4 Assembler (`assembler.ts`)

Receives chunks and assembles the final file on disk:

```typescript
class Assembler {
  constructor(outputDir: string, transferId: TransferId, file: FileMetadata)

  // Write a verified chunk to temp file at correct offset
  async writeChunk(index: number, base64Data: string, expectedSha256: string): Promise<boolean>
    // 1. Base64 decode → Buffer
    // 2. SHA-256 of decoded bytes
    // 3. Compare with expectedSha256 → return false if mismatch
    // 4. Write to tempPath at offset (index * chunkSize)
    // 5. Add to receivedChunks set
    // 6. Return true

  // Finalize: verify whole-file hash, rename to final location
  async finalize(expectedSha256: string): Promise<{ path: string; valid: boolean }>
    // 1. Compute SHA-256 of entire temp file (streaming)
    // 2. Compare with expectedSha256
    // 3. If valid: rename temp → outputDir/filename, return { path, valid: true }
    // 4. If invalid: return { path: '', valid: false }

  // For resume: which chunks are already on disk and verified
  getReceivedChunks(): number[]

  // Clean up temp file
  async cleanup(): Promise<void>

  // Temp file path for this transfer
  getTempPath(): string
    // Return: path.join(outputDir, `.ft-${transferId}.tmp`)
}
```

### 9.5 Checksum (`checksum.ts`)

```typescript
// SHA-256 of a Buffer/Uint8Array → hex string
async function sha256(data: Buffer | Uint8Array): Promise<string>

// SHA-256 of a file (streaming, low memory) → hex string
async function sha256File(filePath: string): Promise<string>
```

Use Node.js `crypto` module (`crypto.createHash('sha256')`).

---

## 10. Direct Connection Strategy

### Why it matters

In STAR topology, secondary-to-secondary messages route through the primary:
```
Secondary A → Primary → Secondary B
```
For a 100MB file at 512KB chunks, that's 200 messages through the primary, doubling bandwidth usage.

### How to bypass

1. During ACCEPT handling, only the **receiver** calls `connectToDevice(sender)` via the `config.connectToDevice` callback
   - This calls `transport.connect(deviceId, hostname, dnsName, 443)` (mesh-node.ts:310)
   - Which triggers `sidecar.dial(deviceId, hostname, dnsName, port)` in the transport layer
   - The sender does NOT dial — it already knows the receiver is online (it just received the ACCEPT message). Both sides dialing simultaneously would create duplicate connections since `connectToDevice()` checks for existing connections (mesh-node.ts:305-308) but can race if both dial before either connection is established.
2. Once the direct connection is established, `sendEnvelope()` (mesh-node.ts:331) finds it via `transport.getConnectionByDeviceId(deviceId)` and sends directly
3. All chunk messages go peer-to-peer automatically

### Fallback

If `connectToDevice()` fails (rare on Tailscale), the adapter should:
1. Log a warning about degraded performance
2. Fall back to normal STAR routing (chunks go through primary)
3. The transfer still works, just slower

---

## 11. React Hook (Follow-up)

> **Out of scope for this RFC.** The React hook (`useFileTransfer`) should be implemented as a separate follow-up once the core `@vibecook/truffle-file-transfer` package is stable. It is not an acceptance criterion for the file transfer primitive.

When implemented, add `use-file-transfer.ts` to `packages/react/src/` following the pattern of `use-mesh.ts` and `use-synced-store.ts`:
- `useEffect` to subscribe to adapter events, cleanup on unmount
- `useState` for transfers and pendingOffers
- `useCallback` for memoized action functions
- `useRef` for adapter reference

---

## 12. Testing Requirements

### Test framework: Vitest (already used across the project)

### Mock patterns (follow `packages/store-sync/src/__tests__/store-sync.test.ts`)

```typescript
// Quiet logger (reuse from existing tests)
const quietLogger = { info: () => {}, warn: () => {}, error: () => {}, debug: () => {} };

// Mock MessageBus
function createMockMessageBus(): IMessageBus {
  return {
    subscribe: vi.fn().mockReturnValue(() => {}),
    publish: vi.fn().mockReturnValue(true),
    broadcast: vi.fn(),
  };
}

// Mock MeshNode
function createMockMeshNode(deviceId = 'dev-1'): MeshNode {
  const emitter = new EventEmitter();
  return Object.assign(emitter, {
    getDeviceId: vi.fn().mockReturnValue(deviceId),
    connectToDevice: vi.fn().mockResolvedValue(undefined),
    isRunning: vi.fn().mockReturnValue(true),
    getMessageBus: vi.fn(),
    getTransport: vi.fn(),
    getDeviceManager: vi.fn(),
  }) as any;
}
```

### Required test cases

**file-transfer-adapter.test.ts**:
- Lifecycle: start subscribes to namespace, stop unsubscribes, dispose cleans up
- `sendFile()` publishes OFFER with correct metadata and hash
- Incoming OFFER emits 'offer' event
- `acceptTransfer()` calls `connectToDevice()` and publishes ACCEPT
- `rejectTransfer()` publishes REJECT
- Incoming ACCEPT triggers chunk sending (verify first chunk published)
- Incoming REJECT emits 'rejected' and cleans up
- Incoming CANCEL emits 'cancelled' and cleans up
- Device offline pauses active transfers
- Device rediscovered triggers RESUME_REQUEST for paused transfers
- Ignores messages from self
- Ignores messages for unknown transfer IDs
- Offer timeout (60s) auto-cancels

**transfer-manager.test.ts**:
- Valid state transitions: offering → accepted → transferring → completing → completed
- Invalid transitions rejected (e.g., completed → transferring)
- Sliding window: `getNextChunksToSend(4)` returns correct indices
- ACK advances window correctly
- NACK triggers resend
- Progress calculation: percentages, bytes/second, ETA
- Missing chunk detection for resume
- Stale paused cleanup

**chunker.test.ts**:
- Correct chunk boundaries for first, middle, last chunk
- Last chunk is smaller when file size is not a multiple of chunkSize
- SHA-256 matches known test vectors
- File path source works (use temp files in tests)
- Buffer source works
- Single-chunk files (size < chunkSize)
- Empty file edge case

**assembler.test.ts**:
- Chunks in order → correct file
- Chunks out of order → correct file (offset-based writing)
- SHA-256 verification catches corrupt chunks
- Final hash verification
- `getReceivedChunks()` returns correct indices (for resume)
- Cleanup removes temp files
- Disk write error handling

**integration.test.ts** (mock two adapters connected via shared mock bus):
- Full happy path: offer → accept → all chunks → complete → verified
- Resume: partial transfer → "disconnect" → resume → complete
- Two concurrent transfers to different peers
- Checksum mismatch → chunk resent → transfer completes
- Cancel mid-transfer → both sides clean up

---

## 13. Example App (Follow-up)

> **Out of scope for this RFC.** The example app should be implemented after the core package is functional. It is not an acceptance criterion.

When implemented, add `examples/file-transfer/index.ts` — a CLI-based demo that creates a mesh node, starts the file transfer adapter, and either sends a file to the first discovered peer or waits for incoming offers.

---

## 14. Future: Go-Native Fast Path

For maximum throughput, the Go sidecar would handle file I/O directly:

> **Note**: Theoretical max is limited by WireGuard tunnel throughput, typically ~300-500 Mbps on modern hardware (~40-60 MB/s). The "~80-100+ MB/s" figure previously cited here exceeds realistic WireGuard performance. Expect ~40-60 MB/s on direct LAN, less through DERP relay.

1. New IPC commands: `ft:send { targetHost, filePath }`, `ft:accept { transferId, savePath }`
2. New Go package: `packages/sidecar/internal/filetransfer/`
3. Go reads file from disk → sends over dedicated Tailscale TCP connection → remote Go writes to disk
4. Progress reported via IPC events to Node.js
5. TypeScript `FileTransferAdapter` detects sidecar capability and delegates

This eliminates Node.js from the data path entirely. Base64 encoding, JSON wrapping, and stdin/stdout overhead are all bypassed.

**This is a separate effort** and should not block the initial release.

---

## 15. Implementation Order

1. **Prerequisite**: Fix `SidecarClient.sendCommand()` to respect `write()` backpressure (see §17)
2. Sidecar buffer/binary fixes (5 Go lines)
3. `types.ts` + `constants.ts`
4. `checksum.ts` + tests
5. `chunker.ts` + tests (using persistent `FileHandle`)
6. `assembler.ts` + tests
7. `transfer-manager.ts` + tests
8. `file-transfer-adapter.ts` + tests (with caller-wired device events)
9. `index.ts` (public exports)
10. Add types to `@vibecook/truffle-types`
11. Integration tests
12. Build + verify all existing tests pass
13. _(Follow-up)_ React hook
14. _(Follow-up)_ Example app

---

## 16. Acceptance Criteria

- [ ] Sidecar WebSocket buffers increased to 64KB
- [ ] Sidecar send channels increased to 1024
- [ ] Sidecar uses BinaryMessage mode
- [ ] New `@vibecook/truffle-file-transfer` package builds and passes tests
- [ ] Can transfer files of any size (tested with 1MB, 100MB, 1GB)
- [ ] Transfer resumes after simulated disconnect
- [ ] Progress events fire with correct percentages and speed
- [ ] Direct P2P connections established for data transfer (receiver-initiated dial)
- [ ] Checksum verification catches corruption
- [ ] All existing Truffle tests still pass

### Follow-up items (not acceptance criteria)

- [ ] React hook `useFileTransfer()` (see §11)
- [ ] Example app demonstrates send/receive between two devices (see §13)
- [ ] Types re-exported from a top-level package if one is created

---

## 17. Known Limitations & Risks

Issues identified during review that should be tracked and addressed:

### Prerequisite Fix

- **`SidecarClient.sendCommand()` ignores `write()` backpressure.** The current implementation doesn't check the return value of `stdin.write()`. When the Go sidecar's `readLoop()` is slow (e.g., handler blocks on a full WebSocket send channel), the OS pipe buffer fills (~64KB on macOS/Linux), and Node's write returns `false`. Without respecting this, the chunk-sending loop can buffer gigabytes in Node's memory. **Fix**: Check `write()` return value and `await` the `'drain'` event before sending the next chunk. This benefits all Truffle features, not just file transfer.

### No Backpressure Mechanism

The receiver cannot signal the sender to slow down beyond the natural pacing of ACK messages. With a sliding window of 4 chunks, the sender can be up to 4 x 683KB (~2.7MB) ahead of what the receiver has processed. For most cases this is fine, but on a very slow receiver (e.g., writing to a slow SD card) the chunks will queue in the Go sidecar's send channel. If the channel fills, the Go sidecar **drops the connection** (`"buffer full"` at `websocket.go:104-105`). Mitigation: The increased channel buffer (256→1024) provides more headroom, and the ACK-based window inherently limits how far ahead the sender gets.

### No Concurrent Transfer Bandwidth Sharing

Multiple simultaneous transfers to the same peer share the same WebSocket connection and sidecar send channel with no fairness mechanism. A large transfer can starve a small one. Consider round-robin chunk scheduling across active transfers in a future version.

### Cancel Should Clean Up Temp Files Immediately

The current design cleans up temp files in `dispose()`, but CANCEL should also immediately delete the `.ft-*.tmp` file for the cancelled transfer to avoid leaving orphaned data on disk.

### Stale Temp File Cleanup

On startup, scan the output directory for `.ft-*.tmp` files from previous crashes and clean them up. The `start()` method already calls "clean up stale paused transfers" but should also handle orphaned temp files with no matching transfer state.

### `PAUSED_TRANSFER_TTL_MS` Default

The 24-hour default for paused transfer TTL is long. If a user transfers a 100MB file, disconnects, and never comes back, the temp file sits on disk for a full day. Consider a shorter default of **2-4 hours**, configurable via `FileTransferAdapterConfig`.

---

## Appendix A: Review of External Feedback

Four external feedback points were evaluated against the actual codebase. Summary:

### A.1 IPC Backpressure (Memory Bloat Risk)

**Claim**: Writing large base64 chunks to `process.stdin.write()` without checking backpressure will cause Node.js to buffer gigabytes in RAM.

**Verdict: Partially valid.** The concern is real but the diagnosis is slightly wrong. The actual backpressure chain is: Go handler slow → OS pipe buffer full (~64KB) → Node `write()` returns `false`. This is correct OS-level backpressure. The issue is that `SidecarClient.sendCommand()` ignores the `write()` return value. **Action**: Fix `sendCommand()` to respect `write()` backpressure (listed as prerequisite in §15 and §17).

### A.2 MessageBus Congestion (Head-of-Line Blocking)

**Claim**: Blasting 4 concurrent CHUNK messages into the bus will starve other critical system messages.

**Verdict: Incorrect.** The MessageBus (`mesh-message-bus.ts`) is purely in-memory dispatch — `publish()` is a synchronous call chain ending in `stdin.write()`. There is no shared queue between different namespace messages. A `file-transfer` chunk and a `sync` update both call `stdin.write()` independently. The only shared bottleneck is the OS stdin pipe and the Go sidecar's single `readLoop()` goroutine, but this processes JSON lines one at a time in microseconds (parse + channel send). 4 x 683KB chunks adds ~2.7MB to the pipe, causing negligible latency for other messages. **Action**: None required.

### A.3 File Descriptor Thrashing in Chunker

**Claim**: Opening a `createReadStream()` per chunk for a 1GB file (2,048 open/close cycles) will severely throttle disk I/O.

**Verdict: Valid principle, overstated impact.** The overhead per open/close is ~10-50 microseconds, so 2,048 calls adds ~20-100ms total — not "severely throttled." But keeping a persistent `FileHandle` open is cleaner API design and avoids fd churn. **Action**: Adopted. Chunker now uses `FileHandle` with `handle.read()` for random access (see §9.3). Handle stored in `SendTransferState`, closed on completion/cancel/error.

### A.4 V8 Garbage Collection Tax

**Claim**: Base64 encoding 512KB chunks will generate many short-lived objects causing GC CPU spikes. Fix: pre-allocate a reusable Buffer.

**Verdict: Incorrect.** `Buffer.alloc(512 * 1024)` allocates memory **outside V8's heap** (in C++ land, via `ArrayBuffer`) — not managed by V8's GC. The base64 string (`buffer.toString('base64')`) creates a ~683KB string in V8's old generation (collected in major GC, not minor). Pre-allocating a "reusable Buffer" doesn't reduce GC pressure because: (1) the read buffer is immediately consumed by `toString('base64')` which creates a new string regardless; (2) strings are immutable in JavaScript. At 4 in-flight chunks, peak live strings are ~2.8MB — trivial for V8. **Action**: None required.
