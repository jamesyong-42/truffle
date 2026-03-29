# RFC 013: TUI Redesign — Dual-Mode Interactive CLI

**Status:** Accepted
**Author:** James + Claude
**Date:** 2026-03-28

---

## 1. Problem Statement

The current Truffle CLI has 10 one-shot commands. They work, but they're designed for neither humans nor agents particularly well.

**For humans**, the experience is poor:
- No persistent session. Run a command, get output, exit. No way to watch what's happening on the mesh.
- **Blind receivers.** `truffle send` delivers a message but the other side sees nothing. File transfers auto-accept silently in foreground mode.
- No interactivity. No chat-like flow, no real-time feedback, no notifications.
- You have to keep running `truffle ls` or `truffle status` to see what changed.

**For agents**, the experience is incomplete:
- `--json` only on `status` and `ls`, not on every command.
- Exit codes are just 0/1 (no semantic codes for not-found, timeout, etc.).
- No `--quiet` wiring through all commands.
- No structured error output.

## 2. Vision

The CLI should be **human-first, agent-complete**:

| Mode | Entry | Experience |
|------|-------|------------|
| **Interactive TUI** | `truffle` (bare command) | Persistent session like Claude Code. Activity feed, slash commands, real-time notifications. The primary human experience. |
| **One-shot commands** | `truffle <subcommand>` | Current behavior, enhanced. `--json` everywhere, structured exit codes. The primary agent/script experience. |

The distinction: **TUI is a session, commands are transactions.**

Running bare `truffle` launches an interactive terminal UI that:
- Starts the daemon in-process (same as current `truffle up --foreground`)
- Shows a live activity feed — peer events, messages, file transfers
- Accepts `/` slash commands for all operations
- Displays incoming messages and file transfer prompts in real-time
- Keeps running until you `/quit`

Meanwhile, one-shot commands (`truffle status`, `truffle ls --json`, `truffle cp file.txt server:/tmp/`) continue to work from other terminals, talking to the running TUI/daemon via IPC.

## 3. Architecture Decision: TUI = Foreground Daemon

Two options were considered:

**Option A: TUI runs the daemon in-process (recommended)**
- Extends the existing `truffle up --foreground` pattern
- Direct access to `Node`, peer events, message subscriptions — no IPC needed for live data
- One-shot commands from other terminals still work via IPC (the daemon accept loop runs as a background tokio task)
- Simple. No protocol changes.

**Option B: TUI as a separate process connecting to a background daemon**
- Would need to extend IPC with streaming subscriptions (peer events, messages, file transfers)
- Significant protocol complexity for no user-visible benefit
- TUI would need reconnection logic if daemon restarts

**Decision: Option A.** The TUI owns the daemon. This is the same architecture as `truffle up --foreground`, but with a ratatui UI instead of raw `println!`.

## 4. Tech Stack

**Decision: ratatui + crossterm (full alternate-screen TUI).**

Alternatives considered:
- **iocraft** (React/Ink-like, inline rendering) — compelling inline mode but v0.8 with single maintainer, 87k downloads vs ratatui's 21.7M. Too early.
- **Raw crossterm + ANSI scroll regions** — minimal deps but ~500-1000 lines of manual rendering code for our layout. Sidebar and autocomplete overlay would be painful.
- **Cursive** — sync event loop is incompatible with our tokio-based architecture. Skip.

ratatui wins because:
- Our layout (pinned status bar + scrollable feed + pinned input + toggleable sidebar + autocomplete overlay) needs a real layout engine
- Chat TUI references exist (iamb Matrix client is nearly identical to our use case)
- Idle CPU is solvable: only call `terminal.draw()` on events, not at 60fps
- The alternate-screen tradeoff is fine — the TUI *is* the experience; one-shot commands exist for normal terminal use

| Crate | Version | Purpose |
|-------|---------|---------|
| `ratatui` | 0.30 | TUI framework. Immediate-mode rendering, component architecture. 21.7M downloads. |
| `crossterm` | 0.29 + `event-stream` | Terminal backend. Async event stream integrates with tokio. Cross-platform. |
| `tui-textarea` | 0.7 | Input bar widget. Single-line mode, cursor movement, clipboard. Under ratatui org. |
| `tui-scrollview` | latest | Scrollable container for the activity feed. By ratatui maintainer (joshka). |
| `ratatui-explorer` | 0.3.0 | File browser overlay for `/cp`. 57k downloads, Widget trait, Vim + arrow keys. |

Compile impact: ~49 new transitive crates, 6-9s clean build. Incremental builds sub-second. All TUI deps stay in the CLI crate — zero changes to truffle-core.

Reference app: **iamb** (Matrix chat client, ratatui) — scrollable message history, input bar, status line, multi-room. Closest to our architecture.

## 5. Visual Design

### 5.1 Startup Banner

On launch, a branded header box renders with block-letter logo, tagline, and node info:

```
  ╭──────────────────────────────────────────────────────╮
  │                                                      │
  │   ▀█▀ █▀█ █ █ █▀▀ █▀▀ █   █▀▀                      │
  │    █  █▀▄ █ █ █▀  █▀  █   █▀                        │
  │    █  █ █ ▀▄▀ █   █   █▄▄ █▄▄                       │
  │                                                      │
  │   mesh networking for your devices                   │
  │                                                      │
  │   my-laptop · 100.64.0.5                             │
  │   ● online · 3 peers · uptime 2h 15m                │
  │                                                      │
  ╰──────────────────────────────────────────────────────╯
```

The banner is part of the scrollable activity feed — it renders once at the top, then naturally scrolls away as events fill the screen. It is NOT a persistent header widget.

