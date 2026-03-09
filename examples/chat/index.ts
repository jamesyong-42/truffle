/**
 * Chat Example - Simple cross-device messaging
 *
 * Starts a mesh node and uses broadcastEnvelope for pub/sub chat.
 *
 * Usage:
 *   npx tsx examples/chat/index.ts
 *
 * Run on multiple devices on the same Tailscale network to chat.
 */

import { NapiMeshNode, resolveSidecarPath } from '@vibecook/truffle';
import type { NapiMeshEvent } from '@vibecook/truffle';
import { createInterface } from 'readline';

const deviceId = `chat-${Date.now()}`;
const deviceName = process.env.NAME ?? `User-${deviceId.slice(-4)}`;

const node = new NapiMeshNode({
  deviceId,
  deviceName,
  deviceType: 'desktop',
  hostnamePrefix: 'truffle-chat',
  sidecarPath: resolveSidecarPath(),
  stateDir: `./tmp/chat-state-${deviceId}`,
});

const rl = createInterface({ input: process.stdin, output: process.stdout });
rl.setPrompt(`${deviceName}> `);

// Subscribe to mesh events
node.onEvent((err: null | Error, event: NapiMeshEvent) => {
  if (err) return;

  switch (event.eventType) {
    case 'started':
      console.log(`Chat started as "${deviceName}" (${deviceId})`);
      console.log('Waiting for peers...\n');
      rl.prompt();
      break;

    case 'deviceDiscovered':
      console.log(`* ${event.deviceId} joined`);
      rl.prompt();
      break;

    case 'deviceOffline':
      console.log(`* ${event.deviceId} left`);
      rl.prompt();
      break;

    case 'authRequired':
      console.log(`Auth required - visit: ${event.payload}`);
      break;

    case 'message': {
      // Incoming application-level message
      if (event.payload && typeof event.payload === 'object') {
        const p = event.payload as Record<string, unknown>;
        if (p.namespace === 'chat' && p.type === 'message') {
          const inner = p.payload as Record<string, unknown> | undefined;
          if (inner && event.deviceId !== deviceId) {
            console.log(`\n[${inner.name ?? event.deviceId}]: ${inner.text}`);
            rl.prompt();
          }
        }
      }
      break;
    }
  }
});

// Start node
await node.start();

rl.on('line', async (line) => {
  const text = line.trim();
  if (text) {
    await node.broadcastEnvelope('chat', 'message', { name: deviceName, text });
  }
  rl.prompt();
});

rl.on('close', async () => {
  console.log('\nBye!');
  await node.stop();
  process.exit(0);
});
