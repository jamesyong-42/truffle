/**
 * Chat Example - Simple cross-device messaging
 *
 * Starts a mesh node and uses broadcast()/onMessage() on the 'chat'
 * namespace for pub/sub chat.
 *
 * Prerequisites:
 *   - Tailscale installed and running
 *   - npm install @vibecook/truffle
 *
 * Usage:
 *   npx tsx examples/chat/index.ts
 *
 * Run on multiple devices on the same Tailscale network to chat. Every
 * device must use the same appId ('chat-example') to see each other as
 * peers. Set NAME to pick your display name.
 */

import { createMeshNode, type MeshNamespacedMessage, type MeshPeerEvent } from '@vibecook/truffle';
import { createInterface } from 'readline';

const CHAT_NAMESPACE = 'chat';
const deviceName = process.env.NAME ?? `User-${Date.now().toString().slice(-4)}`;

const rl = createInterface({ input: process.stdin, output: process.stdout });
rl.setPrompt(`${deviceName}> `);

interface ChatPayload {
  name: string;
  text: string;
}

const mesh = await createMeshNode({
  appId: 'chat-example',
  deviceName,
  onAuthRequired: (url) => {
    console.log(`Auth required - visit: ${url}`);
  },
  onPeerChange: (event: MeshPeerEvent) => {
    switch (event.type) {
      case 'joined':
        console.log(`* ${event.peer?.displayName ?? event.peerId} joined`);
        rl.prompt();
        break;
      case 'left':
        console.log(`* ${event.peer?.displayName ?? event.peerId} left`);
        rl.prompt();
        break;
    }
  },
});

mesh.onMessage(CHAT_NAMESPACE, (msg: MeshNamespacedMessage) => {
  const payload = msg.payload as ChatPayload | null;
  if (!payload || typeof payload.text !== 'string') return;
  // `msg.from` is a Peer handle once the sender is known (hello precedes bus
  // traffic) or the raw routing key string otherwise — guard before use.
  const sender = typeof msg.from === 'string' ? msg.from : msg.from.displayName;
  console.log(`\n[${payload.name ?? sender}]: ${payload.text}`);
  rl.prompt();
});

console.log(`Chat started as "${deviceName}" (${mesh.getLocalInfo().deviceId})`);
console.log('Waiting for peers...\n');
rl.prompt();

rl.on('line', async (line) => {
  const text = line.trim();
  if (text) {
    const payload: ChatPayload = { name: deviceName, text };
    await mesh.broadcast(CHAT_NAMESPACE, Buffer.from(JSON.stringify(payload)));
  }
  rl.prompt();
});

rl.on('close', async () => {
  console.log('\nBye!');
  await mesh.stop();
  process.exit(0);
});
