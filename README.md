# Truffle

[![CI](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml/badge.svg)](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/jamesyong-42/truffle?label=latest)](https://github.com/jamesyong-42/truffle/releases/latest)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Mesh networking for local-first apps and devices, built on Tailscale.**

Truffle helps devices on the same tailnet discover each other, exchange messages, sync state, and transfer files — without running your own central server.

It works as:

- a **CLI + terminal UI** for humans
- a **Rust library** for native apps
- a **Node native addon** and **Tauri plugin** for desktop apps

## Why Truffle

- **Built on Tailscale** — use your existing tailnet for identity, connectivity, and encryption
- **Human-friendly** — interactive TUI, onboarding, slash commands, file picker, progress UI
- **App-friendly** — typed messaging, request/reply, synced store, file transfer primitives
- **Agent-friendly** — `--json`, streaming events, and commands that compose well in scripts

## What it looks like

```text
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
  14:05  ⬇ report.pdf from server ████████████████ 100% ✓
─────────────────────────────────────────────────────────────────
❯ /send how are things? @server
─────────────────────────────────────────────────────────────────
  truffle · ● online · 2 peers
```

## Install

> Requires Tailscale to be installed and authenticated on the devices you want to connect.

```bash
# macOS / Linux
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh

# Windows (PowerShell)
iwr -useb https://jamesyong-42.github.io/truffle/install.ps1 | iex

# Homebrew
brew install jamesyong-42/tap/truffle
```

Supports macOS (arm64/x64), Linux (x64/arm64), and Windows (x64).

## Quick start

```bash
truffle                           # launch the interactive TUI
truffle up                        # start the background daemon
truffle ls                        # list peers on your tailnet
truffle send server "hello"       # send a message
truffle cp report.pdf server:/tmp/  # send a file
truffle watch --json              # stream mesh events as JSONL
```

On first run, Truffle guides you through device naming and Tailscale auth, then drops you into a live mesh view.

## What you can build with it

- **Device-to-device messaging**
- **Verified file transfer**
- **Request/reply workflows**
- **Shared state with synced stores**
- **Local-first desktop tools on top of your tailnet**

## Packages

- [`crates/truffle-cli`](crates/truffle-cli) — interactive CLI + TUI
- [`crates/truffle`](crates/truffle) / [`crates/truffle-core`](crates/truffle-core) — Rust API
- [`crates/truffle-napi`](crates/truffle-napi) — Node.js native addon (`@vibecook/truffle-native`)
- [`crates/truffle-tauri-plugin`](crates/truffle-tauri-plugin) — Tauri v2 plugin
- [`packages/sidecar-slim`](packages/sidecar-slim) — Go sidecar for Tailscale integration

## Learn more

- [Getting Started](docs/guide/getting-started.md)
- [CLI Reference](docs/guide/cli.md)
- [Architecture](docs/guide/architecture.md)
- [API docs](docs/api/index.md)
- [RFCs](docs/rfcs/)

## Development

```bash
cargo build --workspace --exclude truffle-tauri-plugin --exclude truffle-napi
cargo test --workspace --exclude truffle-tauri-plugin --exclude truffle-napi
```

## License

[MIT](LICENSE)
