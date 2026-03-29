# RFC 014: Move File Transfer to truffle-core with Accept/Reject API

**Status:** Draft
**Author:** James + Claude
**Date:** 2026-03-29

---

## 1. Problem Statement

File transfer is currently implemented entirely in the CLI layer (`crates/truffle-cli/src/apps/file_transfer/`). This has two problems:

1. **Not reusable.** Any app using truffle-core (Electron, Tauri, React Native) would need to reimplement the entire file transfer protocol — types, signaling, TCP streaming, SHA-256 verification, progress tracking. That's ~500 lines of protocol code per consumer.

2. **No accept/reject control.** The receive handler (`receive.rs`) auto-accepts everything. The TUI can't show an interactive prompt because the decision happens inside a monolithic background task that the TUI doesn't control.

## 2. Goals

- Move file transfer from CLI to core as a first-class feature
- Expose a clean, generic API that any app can use
- Support pluggable accept/reject policies (auto-accept, interactive prompt, allow-list, etc.)
- Emit events (offer received, progress, complete, failed) that apps can subscribe to
- Keep the protocol unchanged (FtMessage, TCP streaming, SHA-256 verification)

## 3. Current Architecture

```
truffle-core (generic library)
├── node.rs          — Node API: send(), subscribe(), open_tcp(), listen_tcp()
├── session/         — Peer registry, WebSocket connections
├── transport/       — WS, TCP, QUIC
├── network/         — Tailscale provider
└── envelope/        — Namespace framing

truffle-cli (application)
└── apps/file_transfer/
    ├── types.rs     — FtMessage, TransferResult, TransferError
    ├── upload.rs    — Push file to peer (Offer → Accept → TCP stream)
    ├── download.rs  — Pull file from peer (PullRequest → Offer → Accept → TCP)
    └── receive.rs   — Auto-accept background handler (monolithic)
```

The file transfer protocol uses truffle-core's primitives:
- `node.subscribe("ft")` for signaling messages
- `node.send(peer, "ft", data)` for sending signaling messages
- `node.listen_tcp()` / `node.open_tcp()` for data transfer
- All on the "ft" namespace

## 4. Proposed Architecture

```
truffle-core
├── node.rs          — Node API (unchanged)
├── session/         — (unchanged)
├── transport/       — (unchanged)
├── network/         — (unchanged)
├── envelope/        — (unchanged)
└── file_transfer/   — NEW: first-class file transfer module
    ├── mod.rs       — FileTransfer manager, public API
    ├── types.rs     — FtMessage, TransferResult, TransferError, FileOffer
    ├── sender.rs    — Send/push file to peer
    └── receiver.rs  — Receive file with pluggable accept/reject

truffle-cli (application — thin wrapper)
└── apps/file_transfer/  — DELETED (moved to core)
    CLI commands just call node.file_transfer().send_file() etc.
```

## 5. Core API Design

### 5.1 The `FileTransfer` Manager

Attached to the `Node` and accessible via `node.file_transfer()`. Manages active transfers and emits events.

```rust
/// Access the file transfer manager.
impl<N: NetworkProvider> Node<N> {
    pub fn file_transfer(&self) -> &FileTransfer<N> { ... }
}
```

### 5.2 Sending Files

```rust
impl<N: NetworkProvider> FileTransfer<N> {
    /// Send a file to a peer.
    ///
    /// Returns a handle that emits progress events. The transfer begins
    /// immediately — the peer's accept/reject handler determines if they
    /// receive it.
    pub async fn send_file(
        &self,
        peer_id: &str,
        local_path: &str,
        remote_path: &str,
        on_progress: impl Fn(TransferProgress) + Send + 'static,
    ) -> Result<TransferResult, TransferError>;

    /// Request a file from a peer (pull/download).
    pub async fn pull_file(
        &self,
        peer_id: &str,
        remote_path: &str,
        local_path: &str,
        on_progress: impl Fn(TransferProgress) + Send + 'static,
    ) -> Result<TransferResult, TransferError>;
}
```

