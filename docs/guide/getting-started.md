# Getting Started

## Prerequisites

- **Tailscale** installed and authenticated on each device you want to connect
- That's it. Truffle handles the rest.

## Install

::: code-group

```sh [macOS / Linux]
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh
```

```powershell [Windows]
iwr -useb https://jamesyong-42.github.io/truffle/install.ps1 | iex
```

:::

The installer downloads the `truffle` CLI and the `sidecar-slim` binary, places them in `~/.config/truffle/bin`, and adds them to your PATH. Linux binaries are statically linked — works on any distro.

## Quick Start

### 1. Launch the TUI

```sh
truffle
```

On first run, you'll see an onboarding wizard:

```
╭─── truffle ── first time setup ──────────────────────────╮
│                                                          │
│         ▀█▀ █▀█ █ █ █▀▀ █▀▀ █   █▀▀                   │
│          █  █▀▄ █ █ █▀  █▀  █   █▀                    │
│          █  █ █ ▀▄▀ █   █   █▄▄ █▄▄                   │
│                                                          │
│   Name this device:                                      │
│   ❯ jamess-mbp                                           │
│                                                          │
╰──────────────────────────────────────────────────────────╯
```

Accept the auto-generated name or type your own, then authenticate with Tailscale. After setup, the TUI launches with a live activity feed showing peer events, messages, and file transfers.

### 2. Send a message

Type in the TUI:

```
/send hello @server
```

The `@` triggers device autocomplete. The message appears on both sides instantly.

### 3. Send a file

```
/cp report.pdf @server
```

Press Tab to open the file picker, or type the path directly. The other device sees an accept/reject dialog:

```
╭─── incoming file ────────────────────────────────────╮
│  server wants to send you a file:                    │
│  📄 report.pdf  (2.1 MB)                            │
│  [a] Accept  [s] Save as  [r] Reject                │
│                    [d] Don't ask from server         │
╰──────────────────────────────────────────────────────╯
```

### 4. Broadcast to all peers

```
/broadcast deploy starting in 5 minutes
```

## One-Shot Commands

For scripts, CI, and AI agents, use subcommands instead of the TUI:

```sh
truffle up                        # start daemon
truffle ls --json                 # list peers as JSON
truffle send server "hello"       # send a message
truffle cp file.txt server:/tmp/  # copy a file
truffle watch --json              # stream events as JSONL
truffle wait server --timeout 60  # block until peer online
truffle recv --json               # block for next message
truffle down                      # stop daemon
```

All commands support `--json` for structured output with versioned envelopes:

```json
{"version": 1, "node": "jamess-mbp", "peers": [...]}
```

## How It Works

1. `truffle` starts the Go sidecar, which joins your Tailscale network via `tsnet`
2. Peers are discovered automatically via Tailscale's WatchIPNBus
3. WebSocket connections are established lazily on first message
4. Messages flow through the 7-layer stack: TUI → Node API → Envelope → Session → Transport → Network → Sidecar
5. File transfers use WS for signaling (offer/accept) and raw TCP for data, with SHA-256 verification

## Next Steps

- [CLI Reference](/guide/cli) — full command reference
- [Installation](/guide/install) — detailed install options
- [Architecture](/guide/architecture) — how the layers work
