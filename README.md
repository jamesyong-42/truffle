# Truffle

[![crates.io](https://img.shields.io/crates/v/truffle-core)](https://crates.io/crates/truffle-core)
[![npm](https://img.shields.io/npm/v/@vibecook/truffle)](https://www.npmjs.com/package/@vibecook/truffle)
[![CI](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml/badge.svg)](https://github.com/jamesyong-42/truffle/actions/workflows/ci.yml)
[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Mesh networking for local-first apps, built on Tailscale.**

Truffle lets your devices discover each other, elect a primary, and exchange messages over a secure Tailscale network -- no central server required. The core is written in Rust with bindings for Node.js (via NAPI-RS) and Tauri desktop apps.

## Features

- **Device Discovery** -- Automatic peer discovery via Tailscale
- **STAR Topology** -- Primary election with automatic failover
- **Message Bus** -- Namespace-based pub/sub across devices
- **State Sync** -- Cross-device store synchronization
- **File Transfer** -- Resumable transfers with SHA-256 verification and real-time progress
- **Reverse Proxy** -- Built-in HTTP reverse proxy over the mesh
- **Wire Protocol** -- MessagePack/JSON framing with length-prefixed codec

## Architecture

```
+---------------------------------------------------+
|                   Your App                        |
+---------------------------------------------------+
|  React Hooks / CLI (optional)                     |
+---------------------------------------------------+
|  @vibecook/truffle (npm) | truffle-core (cargo)   |
+---------------------------------------------------+
|  NAPI-RS bridge (Node.js) | Tauri plugin (desktop) |
+---------------------------------------------------+
|             truffle-core (Rust)                    |
+---------------------------------------------------+
|           Go shim (tsnet sidecar)                  |
+---------------------------------------------------+
|             Tailscale network                      |
+---------------------------------------------------+
```

The Rust core (`truffle-core`) implements all mesh logic: device discovery, primary election, message routing, store sync, file transfer, and reverse proxy. The `TruffleRuntime` (in `runtime.rs`) wires together the sidecar, bridge, connection manager, and mesh node into a single high-level API. A thin Go shim (`sidecar-slim`) provides the Tailscale `tsnet` integration -- it manages the Tailscale node and bridges connections to the Rust core via a binary-header TCP bridge protocol.

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

### Rust (High-Level -- TruffleRuntime)

For applications that need the full stack (sidecar, bridge, mesh), use `TruffleRuntime`:

```rust
use truffle_core::runtime::{TruffleRuntime, RuntimeConfig};
use truffle_core::mesh::node::{MeshNodeConfig, MeshTimingConfig};
use truffle_core::transport::connection::TransportConfig;

let config = RuntimeConfig {
    mesh: MeshNodeConfig {
        device_id: "my-device".into(),
        device_name: "My Laptop".into(),
        device_type: "desktop".into(),
        hostname_prefix: "myapp".into(),
        prefer_primary: false,
        capabilities: vec![],
        metadata: None,
        timing: MeshTimingConfig::default(),
    },
    transport: TransportConfig::default(),
    sidecar_path: Some("/path/to/truffle-sidecar".into()),
    state_dir: None,
    auth_key: None,
};

let (runtime, mut event_rx) = TruffleRuntime::new(&config);
runtime.start(&config).await.unwrap();

// Access mesh node for device info, messaging, etc.
let devices = runtime.mesh_node().devices().await;

// Dial a peer through the full bridge pipeline
runtime.dial_peer("peer.tailnet.ts.net", 443).await.unwrap();

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

## Packages

| Package | Description |
|---------|-------------|
| [`@vibecook/truffle`](packages/core) | Main npm package -- install this |
| [`@vibecook/truffle-native`](crates/truffle-napi) | NAPI-RS native addon (used internally) |
| [`@vibecook/truffle-sidecar-*`](npm/) | Go sidecar binaries per platform (installed automatically) |
| [`@vibecook/truffle-react`](packages/react) | React hooks (`useMesh`, `useSyncedStore`) |
| [`@vibecook/truffle-cli`](packages/cli) | CLI tool for scaffolding and dev mode |
| [`truffle-core`](crates/truffle-core) | Pure Rust library (crates.io) |
| [`truffle-tauri-plugin`](crates/truffle-tauri-plugin) | Tauri v2 plugin |

## Development

```bash
# Install dependencies
pnpm install

# Build Rust + NAPI addon
cargo build --workspace
cd crates/truffle-napi && pnpm run build

# Build TypeScript packages
pnpm run build

# Run Rust tests (~250 tests)
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