### 5.3 Receiving Files — The Accept/Reject API

This is the key design decision. The core needs to support different policies without knowing about TUIs, GUIs, or CLIs.

**Design: Callback-based offer handler.**

```rust
/// An incoming file offer that can be accepted or rejected.
pub struct FileOffer {
    pub from_peer: String,
    pub from_name: String,
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    pub suggested_path: String,
    pub token: String,
}

/// The decision for an incoming file offer.
pub enum OfferDecision {
    /// Accept the file, save to this path.
    Accept { save_path: String },
    /// Reject the file with a reason.
    Reject { reason: String },
}

impl<N: NetworkProvider> FileTransfer<N> {
    /// Set the handler for incoming file offers.
    ///
    /// The handler is called for each incoming offer and must return
    /// a decision (accept or reject). The handler runs in a tokio task
    /// so it can be async-like via channels.
    ///
    /// If no handler is set, incoming offers are rejected with
    /// "no offer handler configured".
    pub fn set_offer_handler(
        &self,
        handler: impl Fn(FileOffer) -> OfferDecision + Send + Sync + 'static,
    );

    /// Convenience: set an auto-accept handler that saves to a directory.
    pub fn auto_accept(&self, output_dir: &str);

    /// Convenience: set an auto-reject handler.
    pub fn auto_reject(&self);
}
```

**Why callback, not channel?** A channel-based API (`offer_rx.recv()`) would require the consumer to poll continuously. A callback is simpler for the common case (auto-accept) and can be wrapped with a channel by advanced consumers (like the TUI).

### 5.4 Events

Apps subscribe to transfer events for UI updates:

```rust
/// Events emitted by the file transfer system.
#[derive(Debug, Clone)]
pub enum FileTransferEvent {
    /// An incoming offer was received (before accept/reject decision).
    OfferReceived(FileOffer),

    /// Transfer progress update.
    Progress {
        token: String,
        direction: TransferDirection,
        file_name: String,
        bytes_transferred: u64,
        total_bytes: u64,
        speed_bps: f64,
    },

    /// Transfer completed successfully.
    Completed {
        token: String,
        direction: TransferDirection,
        file_name: String,
        bytes_transferred: u64,
        sha256: String,
        elapsed_secs: f64,
    },

    /// Transfer failed.
    Failed {
        token: String,
        direction: TransferDirection,
        file_name: String,
        reason: String,
    },
}

impl<N: NetworkProvider> FileTransfer<N> {
    /// Subscribe to file transfer events.
    pub fn subscribe(&self) -> broadcast::Receiver<FileTransferEvent>;
}
```

### 5.5 How the TUI Uses This

```rust
// In TUI startup:
let ft = node.file_transfer();

// Set up offer handler that sends to the TUI event loop
let (offer_tx, offer_rx) = mpsc::unbounded_channel();
ft.set_offer_handler(move |offer| {
    // Send the offer to the TUI for interactive display
    let _ = offer_tx.send(offer.clone());
    // Block until TUI responds (via a oneshot channel stored in the offer)
    // ... or use a default timeout
});

// Subscribe to progress events for the feed
let mut ft_events = ft.subscribe();

// In the event collector:
// - offer_rx delivers FileOffer to show the accept/reject prompt
// - ft_events delivers Progress/Completed/Failed for the activity feed
```

**For async accept/reject** (the TUI needs to wait for user input), the offer handler can use a `tokio::sync::oneshot` channel internally:

```rust
ft.set_offer_handler_async(move |offer| -> Pin<Box<dyn Future<Output = OfferDecision>>> {
    let tx = offer_tx.clone();
    Box::pin(async move {
        let (decision_tx, decision_rx) = oneshot::channel();
        let _ = tx.send((offer, decision_tx));
        decision_rx.await.unwrap_or(OfferDecision::Reject {
            reason: "timeout".to_string(),
        })
    })
});
```

