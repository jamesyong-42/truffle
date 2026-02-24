import { defineCommand } from 'citty';
import { consola } from 'consola';
import { randomUUID } from 'node:crypto';
import { hostname } from 'node:os';
import { createMeshNode } from '@vibecook/truffle-mesh';
import type { BaseDevice, DeviceRole, Logger } from '@vibecook/truffle-types';

function createCliLogger(): Logger {
  return {
    info: (msg, ...args) => consola.info(msg, ...args),
    warn: (msg, ...args) => consola.warn(msg, ...args),
    error: (msg, ...args) => consola.error(msg, ...args),
    debug: (msg, ...args) => consola.debug(msg, ...args),
  };
}

function formatDevice(device: BaseDevice): string {
  const role = device.role === 'primary' ? ' [primary]' : '';
  const status = device.status === 'online' ? 'online' : 'offline';
  return `  ${device.name} (${device.id}) - ${status}${role}`;
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
      description: 'Path to sidecar binary',
      default: './sidecar',
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
    const logger = createCliLogger();

    consola.start(`Starting Truffle dev node: ${args.name} (${deviceId})`);

    const node = createMeshNode({
      deviceId,
      deviceName: args.name,
      deviceType: args.type,
      hostnamePrefix: args.prefix,
      sidecarPath: args.sidecar,
      stateDir: args['state-dir'],
      authKey: args['auth-key'],
      logger,
    });

    node.on('started', () => {
      consola.success('Mesh node started');
      consola.info(`Local device: ${args.name} (${deviceId})`);
      consola.info('Waiting for peers...');
    });

    node.on('devicesChanged', (devices: BaseDevice[]) => {
      consola.info(`Devices (${devices.length}):`);
      for (const device of devices) {
        consola.log(formatDevice(device));
      }
    });

    node.on('deviceDiscovered', (device: BaseDevice) => {
      consola.success(`Discovered: ${device.name} (${device.id})`);
    });

    node.on('deviceOffline', (deviceId: string) => {
      consola.warn(`Offline: ${deviceId}`);
    });

    node.on('roleChanged', (role: DeviceRole, isPrimary: boolean) => {
      consola.info(`Role changed: ${role}${isPrimary ? ' (primary)' : ''}`);
    });

    node.on('authRequired', (authUrl: string) => {
      consola.warn('Authentication required!');
      consola.info(`Open: ${authUrl}`);
    });

    node.on('error', (error: Error) => {
      consola.error('Mesh error:', error.message);
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
