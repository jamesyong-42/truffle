# Truffle

[![CI](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml/badge.svg)](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/jamesyong-42/truffle?label=latest)](https://github.com/jamesyong-42/truffle/releases/latest)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Mesh networking for your devices, built on Tailscale.**

Truffle lets your devices discover each other, exchange messages, and transfer files over Tailscale's encrypted WireGuard tunnels. No central server. Run `truffle` to launch an interactive TUI, or use one-shot commands for scripts and AI agents.

```
╭─── truffle ──────────────────────────────────────────────────╮
│                                         │ Devices                │
│  ▀█▀ █▀█ █ █ █▀▀ █▀▀ █   █▀▀          │ ● server (direct)      │
│   █  █▀▄ █ █ █▀  █▀  █   █▀           │ ● laptop (relay)       │
│   █  █ █ ▀▄▀ █   █   █▄▄ █▄▄          │ ○ pi (offline)         │
│                                         │                        │
│  mesh networking for your devices       │                        │
│  jamess-mbp · 100.64.0.5               │                        │
│  ● online · 2 peers · 15m              │                        │
╰───────────────────────────────────────────────────────────────╯
  14:02  ● server joined (direct)
  14:03  You → server: Hello!
  14:03  server → You: Hey there!
  14:05  ⬇ report.pdf from server ████████████████ 100% ✓
─────────────────────────────────────────────────────────────────
❯ /send how are things? @server
─────────────────────────────────────────────────────────────────
  truffle · ● online · 2 peers
```

## Install

```bash
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh
```

Supports macOS (arm64/x64), Linux (x64/arm64), and Windows (x64). Linux binaries are statically linked (musl) — works on any distro.

## Quick Start

### Interactive TUI

```bash
truffle                           # launch the interactive TUI
```

First run shows an onboarding wizard with smart device naming and Tailscale auth. After setup, the TUI shows a live activity feed, device panel, and slash commands:

| Command | Description |
|---------|-------------|
| `/send <msg> @device` | Send a message (`@` autocompletes devices) |
| `/broadcast <msg>` | Message all online peers |
| `/cp <path> @device` | Send a file (Tab opens file picker) |
| `/exit` | Quit |

Incoming file transfers show an accept/reject modal dialog.

### One-Shot Commands (for scripts & agents)

```bash
truffle up                        # start daemon in background
truffle ls                        # see who's online
truffle status                    # node identity, IP, uptime
truffle ping laptop               # check latency
truffle send server "hello"       # send a message
truffle cp file.txt server:/tmp/  # copy files (SHA-256 verified)
truffle tcp server:5432           # raw TCP (like netcat)
truffle doctor                    # diagnose connectivity
truffle down                      # graceful shutdown
truffle update                    # self-update to latest release
```

### Agent-Friendly Streaming

```bash
truffle watch --json              # stream mesh events as JSONL
truffle wait server --timeout 60  # block until peer comes online
truffle recv --json --from server # block for next message
```

All commands support `--json` for structured output with versioned envelopes.

## Features

- **Interactive TUI** — Claude Code-inspired terminal UI with live activity feed, device panel, slash commands, file picker, autocomplete
- **Chat messaging** — send/broadcast with `@device` mentions, message grouping for consecutive messages
- **File transfer** — SHA-256 verified, progress bars, interactive accept/reject modal, streaming to disk (constant memory)
- **Peer discovery** — automatic via Tailscale WatchIPNBus, ~30s offline detection
- **Smart naming** — auto-generates clean device names from OS hostname (`Jamess-MBP-6.ts.net lan` → `jamess-mbp`)
- **Agent mode** — `--json` on all commands, structured exit codes, `NO_COLOR`, `truffle watch/wait/recv` for scripting
- **Cross-platform** — macOS, Linux (musl static), Windows
- **First-run onboarding** — name picker + Tailscale auth wizard

## Architecture

Layered architecture (RFC 012) with file transfer as a first-class core feature (RFC 014):

```
Layer 7: Applications   ── CLI commands, TUI, daemon handler
         Node API       ── 16-method public API + FileTransfer manager
Layer 6: Envelope       ── namespace-routed message framing (JSON codec)
Layer 5: Session        ── PeerRegistry, lazy WS connections, event broadcast
Layer 4: Transport      ── WebSocket, TCP, UDP, QUIC transports
Layer 3: Network        ── TailscaleProvider (peer discovery via WatchIPNBus)
         Go Sidecar     ── tsnet integration, WireGuard tunnels
```

**Key design principles:**
- **Layer 3 is the source of truth for peers** — peers exist because Tailscale reports them, not because of transport connections
- **Lazy connections** — WebSocket connections are established on first `send()`, not eagerly
- **Layer separation** — each layer has a trait boundary; e.g., `NetworkProvider` can be swapped without touching upper layers
- **File transfer uses Node primitives** — signaling over WS namespace `"ft"`, data over raw TCP, no special infrastructure

### File Transfer (RFC 014)

The `file_transfer` module in `truffle-core` provides:

```rust
let ft = node.file_transfer();

// Send a file
ft.send_file("peer-id", "/path/to/file.txt", "/dest/").await?;

// Pull a file from a remote peer
ft.pull_file("peer-id", "/remote/file.txt", "/local/path.txt").await?;

// Receive with interactive accept/reject
let mut offers = ft.offer_channel(node.clone()).await;
while let Some((offer, responder)) = offers.recv().await {
    responder.accept("/save/path");  // or responder.reject("reason")
}

// Or auto-accept everything
ft.auto_accept(node.clone(), "~/Downloads/truffle/").await;

// Subscribe to transfer lifecycle events
let mut events = ft.subscribe();
```

## Crates & Packages

| Crate / Package | Status | Description |
|-----------------|--------|-------------|
| [`truffle-core`](crates/truffle-core) | **Active** | Rust library — Layers 3-6, Node API, file transfer (~12k LOC) |
| [`truffle-cli`](crates/truffle-cli) | **Active** | CLI + TUI — 13 one-shot commands, interactive terminal UI (~10k LOC) |
| [`sidecar-slim`](packages/sidecar-slim/) | **Active** | Go sidecar — tsnet, WatchIPNBus, TCP bridge (~1.9k LOC) |
| [`truffle-napi`](crates/truffle-napi) | Bindings | NAPI-RS native addon for Node.js (rewritten in RFC 015) |
| [`truffle-tauri-plugin`](crates/truffle-tauri-plugin) | Bindings | Tauri v2 plugin for desktop apps (rewritten in RFC 015) |

**npm:** `@vibecook/truffle` + platform-specific sidecar packages

## Development

```bash
cargo build --workspace       # build all Rust crates
cargo test --workspace        # run tests (~187 tests)
```

### Prerequisites

- **Rust** >= 1.86
- **Go** >= 1.22 (for building the sidecar)
- **Tailscale** installed and authenticated

## RFCs

| RFC | Status | Description |
|-----|--------|-------------|
| [RFC 012](docs/rfcs/012-layered-architecture-redesign.md) | Implemented | Clean layered architecture redesign |
| [RFC 013](docs/rfcs/013-tui-redesign.md) | Implemented | Dual-mode TUI + agent CLI redesign |
| [RFC 014](docs/rfcs/014-file-transfer-core.md) | Implemented | File transfer as core feature with accept/reject API |
| [RFC 015](docs/rfcs/015-binding-rewrite.md) | Implemented | NAPI-RS and Tauri plugin rewrite |

## Documentation

- [GitHub Pages](https://jamesyong-42.github.io/truffle/) — install scripts and docs
- [Getting Started](docs/guide/getting-started.md)
- [CLI Reference](docs/guide/cli.md)
- [Architecture](docs/guide/architecture.md)

## License

[MIT](LICENSE)
