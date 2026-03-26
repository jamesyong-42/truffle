# Truffle

[![CI](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml/badge.svg)](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/jamesyong-42/truffle?label=latest)](https://github.com/jamesyong-42/truffle/releases/latest)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**P2P mesh networking for your devices, built on Tailscale.**

Truffle gives your devices automatic discovery and direct peer-to-peer communication over Tailscale's encrypted WireGuard tunnels. No coordinator, no election, no central server -- every node connects directly to every other node. ~15k LOC Rust across 2 crates with a clean 7-layer architecture (RFC 012), a product-grade CLI (9 working commands), and a thin Go sidecar (~1.8k LOC) for Tailscale `tsnet` integration. All four transports (WS, TCP, UDP, QUIC) verified cross-machine over real Tailscale. Node.js bindings (NAPI-RS) and Tauri plugin exist but are pending update to the new Node API.

## Install

```bash
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh
```

Supports macOS (arm64/x64), Linux (x64/arm64), and Windows (x64).

## Quick Start

```bash
truffle up                        # start your node and join the mesh
truffle ls                        # see who's online
truffle status                    # show node identity, IP, uptime
truffle ping laptop               # check latency (direct or relay)
truffle send server "hello"       # send a namespaced message
truffle cp file.txt server:/tmp/  # copy files (SHA-256 verified)
truffle tcp server:5432           # raw TCP stream (like netcat)
truffle doctor                    # diagnose connectivity issues
truffle down                      # graceful shutdown
```

## What Works (v2 — RFC 012)

All commands verified end-to-end over real Tailscale (macOS <-> EC2 Linux).

| Command | Status | Description |
|---------|--------|-------------|
| `truffle up` | **Working** | Start node via Go sidecar, discover peers via WatchIPNBus, establish mesh |
| `truffle down` | **Working** | Graceful shutdown — close transports, stop sidecar |
| `truffle status` | **Working** | Show node identity, IP, DNS name, uptime, peer count |
| `truffle ls` | **Working** | List peers with online/offline status and OS info |
| `truffle ping` | **Working** | Latency check via Tailscale (direct path or DERP relay) |
| `truffle cp` | **Working** | File transfer over mesh (HTTP PUT, SHA-256 verified, progress bar) |
| `truffle send` | **Working** | Send namespaced messages via Layer 6 envelopes over WebSocket |
| `truffle tcp` | **Working** | Raw TCP stream through Tailscale (pipe-friendly, like netcat) |
| `truffle doctor` | **Working** | Diagnose Tailscale, sidecar binary, mesh connectivity, key expiry |

## Architecture

Clean 7-layer architecture (RFC 012), built bottom-up with trait boundaries at each layer:

```
Layer 7: Applications   -- CLI commands: file transfer, messaging, diagnostics
         Node API       -- 15-method public API (the only import apps need)
Layer 6: Envelope       -- namespace-routed message framing (EnvelopeCodec)
Layer 5: Session        -- PeerRegistry, lazy connections, message fan-out
Layer 4: Transport      -- WebSocket, TCP, UDP, QUIC (all verified cross-machine)
Layer 3: Network        -- TailscaleProvider, WatchIPNBus peer discovery
         Go Sidecar     -- tsnet integration, encrypted WireGuard tunnels
```

The `Node` struct is the single public entry point. Applications never import from lower layers directly. Peer discovery works via Tailscale's WatchIPNBus -- peers are known before any transport connections are established. Messages are sent point-to-point or fan-out broadcast with namespace-based routing.

### Port Map

| Port | Protocol | Purpose |
|------|----------|---------|
| 443  | TLS/WS   | Incoming WebSocket connections (tsnet HTTPS listener) |
| 9417 | TCP       | Direct TCP mesh connections |
| dynamic | UDP   | UDP relay via tsnet ListenPacket |

## Crates & Packages

| Crate / Package | Status | Description |
|-----------------|--------|-------------|
| [`truffle-core`](crates/truffle-core) | **Active** | Rust library -- Layers 3-6, Node API (~12k LOC) |
| [`truffle-cli`](crates/truffle-cli) | **Active** | CLI tool -- 9 working commands on the Node API (~3k LOC) |
| [`sidecar-slim`](packages/sidecar-slim/) | **Active** | Go sidecar -- Tailscale tsnet, WatchIPNBus, UDP relay (~1.8k LOC) |
| [`truffle-napi`](crates/truffle-napi) | Pending | NAPI-RS native addon for Node.js (needs update to new Node API) |
| [`truffle-tauri-plugin`](crates/truffle-tauri-plugin) | Pending | Tauri v2 plugin for desktop apps (needs update to new Node API) |

**npm:** `@vibecook/truffle` + platform-specific sidecar packages (`@vibecook/truffle-sidecar-*`)

## Development

```bash
cargo build --workspace       # build all Rust crates
cargo test --workspace        # run tests (~192 tests)
```

### Prerequisites

- **Rust** >= 1.75
- **Go** >= 1.22 (for building the sidecar)
- **Tailscale** installed and authenticated

## Releases

Automated via [release-please](https://github.com/googleapis/release-please). Push `feat:` or `fix:` commits to main, merge the generated Release PR, and builds trigger automatically for all 5 platforms.

## RFCs

| RFC | Status | Description |
|-----|--------|-------------|
| [RFC 003](docs/rfcs/) | Implemented | Rust rewrite (truffle-core, truffle-napi, truffle-tauri-plugin) |
| [RFC 005](docs/rfcs/) | Implemented | Core refactor (broadcast events, MessageBus dispatch) |
| [RFC 007](docs/rfcs/) | Implemented | Comprehensive refactor (23 audit fixes) |
| [RFC 008](docs/rfcs/) | Superseded | 6-layer architecture vision (replaced by RFC 012's 7-layer design) |
| [RFC 009](docs/rfcs/) | Implemented | Wire protocol v3 (frame types, typed dispatch) |
| [RFC 010](docs/rfcs/) | Implemented | P2P redesign (remove STAR topology, CLI daemon) |
| [RFC 011](docs/rfcs/) | Implemented | File transfer over mesh (HTTP PUT, resume, SHA-256) |
| [RFC 012](docs/rfcs/012-layered-architecture-redesign.md) | Implemented | Clean layered architecture redesign (Layers 3-7, Node API) |

## Documentation

- [GitHub Pages](https://jamesyong-42.github.io/truffle/) -- install scripts and docs site
- [Architecture: Networking Flows (v1, archived)](docs/architecture/networking-flows.md) -- flow analysis for the old architecture

## License

[MIT](LICENSE)
