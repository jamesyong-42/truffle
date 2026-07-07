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
 *   - npm install @vibecook/truffle
 */

import { createMeshNode, type NapiPeer, type NapiPeerEvent } from '@vibecook/truffle';

function printPeer(peer: NapiPeer): void {
  console.log(`\nDiscovered: ${peer.deviceName}`);
  console.log(`  ID:      ${peer.deviceId}`);
  console.log(`  IP:      ${peer.ip}`);
  console.log(`  OS:      ${peer.os ?? 'unknown'}`);
  console.log(`  Online:  ${peer.online}`);
}

const mesh = await createMeshNode({
  appId: 'discovery-example',
  deviceName: 'Discovery Example',
  onAuthRequired: (url) => {
    console.log(`\nAuth required - visit: ${url}`);
  },
  onPeerChange: (event: NapiPeerEvent) => {
    switch (event.eventType) {
      case 'joined':
        if (event.peer) printPeer(event.peer);
        break;
      case 'left':
        console.log(`Device offline: ${event.peerId}`);
        break;
      case 'updated':
        if (event.peer) console.log(`Updated: ${event.peer.deviceName} (${event.peer.deviceId})`);
        break;
      case 'ws_connected':
        console.log(`Connected: ${event.peerId}`);
        break;
      case 'ws_disconnected':
        console.log(`Disconnected: ${event.peerId}`);
        break;
    }
  },
});

console.log('Mesh node started');
console.log('Local device:', mesh.getLocalInfo());
console.log('Discovering peers...');

const initialPeers = await mesh.getPeers();
initialPeers.forEach(printPeer);

// Graceful shutdown
process.on('SIGINT', async () => {
  console.log('\nShutting down...');
  await mesh.stop();
  process.exit(0);
});