Or simpler: the handler is synchronous but the TUI uses a shared `Arc<Mutex<>>` decision map:

```rust
let decisions: Arc<DashMap<String, oneshot::Sender<OfferDecision>>> = ...;

ft.set_offer_handler(move |offer| {
    let (tx, rx) = oneshot::channel();
    decisions.insert(offer.token.clone(), tx);
    // Block on the oneshot (handler runs in a spawned task)
    rx.blocking_recv().unwrap_or(OfferDecision::Reject {
        reason: "timeout".to_string(),
    })
});

// TUI receives offer via event, shows prompt, user presses 'a':
if let Some((_, tx)) = decisions.remove(&token) {
    let _ = tx.send(OfferDecision::Accept { save_path });
}
```

### 5.6 How the Daemon Uses This

```rust
// In daemon mode (no TUI, no interactive prompts):
let ft = node.file_transfer();

// Check config for auto-accept peers
if config.auto_accept_all {
    ft.auto_accept("~/Downloads/truffle/");
} else {
    ft.auto_reject(); // or accept only from allowed peers
}
```

### 5.7 How an Electron App Would Use This

```rust
// Via NAPI:
let ft = node.file_transfer();

// Events → JavaScript callbacks
let mut rx = ft.subscribe();
tokio::spawn(async move {
    while let Ok(event) = rx.recv().await {
        // Call JS callback with event
    }
});

// Offer handler → JavaScript
ft.set_offer_handler(move |offer| {
    // Send to JS, block for response
    // JS shows a native dialog, user clicks accept/reject
});
```

## 6. Dependencies for truffle-core

Add to `crates/truffle-core/Cargo.toml`:

```toml
sha2 = "0.10"
```

`uuid` and `hex` are already present. `serde`/`serde_json` too. No other new deps needed.

## 7. Module Structure

```
crates/truffle-core/src/file_transfer/
    mod.rs         — FileTransfer<N> struct, set_offer_handler(), subscribe()
    types.rs       — FileOffer, OfferDecision, TransferProgress, TransferResult,
                     TransferError, FileTransferEvent, FtMessage (moved from CLI)
    sender.rs      — send_file(), pull_file() (moved from CLI upload.rs/download.rs)
    receiver.rs    — background offer listener, calls handler, manages TCP receive
```

## 8. Migration Path

### Phase 1: Add file_transfer module to truffle-core
- Move types.rs (FtMessage, TransferResult, TransferError) to core
- Move upload logic to core as `sender.rs`
- Move receive logic to core as `receiver.rs` with the new offer handler API
- Add `FileTransfer<N>` manager struct
- Wire into `Node` via `node.file_transfer()`
- Add `sha2` dependency to core

### Phase 2: Update truffle-cli to use core API
- Delete `cli/src/apps/file_transfer/` entirely
- Update `commands/cp.rs` to call `node.file_transfer().send_file()`
- Update daemon handler to call `node.file_transfer()` methods
- Update TUI `/cp` command to use core API

### Phase 3: TUI accept/reject modal dialog
- Subscribe to core's offer channel for `(FileOffer, OfferResponder)` pairs
- Display modal dialog overlay with `[a]ccept [s]ave as... [r]eject [d]on't ask again`
- Implement "save as..." with inline path editor + Tab file picker
- Implement "don't ask again" via config auto_accept_peers
- Queue multiple offers — one dialog at a time, 60s timeout per offer

### Phase 4: Progress events in TUI feed
- Subscribe to `ft.subscribe()` for FileTransferEvent
- Replace the current manual progress forwarding with core events
- Show proper progress bars inline in the activity feed
- Completion with SHA-256, failure with reason

## 9. What Stays in CLI

