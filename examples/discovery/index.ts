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

import { createMeshNode, type Peer, type MeshPeerEvent } from '@vibecook/truffle';

function printPeer(peer: Peer): void {
  console.log(`\nDiscovered: ${peer.displayName}`);
  // deviceId is the durable ULID, null until the peer's hello is seen (RFC 022).
  console.log(`  Device:  ${peer.deviceId ?? '(identity pending)'}`);
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
  onPeerChange: (event: MeshPeerEvent) => {
    switch (event.type) {
      case 'joined':
        if (event.peer) printPeer(event.peer);
        break;
      case 'left':
        console.log(`Device offline: ${event.peer?.displayName ?? event.peerId}`);
        break;
      case 'updated':
        if (event.peer)
          console.log(`Updated: ${event.peer.displayName} (${event.peer.deviceId ?? 'identity pending'})`);
        break;
      case 'identity':
        // Fires when the peer's hello lands and its durable deviceId is learned.
        if (event.peer) console.log(`Identity: ${event.peer.displayName} is ${event.peer.deviceId}`);
        break;
      case 'ws_connected':
        console.log(`Connected: ${event.peer?.displayName ?? event.peerId}`);
        break;
      case 'ws_disconnected':
        console.log(`Disconnected: ${event.peer?.displayName ?? event.peerId}`);
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