### 5.2 Persistent Status Bar

Once the banner scrolls away, a 1-line status bar stays pinned at the top:

```
  truffle · my-laptop · ● online · 100.64.0.5 · 3 peers · 2h 15m
```

This updates live (peer count, uptime, connection status). The uptime refreshes on each tick (1s).

### 5.3 Full Layout (After Banner Scrolls Away)

```
  truffle · my-laptop · ● online · 100.64.0.5 · 3 peers · 2h 15m

  ● truffle-server joined (100.64.0.3)            14:02
  ● truffle-pi joined (100.64.0.8)                14:02

  You → server: Hello!                            14:03
                How are things?                   14:03
  server → You: Good, you?                        14:03

  ⬇ report.pdf from server (2.1 MB)              14:05
    ████████████████████████████████░░░░ 82%

  ○ truffle-pi disconnected                       14:06

  ─────────────────────────────────────────────────────
  > _
```

### 5.4 Layout Zones

Three zones (no persistent sidebar by default):

1. **Status bar** (top, 1 line, pinned) — Node name, connection status, IP, peer count, uptime. Always visible.

2. **Activity feed** (center, scrollable) — Chronological event stream. Peer events, chat messages, file transfers, command output, system messages. Auto-scrolls to bottom on new content unless user has scrolled up. PageUp/PageDown for scrollback.

3. **Input bar** (bottom, 1 line, pinned) — Thin `─` separator line above a `> ` prompt. `/` prefix triggers slash command mode with autocomplete overlay. Enter submits. Up/Down for command history.

**Device list panel** (right side, always visible) — Shows all known peers with online/offline indicators, connection type (direct/relay), updated in real-time by peer events. This is NOT a toggleable sidebar — it's a persistent part of the layout. ~20 chars wide.

### 5.5 Message Formatting

**Consecutive messages from the same sender collapse:**

```
  You → server: Hello!                            14:03
                How are things?                   14:03
  server → You: Good, you?                        14:03
                Everything's fine                 14:03
```

First message shows full `Sender → Recipient:` prefix. Subsequent messages from the same sender within a short window (e.g. 60s) show only indented content, aligned with the text of the first message.

**Timestamps** are right-aligned, dimmed. Only shown on the first message of a group or when the time changes.

### 5.6 Color Palette

| Color | Usage |
|-------|-------|
| Green | Online indicators `●`, success messages, connection status |
| Red | Error messages, transfer failures |
| Dim/gray | Offline indicators `○`, timestamps, metadata, system messages, separator lines |
| Yellow | Warnings, "connecting" status |
| Cyan | Your own outgoing messages (the "You →" prefix and text) |
| White/default | Incoming messages, peer names, content |
| Bold | Node names, command names, important labels |

### 5.7 Input Prompt

The prompt is always `> `. No mode switching, no context-dependent prompts. The TUI has one mode: you're looking at the feed and typing commands.

When typing `/`, a command autocomplete overlay appears above the input bar. When typing `@`, a device picker overlay appears. These are overlays, not mode switches.

### 5.8 Full Layout With Device Panel

```
  truffle · my-laptop · ● online · 100.64.0.5 · 2h 15m
  ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
                                          │ DEVICES
  ● server joined (direct)        14:02   │ ● server
  ● pi joined (relay)             14:02   │ ● pi
                                          │
  You → server: Hello!            14:03   │
                How are things?   14:03   │
  server → You: Good, you?       14:03   │
                                          │
  ⬇ report.pdf from server       14:05   │
    ████████████████░░░░ 82%              │
                                          │
  ○ pi disconnected              14:06   │ ○ pi
                                          │
  ────────────────────────────────────────���───────────���
  > _
```

The device panel is always visible on the right, separated by a thin `│` line. It updates live as peers join/leave/change connection type.

## 6. Event Architecture

All event sources funnel into a single `mpsc::UnboundedSender<AppEvent>`:

```rust
pub enum AppEvent {
    // Terminal input
    Key(KeyEvent),
    Resize(u16, u16),

    // Mesh events (from Node broadcast channels)
    PeerEvent(PeerEvent),
    IncomingMessage { from_id: String, from_name: String, text: String },
    FileTransferEvent(FtEvent),

    // Internal
    CommandResult { id: u64, output: Vec<DisplayItem> },
    Tick,  // 1s periodic for uptime counter, toast expiry
}
```

Five collector tasks spawned at TUI startup:

| Task | Source | Maps to |
|------|--------|---------|
| Crossterm | `crossterm::event::EventStream` | `Key`, `Resize` |
| Peer events | `server.subscribe_peer_events()` | `PeerEvent` |
| Chat messages | `node.subscribe("chat")` | `IncomingMessage` |
| File transfers | `node.subscribe("ft")` | `FileTransferEvent` |
| Tick timer | `tokio::time::interval(1s)` | `Tick` |

Main render loop:

```rust
loop {
    terminal.draw(|f| ui::render(f, &app))?;
    match event_rx.recv().await {
        Some(event) => {
            if app.handle_event(event) == ShouldQuit::Yes { break; }
        }
        None => break, // all senders dropped
    }
}
```

This is the channel-funnel pattern recommended by ratatui's async documentation. Avoids complex `tokio::select!` in the render loop.

### Decisions

- Tick rate: fixed at 1s (not configurable). Only used for uptime counter and toast expiry.
- Broadcast channel lag: handle `RecvError::Lagged` by logging and continuing (same as current foreground mode).
- All event types go through the same `mpsc::UnboundedSender<AppEvent>` channel. No separate progress channel.

## 7. Application State

