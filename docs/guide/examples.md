# Examples

## CLI Examples

The fastest way to try truffle is with the CLI.

### Device Discovery

```sh
# Start your node
truffle up

# See all nodes on the mesh
truffle ls
```

### Messaging

```sh
# Send a one-shot message
truffle send laptop "build finished"

# Start a live chat
truffle chat laptop
```

### File Transfer

```sh
# Copy a file to another node
truffle cp ./report.pdf server:/tmp/

# Copy from remote to local
truffle cp server:/var/log/app.log ./
```

### Port Forwarding

```sh
# Expose local port 3000 on the mesh
truffle expose 3000

# Forward a remote port to localhost
truffle proxy server:5432
```

## Library Examples

::: warning
The library examples below use the **old Node.js API** (`createMeshNode`, `@vibecook/truffle-mesh`). The NAPI-RS bindings have not yet been updated to the v2 architecture (RFC 012). Use the CLI for now.
:::

The `examples/` directory contains working examples demonstrating programmatic usage with the Rust/Node.js library.

### Device Discovery

Discovers peers on the tailnet and logs device events.

```bash
cd examples/discovery
npx tsx index.ts
```

```typescript
import { createMeshNode } from '@vibecook/truffle-mesh';

const node = createMeshNode({
  deviceId: 'discovery-demo',
  deviceName: 'Discovery Demo',
  deviceType: 'desktop',
  hostnamePrefix: 'truffle-example',
  sidecarPath: '../../packages/sidecar/bin/tsnet-sidecar',
  stateDir: '.state',
});

node.on('deviceDiscovered', (device) => {
  console.log(`Found: ${device.name} (${device.id})`);
});

node.on('devicesChanged', (devices) => {
  console.log(`Total devices: ${devices.length}`);
});

await node.start();
```

### Cross-Device Chat

A simple chat app using the MessageBus.

```bash
cd examples/chat
npx tsx index.ts
```

Uses `bus.broadcast('chat', 'message', { text })` to send messages and `bus.subscribe('chat', handler)` to receive them.

### Shared State

Synchronizes a todo list across devices using `StoreSyncAdapter`.

```bash
cd examples/shared-state
npx tsx index.ts
```

Demonstrates `ISyncableStore` implementation with automatic sync across all connected devices.

### Running Library Examples

1. Build the sidecar: `cd packages/sidecar && make build`
2. Build packages: `pnpm run build`
3. Run the example: `cd examples/<name> && npx tsx index.ts`

Each example needs a running Tailscale network. Use auth keys for headless setups.
