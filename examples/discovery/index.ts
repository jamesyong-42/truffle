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
 *   - Sidecar binary built (cd packages/sidecar && make build)
 */

import { createMeshNode } from '@vibecook/truffle';

const node = createMeshNode({
  deviceId: `discovery-${Date.now()}`,
  deviceName: 'Discovery Example',
  deviceType: 'desktop',
  hostnamePrefix: 'truffle-example',
  sidecarPath: './packages/sidecar/bin/tsnet-sidecar',
  stateDir: './tmp/discovery-state',
});

node.on('started', () => {
  console.log('Mesh node started');
  console.log('Local device:', node.getLocalDevice());
  console.log('Discovering peers...');
});

node.on('deviceDiscovered', (device) => {
  console.log(`\nDiscovered: ${device.name}`);
  console.log(`  ID:   ${device.id}`);
  console.log(`  Type: ${device.type}`);
  console.log(`  IP:   ${device.tailscaleIP}`);
  console.log(`  OS:   ${device.os ?? 'unknown'}`);
});

node.on('deviceOffline', (deviceId) => {
  console.log(`Device offline: ${deviceId}`);
});

node.on('roleChanged', (role, isPrimary) => {
  console.log(`Role: ${role} (isPrimary=${isPrimary})`);
});

node.on('authRequired', (url) => {
  console.log(`\nAuth required - visit: ${url}`);
});

node.on('error', (err) => {
  console.error('Error:', err.message);
});

// Graceful shutdown
process.on('SIGINT', async () => {
  console.log('\nShutting down...');
  await node.stop();
  process.exit(0);
});

await node.start();