```rust
pub struct AppState {
    // Node (direct access, no IPC)
    pub node: Arc<Node<TailscaleProvider>>,

    // Activity feed
    pub items: Vec<DisplayItem>,
    pub scroll_offset: usize,
    pub auto_scroll: bool,  // true when at bottom, false when user scrolls up

    // Input
    pub input: String,
    pub cursor_pos: usize,
    // Command history
    pub history: Vec<String>,
    pub history_index: Option<usize>,

    // Peer cache (updated by PeerEvents)
    pub peers: Vec<PeerInfo>,

    // UI state
    pub notifications: VecDeque<Toast>,
    pub active_transfer_prompt: Option<TransferPrompt>,

    // Timing
    pub started_at: Instant,
}
```

### Display model

```rust
pub enum DisplayItem {
    System {
        time: DateTime<Local>,
        text: String,
        level: SystemLevel,  // Info, Success, Warning, Error
    },
    PeerEvent {
        time: DateTime<Local>,
        kind: PeerEventKind,  // Joined, Left, Connected, Disconnected
        peer_name: String,
        detail: String,       // e.g. IP address
    },
    ChatOutgoing {
        time: DateTime<Local>,
        to: String,
        text: String,
    },
    ChatIncoming {
        time: DateTime<Local>,
        from: String,
        text: String,
    },
    FileTransfer {
        time: DateTime<Local>,
        direction: Direction,  // Send, Receive
        file_name: String,
        size: u64,
        status: TransferStatus,  // InProgress { percent, speed }, Complete { sha256 }, Failed { reason }
    },
    CommandOutput {
        time: DateTime<Local>,
        command: String,
        lines: Vec<StyledLine>,
    },
}
```

Each item renders with:
- Dimmed timestamp `[HH:MM]`
- Colored indicator per type
- Content text

### Decisions

- Activity feed capped at 10,000 items. Oldest items dropped when cap is reached.
- No collapsible output or search for v1. Keep it simple.

## 8. Slash Command System

### Design principle: simple and focused

Four commands. No modes, no state switches. The TUI is always in the same state — you're looking at the activity feed, the device list is on the right, and the input bar is at the bottom.

### Commands

| Command | Description |
|---------|-------------|
| `/send <message> @device` | Send a chat message to a peer |
| `/broadcast <message>` | Send a message to all online peers |
| `/cp <path> @device[:/dest]` | Send a file or folder to a peer |
| `/exit` | Shutdown and exit |

That's it. Peer activity is always visible in the device panel — no `/peers` command needed. Node status is always in the status bar — no `/status` command needed.

### `@` device autocomplete

Typing `@` anywhere in the input triggers a device picker overlay:

```
  > /send hello @s
  ┌──────────────┐
  │ ● server     │
  └──────────────┘
  > /send hello @server
```

The overlay filters as you type after `@`. Tab or Enter selects. Works in both `/send` and `/cp`.

### `/` command autocomplete

Typing `/` as the first character shows all commands:

```
  > /
  ┌──────────────────────────────┐
  │ /send <msg> @device          │
  │ /broadcast <msg>             │
  │ /cp <path> @device           │
  │ /exit                        │
  └──────────────────────────────┘
```

Continue typing to filter. Tab or Enter selects.

### `/cp` file picker

Typing a path argument in `/cp` supports Tab-completion for filesystem paths. Pressing Tab on an empty path argument opens a file browser overlay using `ratatui-explorer`:

```
  > /cp [Tab]
  ┌─ Select file ─────────────────────┐
  │  📁 Documents/                    │
  │  📁 photos/                       │
  │ ▸ 📄 report.pdf (2.1 MB)         │
  │  📄 notes.txt (4.2 KB)           │
  │  [↑↓ navigate] [Enter] [Esc]     │
  └───────────────────────────────────┘
```

The user can also skip the picker entirely and type the path directly:

```
  > /cp ~/Documents/report.pdf @server
```

Full syntax:

```
  /cp report.pdf @server              # saves to server's ~/Downloads/truffle/
  /cp report.pdf @server:/tmp/        # saves to server's /tmp/
  /cp photos/ @server                 # sends entire folder
```

### `/send` syntax

```
  /send hello @server                 # send "hello" to server
  /send how are things? @server       # message is everything before @device
```

The message is everything between `/send ` and ` @device`. The `@device` must be the last token.

### Plain text (no `/` prefix)

Typing text without a `/` prefix does nothing — shows a hint: `Type / for commands`. All interaction goes through slash commands.

### Dependency

`ratatui-explorer` (v0.3.0, 57k downloads) for the file picker overlay. Implements ratatui's Widget trait, Vim keys + arrows, hidden file toggle.

## 9. Message Receive (New Capability)

Currently `apps/messaging.rs` only has `send_message()` and `broadcast_message()`. There is **no receive handler**.

### TUI receive

The TUI subscribes to `node.subscribe("chat")` in the event collector task. The `NamespacedMessage` from the channel contains `from` (peer ID), `namespace`, and `payload`:

```json
{"type": "text", "text": "Hello!"}
```

The collector resolves the peer ID to a display name and emits `AppEvent::IncomingMessage`.

### Non-TUI receive

In background daemon mode, incoming messages are currently lost (nobody subscribes to "chat"). With the new `subscribe` IPC method (section 11.12), agents can use `truffle recv` to capture messages. But if nobody is listening, messages are dropped.

Message persistence (buffering in daemon, loading history on TUI startup) is a future enhancement — see section 18.

## 10. Notification System

### Primary surface: Activity feed

All events append to `items: Vec<DisplayItem>` and show in the scrollable main area. This is the persistent record.