- The `/cp` slash command parsing (TUI-specific)
- The `truffle cp` one-shot command (CLI-specific)
- The interactive accept/reject TUI modal (TUI-specific)
- Config for auto-accept peers (CLI-specific)

## 10. What Moves to Core

- `FtMessage` enum (signaling protocol)
- `TransferResult`, `TransferError` types
- Upload/send logic (hash → offer → wait accept → TCP stream)
- Download/pull logic (pull request → wait offer → accept → TCP receive)
- Receive handler with pluggable accept/reject (channel-based)
- Progress event system (broadcast)
- SHA-256 verification
- Streaming to temp file + rename on verification

## 11. Decided Questions

| Question | Decision | Rationale |
|----------|----------|-----------|
| Auto-start vs explicit | **Lazy start** — activates on first `set_offer_handler()` or `auto_accept()` | No wasted resources, sending always works |
| Handler type | **Channel-based** — `(FileOffer, OfferResponder)` pairs | Cleanest for TUI/GUI/JS event loops, timeout-safe |
| Disk vs memory | **Stream to disk** — temp file + incremental SHA-256 + rename | Constant memory, handles any file size |
| Size limit | **Default 1GB, overridable** via `set_max_transfer_size()` | Safe by default |

## 12. TUI Accept/Reject UX Design

### Modal Dialog (Option B)

When a file offer arrives, a centered modal overlay appears on top of the activity feed:

```
╭─── incoming file ────────────────────────────────────╮
│                                                      │
│  server wants to send you a file:                    │
│                                                      │
│  📄 report.pdf  (2.1 MB)                            │
│  Save to: ~/Downloads/truffle/report.pdf             │
│                                                      │
│  [a] Accept    [s] Save as...    [r] Reject          │
│                           [d] Don't ask from server  │
│                                                      │
╰──────────────────────────────────────────────────────╯
```

The dialog captures all keypresses until dismissed. The user cannot interact with the input bar or feed while the dialog is open.

### Responses

**[a] Accept** — accept with default save path. Dialog closes, transfer begins:
```
  ⬇ report.pdf from server                            14:05
    ████████████████████████████████ 100%  3.4 MB/s
    ✓ 2.1 MB in 0.6s  sha256: 3a8f...d2c1
```

**[s] Save as...** — dialog transforms to path editor:
```
╭─── incoming file ────────────────────────────────────╮
│                                                      │
│  Save report.pdf to:                                 │
│  ❯ ~/Downloads/truffle/report.pdf_                   │
│                                                      │
│  [Enter] Confirm    [Tab] Browse    [Esc] Back       │
│                                                      │
╰──────────────────────────────────────────────────────╯
```

Tab opens the file picker (ratatui-explorer) for directory selection. Enter confirms the path. Esc goes back to the main prompt.

**[r] Reject** — reject the transfer. Dialog closes:
```
  ⬇ report.pdf from server  ✗ rejected                14:05
```

**[d] Don't ask again** — accept this transfer AND add the peer to `auto_accept_peers` in config.toml. Future offers from this peer skip the dialog:
```
  ⬇ report.pdf from server  ✓ auto-accepted           14:05
    (Future files from server accepted automatically)
```

### Timeout

If the user doesn't respond within 60 seconds, the offer is auto-rejected:
```
  ⬇ report.pdf from server  ✗ timed out               14:05
```

### Multiple simultaneous offers

Offers queue. Only one dialog shows at a time. When the user responds, the next queued offer's dialog appears. Queued offers have their own 60s timeout running independently.

### Send progress display

Outgoing transfers (`/cp`) show inline in the feed:
```
  ⬆ report.pdf → server                               14:05
    ████████████████████░░░░░░░░░░░░ 45%  1.2 MB/s  eta 3s
```

On completion:
```
  ⬆ report.pdf → server  ✓  2.1 MB in 1.8s            14:05
```

On failure:
```
  ⬆ report.pdf → server  ✗                             14:05
    Connection closed after 1.2 MB
```
