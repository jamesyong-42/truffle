/**
 * Chat Example - Simple cross-device messaging
 *
 * Starts a mesh node and uses MessageBus for pub/sub chat.
 *
 * Usage:
 *   npx tsx examples/chat/index.ts
 *
 * Run on multiple devices on the same Tailscale network to chat.
 */

import { createMeshNode } from '@vibecook/truffle';
import { createInterface } from 'readline';

const deviceId = `chat-${Date.now()}`;
const deviceName = process.env.NAME ?? `User-${deviceId.slice(-4)}`;

const node = createMeshNode({
  deviceId,
  deviceName,
  deviceType: 'desktop',
  hostnamePrefix: 'truffle-chat',
  sidecarPath: './packages/sidecar/bin/tsnet-sidecar',
  stateDir: `./tmp/chat-state-${deviceId}`,
});

const bus = node.getMessageBus();

// Subscribe to chat messages
bus.subscribe('chat', (msg) => {
  if (msg.from === deviceId) return; // skip own messages
  const { name, text } = msg.payload as { name: string; text: string };
  console.log(`\n[${name}]: ${text}`);
  rl.prompt();
});

node.on('started', () => {
  console.log(`Chat started as "${deviceName}" (${deviceId})`);
  console.log('Waiting for peers...\n');
});

node.on('deviceDiscovered', (device) => {
  console.log(`* ${device.name} joined`);
  rl.prompt();
});

node.on('deviceOffline', (offlineId) => {
  console.log(`* ${offlineId} left`);
  rl.prompt();
});

node.on('authRequired', (url) => {
  console.log(`Auth required - visit: ${url}`);
});

// Start node
await node.start();

// Read input
const rl = createInterface({ input: process.stdin, output: process.stdout });
rl.setPrompt(`${deviceName}> `);
rl.prompt();

rl.on('line', (line) => {
  const text = line.trim();
  if (text) {
    bus.broadcast('chat', 'message', { name: deviceName, text });
  }
  rl.prompt();
});

rl.on('close', async () => {
  console.log('\nBye!');
  await node.stop();
  process.exit(0);
});