### Secondary surface: Toast notifications (only when you'd miss them)

Toasts fire ONLY when the user would miss the event — i.e., they're scrolled up in the feed. If the user is at the bottom watching events flow in, no toast (the event is already visible).

Toast appears in the top-right corner for 3-5 seconds:

```
                               ┌────────────────────┐
                               │ server: Hey!       │
                               └────────────────────┘
```

### Priority rules

| Event | Feed | Toast (when scrolled up) | Bell |
|-------|------|--------------------------|------|
| Incoming chat message | Always | Yes | Configurable (off by default) |
| File transfer offer | Always | Yes | Configurable (off by default) |
| File transfer complete | Always | No | No |
| Peer joined | Always | No | No |
| Peer left | Always | No | No |

### Unread indicator

When the user has scrolled up and new messages arrive, the status bar shows: `↓ 3 new`. Pressing End scrolls to bottom and clears it.

### File transfer accept/reject prompt

When a file transfer offer arrives, an inline prompt appears in the feed:

```
  ⬇ server wants to send report.pdf (2.1 MB) → ~/Downloads/truffle/
    [a]ccept  [s]ave as...  [r]eject  [d]on't ask again
```

- `a` — accept, save to default location
- `s` — accept, type a custom save path
- `r` — reject the transfer
- `d` — don't ask again for this peer (auto-accept future transfers, stored in config.toml)

The prompt is an interactive element in the feed. Single keypress to respond — no need to type a slash command. If the user ignores it, the offer times out after 60 seconds and is rejected.

The `[d]on't ask again` option adds the peer to an `auto_accept_peers` list in `~/.config/truffle/config.toml`. This can be undone by editing the config.

### Terminal bell

Off by default. Enable with `bell = true` in `[output]` section of config.toml. When enabled, sends `\x07` on incoming chat messages and file transfer offers.

## 11. Agent Mode (One-Shot Commands)

The one-shot CLI is the agent/script interface. All existing commands are kept. Three new streaming commands are added. Every command gets `--json` and structured exit codes.

### 11.1 Command Surface

**Existing commands (unchanged behavior, enhanced output):**

| Command | Purpose | `--json` output |
|---------|---------|-----------------|
| `truffle up` | Start daemon | `{"status": "started", "pid": 1234}` |
| `truffle down` | Stop daemon | `{"status": "stopped"}` |
| `truffle status` | Node info | `{"version": 1, "node": "my-laptop", "ip": "...", "online": true, ...}` |
| `truffle ls` | List peers | `{"version": 1, "node": "my-laptop", "peers": [...]}` |
| `truffle ping <node>` | Latency | `{"version": 1, "node": "server", "results": [{"seq": 1, "ms": 12.3, ...}], ...}` |
| `truffle send <node> <msg>` | Send message | `{"version": 1, "sent": true, "to": "server"}` |
| `truffle cp <src> <dst>` | File transfer | `{"version": 1, "file": "report.pdf", "bytes": 2100000, "sha256": "...", ...}` |
| `truffle tcp <target>` | Raw TCP test | `{"version": 1, "connected": true, "peer": "server", "port": 5432}` |
| `truffle doctor` | Diagnostics | `{"version": 1, "checks": [{"name": "...", "pass": true, ...}]}` |
| `truffle update` | Self-update | `{"version": 1, "updated": true, "from": "0.3.2", "to": "0.3.3"}` |

**New agent-oriented commands:**

| Command | Purpose | Behavior |
|---------|---------|----------|
| `truffle watch` | Stream mesh events | Long-running. Outputs JSON lines to stdout, one per event. |
| `truffle wait <node>` | Block until peer is online | Exits 0 when peer appears. `--timeout 60` exits 5 on timeout. |
| `truffle recv` | Block for next message | Prints one incoming message, then exits. `--timeout 30` supported. |

### 11.2 `truffle watch` — Event Stream

Streams mesh events as newline-delimited JSON (JSONL) to stdout. Runs until Ctrl+C or `--timeout`.

```bash
$ truffle watch --json
{"type":"peer.joined","peer":"server","ip":"100.64.0.3","connection":"direct","time":"2026-03-28T14:02:00Z"}
{"type":"peer.left","peer":"pi","time":"2026-03-28T14:06:00Z"}
{"type":"message.received","from":"server","text":"deploy complete","time":"2026-03-28T14:10:00Z"}
{"type":"transfer.received","from":"server","file":"report.pdf","bytes":2100000,"time":"2026-03-28T14:15:00Z"}
{"type":"peer.connected","peer":"pi","connection":"relay","time":"2026-03-28T14:20:00Z"}
```

Options:
- `--filter <type>` — only show specific event types (e.g., `--filter message` or `--filter peer`)
- `--timeout <secs>` — exit after N seconds
- Without `--json`, outputs human-readable one-liners

**Implementation:** Requires a new daemon IPC method `subscribe` that keeps the connection open and streams `DaemonNotification` events. The daemon would need to forward peer events and namespace subscriptions over IPC — this extends the current protocol.

### 11.3 `truffle wait <node>` — Peer Wait

Blocks until a peer comes online. Useful for scripting:

```bash
$ truffle wait server --timeout 60 && truffle cp report.pdf server:/tmp/
```

Options:
- `--timeout <secs>` — exit with code 5 (TIMEOUT) if peer doesn't appear
- `--json` — output peer info when found: `{"version": 1, "peer": "server", "ip": "...", "connection": "direct"}`
- Without `--timeout`, waits indefinitely

**Implementation:** Uses `subscribe` IPC method filtered to peer events, exits when matching peer join is received.

