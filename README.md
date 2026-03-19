# Truffle

[![crates.io](https://img.shields.io/crates/v/truffle-core)](https://crates.io/crates/truffle-core)
[![npm](https://img.shields.io/npm/v/@vibecook/truffle)](https://www.npmjs.com/package/@vibecook/truffle)
[![CI](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml/badge.svg)](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Mesh networking for local-first apps, built on Tailscale.**

Truffle lets your devices discover each other, elect a primary, and exchange messages over a secure Tailscale network -- no central server required. The core is written in Rust (~23k LOC, ~436 tests) with bindings for Node.js (via NAPI-RS), Tauri desktop apps, and a CLI tool. A thin Go sidecar (~600 LOC) provides the Tailscale `tsnet` integration.

## Features

- **Device Discovery** -- Automatic peer discovery via Tailscale with rich peer info (WhoIs identity, connection quality, key expiry)
- **STAR Topology** -- Primary election with automatic failover
- **Message Bus** -- Namespace-based pub/sub across devices
- **State Sync** -- Cross-device store synchronization via CRDT-like device-scoped slices
- **File Transfer** -- Resumable transfers with SHA-256 verification and real-time progress
- **HTTP Services** -- Reverse proxy, static file hosting, PWA support, and Web Push notifications
- **Wire Protocol** -- MessagePack/JSON framing with length-prefixed codec
- **Builder API** -- Ergonomic `TruffleRuntime::builder()` for configuring the full stack
- **CLI Tool** -- `truffle-cli` for status, peers, messaging, proxy, and file serving

## Architecture

Truffle is organized into 6 layers, from network foundation to application services:

```
┌─────────────────────────────────────────────────────────────┐
│  Layer 5: HTTP Services                                      │
│  Reverse proxy, static hosting, PWA, Web Push                │
├─────────────────────────────────────────────────────────────┤
│  Layer 4: Application Services                               │
│  Message bus, store sync, file transfer                      │
├─────────────────────────────────────────────────────────────┤
│  Layer 3: Mesh                                               │
│  STAR topology, device discovery, primary election           │
├─────────────────────────────────────────────────────────────┤
│  Layer 2: Transport                                          │
│  WebSocket connections, heartbeat, reconnection              │
├─────────────────────────────────────────────────────────────┤
│  Layer 1: Bridge                                             │
│  Binary-header TCP bridge between Go sidecar and Rust        │
├─────────────────────────────────────────────────────────────┤
│  Layer 0: Tailscale (tsnet)                                  │
│  Encrypted WireGuard tunnels, MagicDNS, auto-certs, ACLs    │
└─────────────────────────────────────────────────────────────┘
```

The Rust core (`truffle-core`) implements all mesh logic: device discovery, primary election, message routing, store sync, file transfer, HTTP services (reverse proxy, static hosting, PWA, Web Push), and a unified event API. The `TruffleRuntime` (in `runtime.rs`) wires together the sidecar, bridge, connection manager, mesh node, and HTTP router into a single high-level API with an ergonomic builder pattern. A thin Go shim (`sidecar-slim`) provides the Tailscale `tsnet` integration with WhoIs-based peer identity, background state monitoring, and ephemeral node support.

## Installation

**npm:**

```bash
npm install @vibecook/truffle
```

The Go sidecar binary is automatically installed for your platform (macOS, Linux, Windows). No manual downloads needed.

**Cargo:**

```toml
[dependencies]
truffle-core = "0.1"
```

**Tauri v2:**

```toml
[dependencies]
truffle-tauri-plugin = { git = "https://github.com/jamesyong-42/truffle" }
```

## Quick Start

### Node.js

```typescript
import { NapiMeshNode, resolveSidecarPath } from '@vibecook/truffle';

const node = new NapiMeshNode({
  deviceId: 'my-device-id',
  deviceName: 'My Laptop',
  deviceType: 'desktop',
  hostnamePrefix: 'myapp',
  sidecarPath: resolveSidecarPath(),
});

// Subscribe to events
node.onEvent((err, event) => {
  if (err) return;
  console.log(event.eventType, event.deviceId, event.payload);
});

await node.start();

// Discover devices
const devices = await node.devices();
console.log('Devices on mesh:', devices);

// Send a message
await node.broadcastEnvelope('chat', 'message', { text: 'Hello mesh!' });
```

### Rust (Low-Level -- MeshNode)

```rust
use std::sync::Arc;
use truffle_core::mesh::node::{MeshNode, MeshNodeConfig, MeshTimingConfig};
use truffle_core::transport::connection::{ConnectionManager, TransportConfig};
use truffle_core::protocol::envelope::MeshEnvelope;

let config = MeshNodeConfig {
    device_id: "my-device".into(),
    device_name: "My Laptop".into(),
    device_type: "desktop".into(),
    hostname_prefix: "myapp".into(),
    prefer_primary: false,
    capabilities: vec![],
    metadata: None,
    timing: MeshTimingConfig::default(),
};

let (conn_mgr, _transport_rx) = ConnectionManager::new(TransportConfig::default());
// MeshNode::new() returns a broadcast::Receiver (supports multiple consumers)
let (node, mut event_rx) = MeshNode::new(config, Arc::new(conn_mgr));
node.start().await;

let devices = node.devices().await;
let envelope = MeshEnvelope::new("chat", "message", serde_json::json!({"text": "Hello!"}));
node.broadcast_envelope(&envelope).await;

// Additional consumers subscribe independently
let mut rx2 = node.subscribe_events();
```

### Rust (High-Level -- TruffleRuntime Builder)

For applications that need the full stack (sidecar, bridge, mesh, HTTP), use the builder API:

```rust
use truffle_core::runtime::TruffleRuntime;

let runtime = TruffleRuntime::builder()
    .device_id("my-device")
    .device_name("My Laptop")
    .device_type("desktop")
    .hostname("myapp")
    .sidecar_path("/path/to/truffle-sidecar")
    .build()?;

let mut event_rx = runtime.start().await?;

// Access mesh node for device info, messaging, etc.
let devices = runtime.mesh_node().devices().await;

// Set up HTTP reverse proxy
runtime.http().proxy("/api", "localhost:3000").await?;

// Serve static files
runtime.http().serve_static("/", "./public").await?;

// Dial a peer through the full bridge pipeline
runtime.dial_peer("peer.tailnet.ts.net", 443).await?;

// Access sub-services
let bus = runtime.bus();
let sync = runtime.store_sync();
let ft = runtime.file_transfer();

runtime.stop().await;
```

### React

```tsx
import { useMesh } from '@vibecook/truffle-react';

function MeshStatus({ node }) {
  const { devices, isPrimary, role, broadcast } = useMesh(node);

  return (
    <div>
      <p>Role: {role} {isPrimary ? '(primary)' : ''}</p>
      <p>Devices: {devices.length}</p>
      <button onClick={() => broadcast('chat', 'ping', { ts: Date.now() })}>
        Ping
      </button>
    </div>
  );
}
```

## Crates & Packages

### Rust Crates

| Crate | Description |
|-------|-------------|
| [`truffle-core`](crates/truffle-core) | Pure Rust library -- mesh networking, HTTP services, store sync, file transfer (crates.io) |
| [`truffle-napi`](crates/truffle-napi) | NAPI-RS native addon for Node.js bindings |
| [`truffle-tauri-plugin`](crates/truffle-tauri-plugin) | Tauri v2 plugin for desktop apps |
| [`truffle-cli`](crates/truffle-cli) | CLI tool -- `status`, `peers`, `send`, `proxy`, `serve` commands |

### npm Packages

| Package | Description |
|---------|-------------|
| [`@vibecook/truffle`](packages/core) | Main npm package -- install this |
| [`@vibecook/truffle-native`](crates/truffle-napi) | NAPI-RS native addon (used internally) |
| [`@vibecook/truffle-sidecar-*`](npm/) | Go sidecar binaries per platform (installed automatically) |
| [`@vibecook/truffle-react`](packages/react) | React hooks (`useMesh`, `useSyncedStore`) |

### Go Sidecar

| Component | Description |
|-----------|-------------|
| [`sidecar-slim`](sidecar-slim/) | Thin Go shim (~600 LOC) for Tailscale tsnet integration -- WhoIs identity, state monitoring, ephemeral nodes |

## Development

```bash
# Install dependencies
pnpm install

# Build Rust + NAPI addon
cargo build --workspace
cd crates/truffle-napi && pnpm run build

# Build TypeScript packages
pnpm run build

# Run Rust tests (~436 tests)
cargo test --workspace

# Lint and format
pnpm run lint
pnpm run format:check
```

### Prerequisites

- **Rust** >= 1.75
- **Node.js** >= 18
- **Go** >= 1.22 (for building the sidecar shim)
- **Tailscale** installed and authenticated (or use an auth key)

## License

[MIT](LICENSE)
