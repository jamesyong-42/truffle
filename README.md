# Truffle

[![CI](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml/badge.svg)](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/jamesyong-42/truffle?label=latest)](https://github.com/jamesyong-42/truffle/releases/latest)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**P2P mesh networking for your devices, built on Tailscale.**

Truffle gives your devices automatic discovery and direct peer-to-peer communication over Tailscale's encrypted WireGuard tunnels. No coordinator, no election, no central server -- every node connects directly to every other node. The core is written in Rust with a clean layered architecture (RFC 012), a product-grade CLI, and a thin Go sidecar (~1.1k LOC) for Tailscale `tsnet` integration. Node.js bindings (NAPI-RS) and Tauri plugin are available but pending update to the new Node API.

## Install

```bash
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh
```

Supports macOS (arm64/x64), Linux (x64/arm64), and Windows (x64).

## Quick Start

```bash
truffle up                        # start your node
truffle ls                        # see who's online
truffle ping laptop               # check latency
truffle cp file.txt server:/tmp/  # copy files between devices
truffle send server "hello"       # send a message
truffle tcp server:5432           # raw TCP connection
truffle doctor                    # diagnostics
truffle down                      # stop your node
```

## What Works

| Command | Status | Description |
|---------|--------|-------------|
| `truffle up/down` | **Working** | Start/stop node, auto-discover peers, establish mesh connections |
| `truffle status` | **Working** | Show node status, IP, DNS, uptime, peer count |
| `truffle ls` | **Working** | List peers with online status and connection type |
| `truffle ping` | **Working** | Connectivity check via Tailscale (direct/relay), latency stats |
| `truffle cp` | **Working** | File transfer over mesh (HTTP PUT, SHA-256 verified, resume support) |
| `truffle send` | **Working** | Send messages via mesh WebSocket connections |
| `truffle tcp` | **Working** | Raw TCP connection through Tailscale (like netcat) |
| `truffle doctor` | **Working** | Diagnose Tailscale, sidecar, mesh, key expiry |
| `truffle update` | **Working** | Self-update from GitHub releases |
| `truffle completion` | **Working** | Shell completions (bash/zsh/fish) |
| `truffle ws` | Partial | WebSocket connection (handler returns metadata, no interactive streaming yet) |
| `truffle chat` | Partial | Terminal chat (CLI UI works, daemon handler is metadata-only) |
| `truffle proxy` | Partial | Port forwarding (handler scaffolded, needs data plane wiring) |
| `truffle expose` | Partial | Share local port (handler scaffolded, needs data plane wiring) |

## Architecture

Clean layered architecture (RFC 012), built bottom-up with trait boundaries at each layer:

```
Layer 7: Applications  -- file transfer, messaging, diagnostics (pure consumers of Node API)
Layer 6: Envelope      -- namespace-routed message framing
Layer 5: Session       -- PeerRegistry, lazy connections, message routing
Layer 4: Transport     -- WebSocket, TCP, UDP, QUIC protocols
Layer 3: Network       -- TailscaleProvider, WatchIPNBus peer discovery
         Go Sidecar    -- tsnet integration, encrypted WireGuard tunnels
```

The `Node` struct is the single public entry point (~12 methods). Applications never import from lower layers directly. Peer discovery works via Tailscale's WatchIPNBus -- peers are known before any transport connections are established. Messages are sent point-to-point or fan-out broadcast with namespace-based routing.

## Crates & Packages

| Crate / Package | Description |
|-----------------|-------------|
| [`truffle-core`](crates/truffle-core) | Rust library -- layered networking (Layers 3-6), Node API |
| [`truffle-cli`](crates/truffle-cli) | CLI tool -- `up`, `ls`, `ping`, `cp`, `send`, `tcp`, `doctor` |
| [`truffle-napi`](crates/truffle-napi) | NAPI-RS native addon for Node.js (pending update to new Node API) |
| [`truffle-tauri-plugin`](crates/truffle-tauri-plugin) | Tauri v2 plugin for desktop apps (pending update to new Node API) |
| [`sidecar-slim`](packages/sidecar-slim/) | Go sidecar for Tailscale tsnet integration (WatchIPNBus, UDP relay) |

**npm:** `@vibecook/truffle` + platform-specific sidecar packages (`@vibecook/truffle-sidecar-*`)

## Development

```bash
cargo build --workspace       # build all Rust crates
cargo test --workspace        # run tests (~159 tests)
pnpm install && pnpm build    # build TypeScript packages
```

### Prerequisites

- **Rust** >= 1.75
- **Go** >= 1.22 (for building the sidecar)
- **Node.js** >= 18 (for NAPI bindings)
- **Tailscale** installed and authenticated

## Releases

Automated via [release-please](https://github.com/googleapis/release-please). Push `feat:` or `fix:` commits to main, merge the generated Release PR, and builds trigger automatically for all 5 platforms.

## RFCs

| RFC | Status | Description |
|-----|--------|-------------|
| [RFC 003](docs/rfcs/) | Implemented | Rust rewrite (truffle-core, truffle-napi, truffle-tauri-plugin) |
| [RFC 005](docs/rfcs/) | Implemented | Core refactor (broadcast events, MessageBus dispatch) |
| [RFC 007](docs/rfcs/) | Implemented | Comprehensive refactor (23 audit fixes) |
| [RFC 008](docs/rfcs/) | Superseded | 6-layer architecture vision (superseded by RFC 012) |
| [RFC 009](docs/rfcs/) | Implemented | Wire protocol v3 (frame types, typed dispatch) |
| [RFC 010](docs/rfcs/) | Implemented | P2P redesign (remove STAR topology, CLI daemon) |
| [RFC 011](docs/rfcs/) | Implemented | File transfer over mesh (HTTP PUT, resume, SHA-256) |
| [RFC 012](docs/rfcs/012-layered-architecture-redesign.md) | Implemented | Clean layered architecture redesign (Layers 3-7, Node API) |

## Documentation

- [Architecture: Networking Flows](docs/architecture/networking-flows.md) -- complete flow analysis for every CLI command
- [GitHub Pages](https://jamesyong-42.github.io/truffle/) -- install scripts and docs site

## License

[MIT](LICENSE)