### 11.4 `truffle recv` — Receive Message

Blocks and prints the next incoming chat message, then exits. Pipe-friendly:

```bash
$ truffle recv --json --timeout 30
{"version": 1, "from": "server", "from_id": "abc123", "text": "deploy complete", "time": "2026-03-28T14:10:00Z"}
```

Options:
- `--from <node>` — only accept messages from a specific peer
- `--timeout <secs>` — exit with code 5 (TIMEOUT) if no message arrives
- `--json` — structured output (default is plain: `server: deploy complete`)
- Without `--timeout`, waits indefinitely

Can be used in a loop:
```bash
while true; do
  msg=$(truffle recv --json --timeout 300)
  echo "$msg" | process_message.sh
done
```

**Implementation:** Uses `subscribe` IPC method filtered to chat namespace.

### 11.5 Global `--json` Flag

Add to `Cli` struct as a global flag. When active:
- Output is a JSON envelope to stdout with `"version": 1` field
- Progress bars, decorations, colors suppressed
- Errors are structured JSON (see 11.8)

### 11.6 JSON Envelope Format

All `--json` output uses a versioned envelope:

```json
{
  "version": 1,
  "node": "my-laptop",
  ...command-specific fields...
}
```

The `version` field allows future breaking changes to the schema. Agents can check `version` and fail gracefully on unexpected versions. Error responses use the same envelope:

```json
{
  "version": 1,
  "error": {
    "code": 3,
    "type": "not_found",
    "message": "Peer 'serverx' not found",
    "suggestion": "Did you mean 'server'? Run 'truffle ls' to see available peers."
  }
}
```

### 11.7 Structured Exit Codes

```
0   SUCCESS
1   ERROR (general / internal)
2   USAGE (bad arguments / syntax)
3   NOT_FOUND (peer not found)
4   NOT_ONLINE (daemon not running / node offline)
5   TIMEOUT
6   TRANSFER_FAILED
```

### 11.8 stderr/stdout Discipline

- `--json` mode: JSON envelope to stdout, progress/logs to stderr
- Default mode: formatted output to stdout, errors to stderr
- `--quiet` mode: suppress all non-essential output, only result to stdout

### 11.9 NO_COLOR Support

