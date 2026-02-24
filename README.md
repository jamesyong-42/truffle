# Truffle

[![MIT License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Mesh networking for local-first apps, built on Tailscale.**

Truffle lets your devices discover each other, elect a primary, and exchange messages over a secure Tailscale network — no central server required.

## Features

- **Device Discovery** — Automatic peer discovery via Tailscale
- **STAR Topology** — Primary election with automatic failover
- **Message Bus** — Namespace-based pub/sub across devices
- **State Sync** — Cross-device store synchronization
- **Wire Protocol** — MessagePack/JSON framing with length-prefixed codec
- **Zero Config** — Works out of the box with Tailscale auth keys

## Architecture

```
┌─────────────────────────────────────────────────┐
│                  Your App                        │
├─────────────────────────────────────────────────┤
│  Layer 2: MessageBus (pub/sub)                  │
│           StoreSyncAdapter                       │
├─────────────────────────────────────────────────┤
│  Layer 1: MeshNode (routing, election, discovery)│
├─────────────────────────────────────────────────┤
│  Layer 0: WebSocketTransport (connections)       │
├─────────────────────────────────────────────────┤
│  SidecarClient (IPC to Go binary)               │
├─────────────────────────────────────────────────┤
│  Go Sidecar (embeds Tailscale tsnet)            │
└─────────────────────────────────────────────────┘
```

## Quick Start

```bash
npm install @vibecook/truffle
```

```typescript
import { createMeshNode } from '@vibecook/truffle';

const node = createMeshNode({
  deviceId: 'my-device-id',
  deviceName: 'My Laptop',
  deviceType: 'desktop',
  hostnamePrefix: 'myapp',
  sidecarPath: './path/to/sidecar',
  stateDir: './tailscale-state',
});

node.on('deviceDiscovered', (device) => {
  console.log('Found device:', device.name);
});

node.on('roleChanged', (role, isPrimary) => {
  console.log(`Role: ${role}, isPrimary: ${isPrimary}`);
});

await node.start();
```

## Packages

| Package | Description |
|---------|-------------|
| [`@vibecook/truffle`](packages/core) | Unified entry point — install this |
| [`@vibecook/truffle-types`](packages/types) | Type definitions and Zod schemas |
| [`@vibecook/truffle-protocol`](packages/protocol) | Wire format (MessagePack/JSON codec) and message bus interface |
| [`@vibecook/truffle-sidecar-client`](packages/sidecar-client) | TypeScript client for the Go sidecar process |
| [`@vibecook/truffle-transport`](packages/transport) | WebSocket transport layer over Tailscale |
| [`@vibecook/truffle-mesh`](packages/mesh) | Device discovery, STAR routing, primary election |
| [`@vibecook/truffle-store-sync`](packages/store-sync) | Cross-device state synchronization |

## Prerequisites

- **Node.js** >= 18
- **Tailscale** — installed and authenticated (or use an auth key)
- **Go** >= 1.21 — required to build the sidecar binary

## Development

```bash
# Install dependencies
pnpm install

# Build all packages
pnpm run build

# Run all tests
pnpm run test

# Type check
pnpm run typecheck
```

## License

[MIT](LICENSE)
