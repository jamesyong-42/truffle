import { defineCommand } from 'citty';
import { consola } from 'consola';
import { randomUUID } from 'node:crypto';
import { hostname } from 'node:os';
import { NapiMeshNode, resolveSidecarPath } from '@vibecook/truffle';
import type { NapiBaseDevice as BaseDevice, NapiMeshEvent } from '@vibecook/truffle';

function formatDevice(device: BaseDevice): string {
  const status = device.status === 'online' ? 'online' : 'offline';
  return `  ${device.name} (${device.id}) - ${status}`;
}

export const devCommand = defineCommand({
  meta: {
    name: 'dev',
    description: 'Start a dev mesh node with auto-discovery',
  },
  args: {
    name: {
      type: 'string',
      description: 'Device name',
      default: hostname(),
    },
    prefix: {
      type: 'string',
      description: 'Hostname prefix for peer discovery',
      default: 'truffle',
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
    type: {
      type: 'string',
      description: 'Device type',
      default: 'desktop',
    },
  },
  async run({ args }) {
    const deviceId = randomUUID().slice(0, 8);

    consola.start(`Starting Truffle dev node: ${args.name} (${deviceId})`);

    const node = new NapiMeshNode({
      deviceId,
      deviceName: args.name,
      deviceType: args.type,
      hostnamePrefix: args.prefix,
      sidecarPath: args.sidecar ?? resolveSidecarPath(),
      stateDir: args['state-dir'],
      authKey: args['auth-key'],
    });

    // Subscribe to mesh events via NAPI callback
    node.onEvent((err: null | Error, event: NapiMeshEvent) => {
      if (err) {
        consola.error('Mesh error:', err.message);
        return;
      }

      switch (event.eventType) {
        case 'started':
          consola.success('Mesh node started');
          consola.info(`Local device: ${args.name} (${deviceId})`);
          consola.info('Waiting for peers...');
          break;

        case 'peersChanged':
          if (Array.isArray(event.payload)) {
            const devices = event.payload as BaseDevice[];
            consola.info(`Devices (${devices.length}):`);
            for (const device of devices) {
              consola.log(formatDevice(device));
            }
          }
          break;

        case 'peerDiscovered':
          if (event.payload) {
            const device = event.payload as BaseDevice;
            consola.success(`Discovered: ${device.name} (${device.id})`);
          } else if (event.deviceId) {
            consola.success(`Discovered: ${event.deviceId}`);
          }
          break;

        case 'peerOffline':
          consola.warn(`Offline: ${event.deviceId ?? 'unknown'}`);
          break;

        case 'peerConnected':
          consola.info(`Peer connected: ${event.payload?.peer_dns ?? event.deviceId ?? 'unknown'}`);
          break;

        case 'peerDisconnected':
          consola.warn(`Peer disconnected: ${event.deviceId ?? 'unknown'}`);
          break;

        case 'authRequired':
          consola.warn('Authentication required!');
          if (event.payload?.url) {
            consola.info(`Open: ${event.payload.url}`);
          }
          break;

        case 'error':
          consola.error('Mesh error:', event.payload?.message ?? event.payload);
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

    try {
      await node.start();
    } catch (error) {
      consola.error('Failed to start:', error);
      process.exit(1);
    }
  },
});