Check `std::env::var("NO_COLOR")` in `output::init_color()`. Takes precedence over `--color auto`. Follows the [no-color.org](https://no-color.org) standard.

### 11.10 CLI Syntax

The one-shot CLI syntax keeps the existing pattern: **target first, then payload** (like `ssh`, `scp`, `ping`):

```
truffle send <node> <message>    # target first
truffle cp <src> <dst>           # scp-style
truffle ping <node>              # target first
```

This is deliberately different from the TUI's `/send <msg> @device` syntax. CLI convention is target-first; TUI convention is message-first with `@` mentions. Agents learn the syntax from `--help`.

### 11.11 File Transfer Behavior

The sender side (`truffle cp`) is always non-interactive — it sends and reports progress/result.

What happens on the receiver depends on their mode:
- **TUI running:** Receiver sees accept/reject prompt (section 10)
- **Daemon running (no TUI):** Auto-accept based on config (`auto_accept_peers` list or global `auto_accept = true` in config.toml)
- **Nothing running:** Transfer fails, sender gets exit code 4 (NOT_ONLINE)

### 11.12 Daemon Protocol Extensions

The three new commands (`watch`, `wait`, `recv`) require extending the daemon IPC protocol with a `subscribe` method:

```
Method: "subscribe"
Params: {
  "events": ["peer", "message", "transfer"],   // which event types
  "filter": { "peer": "server" }               // optional filter
}
Response: (none — connection stays open)
Notifications: stream of events as DaemonNotification messages
```

The client keeps the IPC connection open and reads notifications in a loop (same pattern as `request_with_notifications` but without a terminal response). The daemon spawns a forwarder task that bridges its internal broadcast channels to the IPC connection.

This is the only daemon protocol change in this RFC.

## 12. Entry Point Changes

```rust
// main.rs — new dispatch logic

match cli.command {
    None if std::io::stdin().is_terminal() => {
        // Bare `truffle` on a TTY → launch TUI
        tui::run(&config).await
    }
    None => {
        // Bare `truffle` piped → show status (backward compat for scripts)
        commands::status::run(&config, cli.json, false).await
    }
    Some(command) => {
        // Explicit subcommand → one-shot dispatch (unchanged)
        match command { ... }
    }
}
```

TTY detection ensures:
- `truffle` in a terminal → TUI
- `truffle | jq .` or `echo | truffle` → status output (no TUI)
- `truffle status` → always one-shot, regardless of TTY

## 13. New Module Structure

```
src/
  main.rs               -- (modified) route bare command to TUI
  tui/
    mod.rs              -- run() entry, terminal init/restore, main loop, panic hook
    app.rs              -- AppState, handle_event(), display model types
    event.rs            -- AppEvent enum, spawn_event_collectors()
    ui/
      mod.rs            -- Top-level render() with Layout constraints
      status_bar.rs     -- 1-line node identity + stats
      activity_feed.rs  -- Scrollable Vec<DisplayItem> renderer
      input_bar.rs      -- Text input + @device autocomplete + /command autocomplete
      device_panel.rs   -- Always-visible peer list (right side)
      toast.rs          -- Ephemeral notification overlay (only when scrolled up)
      file_picker.rs    -- File browser overlay (wraps ratatui-explorer)
      transfer_prompt.rs -- Accept/reject/save-as inline prompt
    commands/
      mod.rs            -- Command registry, parse(), dispatch
      send.rs           -- /send <msg> @device
      broadcast.rs      -- /broadcast <msg>
      cp.rs             -- /cp <path> @device[:/dest] (with progress tracking)
      exit.rs           -- /exit
      quit.rs           -- /quit, /q
  exit_codes.rs         -- (new) structured exit codes
  output.rs             -- (modified) add NO_COLOR, extract Span helpers
```

## 14. Files Reused Unchanged

| File | What TUI reuses |
|------|-----------------|
| `daemon/server.rs` | `DaemonServer::start()`, `.node()`, `.subscribe_peer_events()`, `.run()` |
| `apps/file_transfer/receive.rs` | `spawn_receive_handler()` — spawned at TUI startup |
| `apps/messaging.rs` | `send_message()` — called by `/send` |
| `apps/diagnostics.rs` | `run_diagnostics()` — called by `/doctor` |
| `config.rs` | `TruffleConfig` — loaded at startup |
| `resolve.rs` | `NameResolver` — used for peer name autocomplete |
| `output.rs` | `format_latency()`, `format_uptime()`, `format_bytes()`, `format_speed()` |

## 15. Implementation Phases

Each phase produces a working, runnable result. Phases are sequential within TUI and Agent tracks, but the two tracks are independent and can be interleaved.

### Track A: TUI

#### Phase A1: Skeleton — terminal + daemon + static layout
**Goal:** `truffle` launches a TUI, connects to mesh, shows static content, exits cleanly.

- Add ratatui 0.30, crossterm 0.29 (`event-stream`), tui-textarea 0.7 to Cargo.toml
- Create `src/tui/mod.rs`: `pub async fn run(config: &TruffleConfig) -> Result<(), String>`
  - Initialize terminal (alternate screen, raw mode, mouse capture)
  - Set up panic hook to restore terminal on crash
  - Start `DaemonServer` in-process (reuse `commands/up.rs` foreground pattern)
  - Spawn `server.run()` as background task (IPC accept loop)
  - Enter render loop with static placeholder content
  - Handle Ctrl+C → restore terminal, shutdown daemon, exit
- Create `src/tui/app.rs`: `AppState` with node ref, `Vec<DisplayItem>`, peer list, started_at
- Create `src/tui/ui/mod.rs`: top-level `render()` with 3-zone Layout:
  - Status bar (1 line, top): node name, "connecting..." placeholder
  - Activity feed (center): empty or "Welcome to truffle" message
  - Input bar (1 line, bottom): `─` separator + `> ` prompt (non-functional yet)
  - Device panel (right, 20 chars): empty placeholder with "DEVICES" header
- Modify `src/main.rs`: route bare `truffle` (TTY) to `tui::run()`, non-TTY to `commands::status::run()`
- **Files created:** `tui/mod.rs`, `tui/app.rs`, `tui/event.rs`, `tui/ui/mod.rs`, `tui/ui/status_bar.rs`, `tui/ui/activity_feed.rs`, `tui/ui/input_bar.rs`, `tui/ui/device_panel.rs`
- **Files modified:** `main.rs`, `Cargo.toml`
- **Test:** Run `truffle` → TUI appears with node info. `truffle status` from another terminal works (IPC). Ctrl+C exits cleanly. Kill with SIGTERM restores terminal.
- **Risk addressed:** This phase proves ratatui + tokio + DaemonServer integration works. If anything is fundamentally broken, we find out here.

#### Phase A2: Live events — peer activity + device panel
**Goal:** Peers appear in the device panel and the activity feed in real-time.

- Create event collector (`tui/event.rs`): spawn 3 tasks initially:
  - Crossterm EventStream → `AppEvent::Key` / `AppEvent::Resize`
  - `server.subscribe_peer_events()` → `AppEvent::PeerEvent`
  - `tokio::time::interval(1s)` → `AppEvent::Tick`
- Implement `app.handle_event()` for PeerEvent: update `app.peers` cache, append `DisplayItem::PeerEvent` to feed
- Implement device panel (`tui/ui/device_panel.rs`): render `app.peers` with `●`/`○` indicators
- Implement activity feed (`tui/ui/activity_feed.rs`): render `Vec<DisplayItem>` with timestamps
- Update status bar: show live peer count, uptime (updates on Tick), connection status
- Render the startup banner (block-letter TRUFFLE logo) as the first items in the feed
- **Files created:** (event.rs was stubbed in A1, now fully implemented)
- **Test:** Run `truffle` on two machines on the same tailnet. See peer appear in device panel and "● peer joined" in feed. Disconnect the other machine, see "○ peer left".

#### Phase A3: Input bar + slash commands (/send, /broadcast, /exit)
**Goal:** Type `/send hello @server` and the message appears on both sides.

- Implement input bar (`tui/ui/input_bar.rs`): text editing with cursor, backspace, delete, Home/End
- Command parser (`tui/commands/mod.rs`): split `/command args`, registry lookup
- Implement `/send <msg> @device`: parse message + `@device` token, call `apps::messaging::send_message()`, append `DisplayItem::ChatOutgoing` to feed
- Implement `/broadcast <msg>`: call `apps::messaging::broadcast_message()`, append to feed
- Implement `/exit`: trigger shutdown
- Add chat message event collector: `node.subscribe("chat")` → `AppEvent::IncomingMessage`
- Implement `DisplayItem::ChatIncoming` rendering with sender name resolution
- Implement message grouping: consecutive messages from same sender collapse (indented, no repeated name)
- Plain text without `/` shows hint: "Type / for commands"
- **Files created:** `tui/commands/mod.rs`, `tui/commands/send.rs`, `tui/commands/broadcast.rs`, `tui/commands/exit.rs`
- **Test:** Two TUI instances. `/send hello @server` on one → message appears on both. `/broadcast hey everyone` → all peers see it.

#### Phase A4: `@` device autocomplete + `/` command autocomplete
**Goal:** Typing `@` or `/` shows picker overlays.

- Implement `@` autocomplete: detect `@` in input, show device picker overlay above input bar, filter as user types, Tab/Enter to select
- Implement `/` autocomplete: detect `/` at start of input, show command list overlay, filter as user types
- Command history: Up/Down arrows cycle through previous commands, stored in `Vec<String>`
- Scrollback: PageUp/PageDown scroll the activity feed, auto_scroll disengages when scrolled up, re-engages on End or new command
- Unread indicator in status bar: `↓ N new` when scrolled up and new events arrive
- **Test:** Type `@s` → overlay shows "● server". Tab completes. Type `/` → shows all 4 commands. Up arrow recalls last command. PageUp scrolls feed.

#### Phase A5: File transfer — `/cp` + receive prompt
**Goal:** `/cp report.pdf @server` sends with progress. Receiver sees accept/reject prompt.

- Spawn `receive::spawn_receive_handler()` at TUI startup (same as current foreground mode)
- Add file transfer event collector: `node.subscribe("ft")` → `AppEvent::FileTransferEvent`
- Implement `/cp <path> @device[:/dest]`: parse path + device, call file upload through daemon handler
- Render `DisplayItem::FileTransfer` with inline progress bar (updates in-place on each event)
- Implement transfer prompt (`tui/ui/transfer_prompt.rs`): render `[a]ccept [s]ave as... [r]eject [d]on't ask again` in feed on incoming offer
- Handle single-keypress responses (a/s/r/d) when a transfer prompt is active
- `[d]on't ask again` persists peer to `auto_accept_peers` in config.toml
- Timeout: 60s auto-reject if no response
- **Files created:** `tui/commands/cp.rs`, `tui/ui/transfer_prompt.rs`
- **Test:** `/cp test.txt @server` → progress bar, completion message with SHA-256. On receiver: prompt appears, press `a`, file saved.

#### Phase A6: File picker + toast notifications + polish
**Goal:** Tab-completion for file paths, toast overlay, final polish.

- Add `ratatui-explorer` dependency
- Implement file picker overlay (`tui/ui/file_picker.rs`): Tab in `/cp ` opens browser, Esc closes, Enter selects
- Implement toast overlay (`tui/ui/toast.rs`): appears top-right only when scrolled up, 3-5s auto-dismiss
- Terminal bell: send `\x07` on incoming message/transfer if `bell = true` in config
- Command history persistence: save to `~/.config/truffle/history`, load on startup
- Activity feed cap: keep last 10,000 items, drop oldest
- **Files created:** `tui/ui/file_picker.rs`, `tui/ui/toast.rs`
- **Test:** `/cp [Tab]` → file browser appears. Receive a message while scrolled up → toast appears. History persists across restarts.

### Track B: Agent Mode

#### Phase B1: `--json` + exit codes + error format
**Goal:** Every one-shot command supports `--json` with versioned envelope output.

- Add global `--json` flag to `Cli` struct in main.rs
- Create `src/exit_codes.rs` with constants (SUCCESS through TRANSFER_FAILED)
- Create `src/json_output.rs`: helper for building versioned JSON envelopes, structured errors
- Wire `--json` through all 10 existing commands:
  - `status`, `ls`: already have `--json`, refactor to use versioned envelope
  - `ping`, `send`, `cp`, `tcp`, `doctor`, `up`, `down`, `update`: add JSON output path
- Wire structured exit codes through all error paths
- Add NO_COLOR check to `output::init_color()`
- stderr/stdout discipline: when `--json`, all non-data output goes to stderr
- **Files created:** `exit_codes.rs`, `json_output.rs`
- **Files modified:** `main.rs`, all `commands/*.rs`, `output.rs`
- **Test:** `truffle ls --json | jq .` → valid JSON with `"version": 1`. `truffle send nonexistent hello; echo $?` → exit code 3. `NO_COLOR=1 truffle status` → no ANSI codes.

#### Phase B2: Daemon `subscribe` protocol
**Goal:** Daemon supports long-lived streaming subscriptions over IPC.

- Add `subscribe` method to daemon protocol (`daemon/protocol.rs`)
- Implement handler in `daemon/handler.rs`: spawn forwarder tasks that bridge `node.subscribe()` broadcast channels to the IPC connection as `DaemonNotification` messages
- Handle client disconnect: drop forwarder tasks on connection close
- Handle event filtering by type and optional peer filter
- **Files modified:** `daemon/protocol.rs`, `daemon/handler.rs`, `daemon/server.rs`
- **Test:** Manual test with a raw IPC client sending `{"jsonrpc":"2.0","id":1,"method":"subscribe","params":{"events":["peer"]}}` and observing streamed notifications.

#### Phase B3: `truffle watch` + `truffle wait` + `truffle recv`
**Goal:** Three new streaming commands work end-to-end.

- Add `Watch`, `Wait`, `Recv` variants to `Commands` enum in main.rs
- Implement `commands/watch.rs`: connect to daemon with `subscribe`, print events as JSONL. Options: `--filter`, `--timeout`, `--json` (human-readable without `--json`)
- Implement `commands/wait.rs`: connect with `subscribe` filtered to peer events, exit 0 on match. Options: `--timeout`
- Implement `commands/recv.rs`: connect with `subscribe` filtered to chat namespace, print first message, exit. Options: `--from`, `--timeout`, `--json`
- **Files created:** `commands/watch.rs`, `commands/wait.rs`, `commands/recv.rs`
- **Files modified:** `main.rs` (add command variants), `commands/mod.rs`
- **Test:** `truffle watch --json --filter peer` in one terminal, join mesh from another → events stream. `truffle wait server --timeout 10 && echo "server is online"`. `truffle recv --json --from server --timeout 30`.

### Phase Order

Recommended sequence:

```
A1 → A2 → A3 → A4 → A5 → A6    (TUI track)
         ↗
B1 → B2 → B3                     (Agent track)
```

B1 (--json + exit codes) can start any time — it's independent of the TUI. B2 (subscribe protocol) should come after A2 since it reuses the same event forwarding patterns. B3 depends on B2.

Within the TUI track, A1-A3 is the critical path — that gets you a working chat TUI. A4-A6 is polish.

A practical order if working solo:

1. **A1** — get the TUI rendering (de-risk ratatui integration)
2. **A2** — live peer events (proves the event loop architecture)
3. **A3** — chat messaging (first real feature, two nodes talking)
4. **B1** — agent mode (independent, good context switch from TUI work)
5. **A4** — autocomplete + scrollback (polish the input experience)
6. **A5** — file transfer (most complex TUI feature)
7. **B2** — subscribe protocol (needed for watch/wait/recv)
8. **B3** — streaming commands (agent features)
9. **A6** — file picker + toasts + final polish

## 16. Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| Terminal raw mode conflicts with daemon IPC | No conflict: IPC uses Unix sockets, not stdout |
| Panic leaves terminal in broken state | Panic hook restores terminal (disable raw mode, leave alternate screen) |
| Broadcast channel lag | Handle `RecvError::Lagged` gracefully (log, continue) — same as current foreground mode |
| Large dependency addition (ratatui) | ratatui is well-maintained, widely used, and compile-time-only cost is modest |
| TUI blocks daemon shutdown signal | TUI owns signal handlers, coordinates both ratatui cleanup and daemon shutdown |

## 17. Non-Goals (for this RFC)

- **Message persistence / history** — Messages are not stored. TUI shows only what happens during its session.
- **Multi-window / split panes** — Single main area + sidebar. No arbitrary splits.
- **Plugin / extension system** — Slash commands are built-in only.
- **Remote TUI** — TUI only runs locally, not over SSH (though it technically works over SSH since it's a standard terminal app).
- **GUI** — This is terminal-only.

## 18. Future Possibilities

- **Message persistence**: Daemon could buffer messages in a SQLite DB, TUI loads history on startup.
- **Rooms / channels**: Namespace-based chat rooms instead of direct peer-to-peer only.
- **Custom slash commands**: User-defined commands via config or scripts.
- **Themes**: Configurable color schemes (dark/light/custom).
- **Mouse support**: Click on peers to start chat, click on transfers to see details.
- **MCP server integration**: Expose truffle mesh as an MCP server so AI agents can send messages, transfer files, and discover peers via the standard protocol.

## 19. Implementation Strategy — Agent Teams

The implementation can be parallelized across subagents using git worktrees for isolation. Each agent works on a self-contained phase, and results are merged sequentially.

### Solo phases (must be sequential, main context)

**A1 (TUI skeleton)** must be done first in the main context — it creates the `tui/` module structure that everything else depends on. This is also the highest-risk phase (ratatui + tokio + DaemonServer integration) so it benefits from close attention.

**A2 (live events)** should also be in main context — it establishes the event loop architecture that A3-A6 all build on.

### Parallelizable after A2

Once A2 is merged, several phases can run in parallel using worktree isolation:

```
A2 merged
  ├─→ Agent 1 (worktree): A3 — chat messaging (/send, /broadcast, message receive)
  ├─→ Agent 2 (worktree): B1 — agent mode (--json on all commands, exit codes, NO_COLOR)
  └─→ main context: review + merge A3, then B1
```

After A3 + B1 merged:

```
  ├─→ Agent 1 (worktree): A4 — autocomplete + scrollback
  ├─→ Agent 2 (worktree): B2 — daemon subscribe protocol
  └─→ main context: review + merge
```

After A4 + B2 merged:

```
  ├─→ Agent 1 (worktree): A5 — file transfer UI + accept/reject prompt
  ├─→ Agent 2 (worktree): B3 — watch/wait/recv commands
  └─→ main context: review + merge
```

Final phase (sequential):

```
  └─→ A6 — file picker + toasts + polish (depends on A5)
```

### Agent instructions per phase

Each agent should receive:
1. This RFC as context (the full design document)
2. The specific phase description from section 15
3. Instructions to read the existing code it depends on before writing
4. The test criteria for the phase

### What NOT to parallelize

- A1 and A2 touch foundational structures (`tui/mod.rs`, `tui/app.rs`, `tui/event.rs`) that every subsequent phase depends on. These must be sequential.
- A5 (file transfer UI) depends on A3 (messaging patterns) and A4 (autocomplete for `@device`). Don't start A5 until A3 and A4 are merged.
- B3 (watch/wait/recv) depends on B2 (subscribe protocol). Sequential within Track B.

### Merge order

```
1. A1  (skeleton)           — sequential, main
2. A2  (live events)        — sequential, main
3. A3  (chat)    ║ B1 (--json)  — parallel worktrees
4. A4  (autocomplete) ║ B2 (subscribe) — parallel worktrees
5. A5  (file transfer) ║ B3 (watch/wait/recv) — parallel worktrees
6. A6  (polish)             — sequential, main
```

Total: 6 merge rounds. With parallelization, 2 phases complete per round in steps 3-5.
