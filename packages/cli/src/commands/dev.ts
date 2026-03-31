import { defineCommand } from 'citty';
import { consola } from 'consola';
import { hostname } from 'node:os';
import { NapiNode, resolveSidecarPath } from '@vibecook/truffle';
import type { NapiPeer, NapiPeerEvent } from '@vibecook/truffle';

function formatPeer(peer: NapiPeer): string {
  const status = peer.online ? 'online' : 'offline';
  const ws = peer.wsConnected ? ', ws' : '';
  return `  ${peer.name} (${peer.id}) - ${status}${ws}`;
}

export const devCommand = defineCommand({
  meta: {
    name: 'dev',
    description: 'Start a dev mesh node with auto-discovery',
  },
  args: {
    name: {
      type: 'string',
      description: 'Node name',
      default: hostname(),
    },
    sidecar: {
      type: 'string',
      description: 'Path to sidecar binary (auto-detected if omitted)',
    },
    'state-dir': {
      type: 'string',
      description: 'State directory',
      default: '.truffle-state',
    },
    'auth-key': {
      type: 'string',
      description: 'Tailscale auth key',
    },
  },
  async run({ args }) {
    consola.start(`Starting Truffle dev node: ${args.name}`);

    const node = new NapiNode();

    await node.start({
      name: args.name,
      sidecarPath: args.sidecar ?? resolveSidecarPath(),
      stateDir: args['state-dir'],
      authKey: args['auth-key'],
    });

    const info = node.getLocalInfo();
    consola.success(`Node started: ${info.name} (${info.id})`);
    consola.info('Waiting for peers...');

    // Subscribe to peer change events
    node.onPeerChange((event: NapiPeerEvent) => {
      switch (event.eventType) {
        case 'joined':
          if (event.peer) {
            consola.success(`Joined: ${formatPeer(event.peer)}`);
          }
          break;

        case 'left':
          consola.warn(`Left: ${event.peerId}`);
          break;

        case 'updated':
          if (event.peer) {
            consola.info(`Updated: ${formatPeer(event.peer)}`);
          }
          break;

        case 'ws_connected':
          consola.info(`WS connected: ${event.peerId}`);
          break;

        case 'ws_disconnected':
          consola.warn(`WS disconnected: ${event.peerId}`);
          break;

        case 'auth_required':
          consola.warn('Authentication required!');
          if (event.authUrl) {
            consola.info(`Open: ${event.authUrl}`);
          }
          break;
      }
    });

    // Graceful shutdown
    const shutdown = async () => {
      consola.info('Shutting down...');
      await node.stop();
      consola.success('Stopped');
      process.exit(0);
    };

    process.on('SIGINT', shutdown);
    process.on('SIGTERM', shutdown);
  },
});
