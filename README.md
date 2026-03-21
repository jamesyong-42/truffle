# Truffle

[![crates.io](https://img.shields.io/crates/v/truffle-core)](https://crates.io/crates/truffle-core)
[![npm](https://img.shields.io/npm/v/@vibecook/truffle)](https://www.npmjs.com/package/@vibecook/truffle)
[![CI](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml/badge.svg)](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**P2P mesh networking for your devices, built on Tailscale.**

Truffle gives your devices automatic discovery and direct peer-to-peer communication over Tailscale's encrypted WireGuard tunnels. No coordinator, no election, no central server -- every node connects directly to every other node. The core is written in Rust (~40k LOC, ~982 tests) with a product-grade CLI, Node.js bindings (NAPI-RS), Tauri plugin, and a thin Go sidecar (~1.1k LOC) for Tailscale `tsnet` integration.

## Install

```bash
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh
```

## Quick Start (CLI)

```bash
truffle up                        # start your node
truffle ls                        # see who's online
truffle ping laptop               # check latency
truffle tcp server:5432           # raw TCP connection
truffle proxy 5432 server:5432    # port forwarding
truffle expose 3000               # share local port
truffle chat laptop               # terminal chat
truffle cp file.txt server:/tmp/  # copy files
truffle doctor                    # diagnostics
```

## Quick Start (Rust API)

```rust
use truffle_core::runtime::TruffleRuntime;

let runtime = TruffleRuntime::builder()
    .hostname("my-app")
    .sidecar_path("/path/to/sidecar")
    .build()?;

let mut events = runtime.start().await?;

// Direct P2P messaging
runtime.broadcast_envelope(&envelope).await;

// HTTP services
runtime.http().proxy("/api", "localhost:3000").await?;
runtime.http().serve_static("/", "./public").await?;
```

## Architecture

6 layers, from network foundation to application services:

```
Layer 5: HTTP      -- reverse proxy, static hosting, PWA, Web Push
Layer 4: Services  -- message bus, store sync, file transfer
Layer 3: Mesh      -- P2P device discovery, direct messaging
Layer 2: Transport -- WebSocket, heartbeat, reconnection
Layer 1: Bridge    -- Go sidecar TCP bridge protocol
Layer 0: Tailscale -- tsnet, encrypted WireGuard tunnels
```

Every node maintains direct WebSocket connections to every other node. Messages are sent point-to-point or fan-out broadcast -- no routing through a hub, no topology management. Tailscale already provides the full-mesh encrypted network; truffle builds application-layer services directly on top of it.

## Crates & Packages

| Crate / Package | Description |
|-----------------|-------------|
| [`truffle-core`](crates/truffle-core) | Rust library -- mesh networking, HTTP services, store sync, file transfer |
| [`truffle-cli`](crates/truffle-cli) | CLI tool -- `up`, `ls`, `ping`, `proxy`, `expose`, `chat`, `cp`, `doctor` |
| [`truffle-napi`](crates/truffle-napi) | NAPI-RS native addon for Node.js |
| [`truffle-tauri-plugin`](crates/truffle-tauri-plugin) | Tauri v2 plugin for desktop apps |
| [`sidecar-slim`](sidecar-slim/) | Go sidecar (~1.1k LOC) for Tailscale tsnet integration |

**npm:** `@vibecook/truffle` (includes platform-specific sidecar binaries)

## Development

```bash
cargo build --workspace       # build all Rust crates
cargo test --workspace        # run ~982 tests
pnpm install && pnpm build    # build TypeScript packages
```

### Prerequisites

- **Rust** >= 1.75
- **Go** >= 1.22 (for building the sidecar)
- **Node.js** >= 18 (for NAPI bindings)
- **Tailscale** installed and authenticated

## Stats

- ~40k LOC Rust + ~1.1k LOC Go
- ~982 tests
- 5 crates (truffle-core, truffle-napi, truffle-tauri-plugin, truffle-cli + Go sidecar)
- v3 wire protocol with typed dispatch
- Cross-platform: macOS, Linux, Windows

## Documentation

Full documentation at https://jamesyong-42.github.io/truffle/

## License

[MIT](LICENSE)
