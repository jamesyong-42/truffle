/**
 * Discovery Example - Find peers on the mesh network
 *
 * Starts a mesh node, discovers peers, and prints device info.
 *
 * Usage:
 *   npx tsx examples/discovery/index.ts
 *
 * Prerequisites:
 *   - Tailscale installed and running
 *   - NAPI addon built (cd crates/truffle-napi && pnpm run build)
 */

import { NapiMeshNode, resolveSidecarPath } from '@vibecook/truffle';
import type { NapiMeshEvent } from '@vibecook/truffle';

const node = new NapiMeshNode({
  deviceId: `discovery-${Date.now()}`,
  deviceName: 'Discovery Example',
  deviceType: 'desktop',
  hostnamePrefix: 'truffle-example',
  sidecarPath: resolveSidecarPath(),
  stateDir: './tmp/discovery-state',
});

// Subscribe to mesh events via NAPI callback
node.onEvent((err: null | Error, event: NapiMeshEvent) => {
  if (err) return;

  switch (event.eventType) {
    case 'started':
      console.log('Mesh node started');
      node.localDevice().then((device) => {
        console.log('Local device:', device);
      });
      console.log('Discovering peers...');
      break;

    case 'deviceDiscovered':
      console.log(`\nDiscovered: ${event.payload?.name ?? event.deviceId}`);
      console.log(`  ID:   ${event.deviceId}`);
      if (event.payload && typeof event.payload === 'object') {
        const p = event.payload as Record<string, unknown>;
        console.log(`  Type: ${p.deviceType ?? 'unknown'}`);
        console.log(`  IP:   ${p.tailscaleIp ?? 'unknown'}`);
        console.log(`  OS:   ${p.os ?? 'unknown'}`);
      }
      break;

    case 'deviceOffline':
      console.log(`Device offline: ${event.deviceId}`);
      break;

    case 'roleChanged':
      if (event.payload && typeof event.payload === 'object') {
        const p = event.payload as Record<string, unknown>;
        console.log(`Role: ${p.role} (isPrimary=${p.isPrimary})`);
      }
      break;

    case 'authRequired':
      console.log(`\nAuth required - visit: ${event.payload}`);
      break;

    case 'error':
      console.error('Error:', event.payload);
      break;
  }
});

// Graceful shutdown
process.on('SIGINT', async () => {
  console.log('\nShutting down...');
  await node.stop();
  process.exit(0);
});

await node.start();
