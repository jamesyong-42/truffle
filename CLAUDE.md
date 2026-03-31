# Truffle — Mesh Networking Library

## What This Is

Mesh networking for local-first apps built on Tailscale. Devices discover each other, exchange messages, sync state, transfer files — no central server.

## Architecture

Clean layered architecture (RFC 012):

```
Layer 7: Applications   — CLI commands, TUI, daemon handler
         Node API       — 16-method public API + FileTransfer + SyncedStore
Layer 6: Envelope       — namespace-routed message framing (JSON codec)
Layer 5: Session        — PeerRegistry, lazy WS connections, event broadcast
Layer 4: Transport      — WebSocket, TCP, UDP, QUIC transports
Layer 3: Network        — TailscaleProvider (peer discovery via WatchIPNBus)
         Go Sidecar     — tsnet integration, WireGuard tunnels
```

Key principles:
- Layer 3 (WatchIPNBus) is the source of truth for peers — NOT transport connections
- `PeerState.ws_connected` tracks WebSocket state; `PeerState.online` tracks Tailscale status
- Subsystems (FileTransfer, SyncedStore) live in core, built on Node primitives, own their namespace
- `send_typed()`/`broadcast_typed()` for explicit msg_type — subsystems use these, not `send()`

## Crates

| Crate | Purpose |
|-------|---------|
| `truffle-core` (~12k LOC) | Layers 3-6, Node API, file transfer, synced store, request/reply |
| `truffle-cli` (~10k LOC) | 13 CLI commands + interactive TUI (ratatui + crossterm) |
| `truffle-sidecar` | Build-time sidecar download helper |
| `truffle` | Thin re-export crate |
| `truffle-napi` | NAPI-RS bridge for Node.js |
| `truffle-tauri-plugin` | Tauri v2 desktop plugin |

## Build & Test

```bash
cargo build --workspace
cargo test --workspace        # ~199 tests, all in truffle-core
```

## Release Workflow

1. Push `feat:` or `fix:` commit to main
2. release-please creates/updates a release PR
3. Checkout release branch, bump Cargo.toml versions (`sed -i '' 's/^version = "OLD"/version = "NEW"/'`), push
4. Wait for CI, merge PR with `--admin`
5. Binary builds (CLI + sidecar) auto-trigger via `actions: write` permission
6. Use `--ref truffle-v<VERSION>` if manually triggering workflows (NOT `--ref main`)

## Key Conventions

- **Namespaces**: `"ft"` = file transfer, `"ss:{id}"` = synced store, `"chat"` = messaging
- **Wire protocol**: Envelope with namespace + msg_type + JSON payload over WebSocket
- **Windows TUI**: Must filter `KeyEventKind::Press` — crossterm emits Press+Release on Windows
- **Tests**: Use `MockNetworkProvider` + `make_test_node()` pattern; mock can't do real WS between nodes
- **CI excludes**: truffle-tauri-plugin and truffle-napi from `cargo test` (need platform-specific tooling)

## RFCs

RFCs live in `docs/rfcs/`. Key ones:
- **012**: Layered architecture redesign
- **013**: TUI + agent CLI dual-mode
- **014**: File transfer in core (not Layer 7)
- **015**: NAPI-RS and Tauri plugin rewrite
- **016**: SyncedStore + request/reply (Phase 1-2 done, Phase 3 persistence pending)
