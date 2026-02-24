# Getting Started

## Prerequisites

- **Node.js** >= 18
- **Tailscale** installed and authenticated (or use an auth key)
- **Go** >= 1.21 (to build the sidecar binary)

## Installation

```bash
npm install @vibecook/truffle
```

For the sidecar binary (prebuilt, when available):

```bash
npm install @vibecook/truffle-sidecar-bin
```

## Quick Start

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

## Using the CLI

```bash
# Initialize a project
npx @vibecook/truffle-cli init

# Start a dev node
npx @vibecook/truffle-cli dev --prefix myapp

# Check status
npx @vibecook/truffle-cli status
```

## Packages

| Package | Description |
|---------|-------------|
| `@vibecook/truffle` | Unified entry point — install this |
| `@vibecook/truffle-types` | Type definitions and Zod schemas |
| `@vibecook/truffle-protocol` | Wire format and message bus interface |
| `@vibecook/truffle-sidecar-client` | TypeScript client for the Go sidecar |
| `@vibecook/truffle-transport` | WebSocket transport layer |
| `@vibecook/truffle-mesh` | Discovery, routing, primary election |
| `@vibecook/truffle-store-sync` | Cross-device state synchronization |
| `@vibecook/truffle-react` | React hooks for mesh networking |
| `@vibecook/truffle-cli` | CLI for project scaffolding and dev |
