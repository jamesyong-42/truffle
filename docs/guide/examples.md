# Examples

The `examples/` directory contains working examples demonstrating different Truffle features.

## Device Discovery

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

## Cross-Device Chat

A simple chat app using the MessageBus.

```bash
cd examples/chat
npx tsx index.ts
```

Uses `bus.broadcast('chat', 'message', { text })` to send messages and `bus.subscribe('chat', handler)` to receive them.

## Shared State

Synchronizes a todo list across devices using `StoreSyncAdapter`.

```bash
cd examples/shared-state
npx tsx index.ts
```

Demonstrates `ISyncableStore` implementation with automatic sync across all connected devices.

## Running Examples

1. Build the sidecar: `cd packages/sidecar && make build`
2. Build packages: `pnpm run build`
3. Run the example: `cd examples/<name> && npx tsx index.ts`

Each example needs a running Tailscale network. Use auth keys for headless setups.
