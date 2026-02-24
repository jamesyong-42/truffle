import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { EventEmitter } from 'events';
import { WebSocketTransport } from '../index.js';
import type { ISidecarClient } from '@vibecook/truffle-sidecar-client';

function createMockSidecar(): ISidecarClient {
  const emitter = new EventEmitter();
  return Object.assign(emitter, {
    start: vi.fn().mockResolvedValue(undefined),
    stop: vi.fn().mockResolvedValue(undefined),
    getStatus: vi.fn().mockReturnValue({ state: 'running' }),
    isRunning: vi.fn().mockReturnValue(true),
    sendToConnection: vi.fn(),
    dial: vi.fn(),
    dialClose: vi.fn(),
    dialSend: vi.fn(),
    getTailnetPeers: vi.fn(),
  }) as unknown as ISidecarClient;
}

describe('@vibecook/truffle-transport - WebSocketTransport', () => {
  let transport: WebSocketTransport;
  let sidecar: ISidecarClient;

  beforeEach(() => {
    vi.useFakeTimers();
    sidecar = createMockSidecar();
    transport = new WebSocketTransport(sidecar);
  });

  afterEach(() => {
    if (transport.isRunning()) {
      transport.stop();
    }
    vi.useRealTimers();
  });

  describe('lifecycle', () => {
    it('starts and stops', () => {
      transport.start();
      expect(transport.isRunning()).toBe(true);

      transport.stop();
      expect(transport.isRunning()).toBe(false);
    });

    it('throws if started twice', () => {
      transport.start();
      expect(() => transport.start()).toThrow('Transport already running');
    });
  });

  describe('incoming connections', () => {
    it('creates connection on wsConnect', () => {
      transport.start();

      sidecar.emit('wsConnect', 'conn-1', '100.64.0.2:443');

      const connections = transport.getConnections();
      expect(connections).toHaveLength(1);
      expect(connections[0].id).toBe('incoming:conn-1');
      expect(connections[0].direction).toBe('incoming');
      expect(connections[0].status).toBe('connected');
    });

    it('emits data events on wsMessage', () => {
      transport.start();

      sidecar.emit('wsConnect', 'conn-1', '100.64.0.2:443');

      const dataHandler = vi.fn();
      transport.on('data', dataHandler);

      sidecar.emit('wsMessage', 'conn-1', '{"test":"data"}');

      expect(dataHandler).toHaveBeenCalledWith('incoming:conn-1', '{"test":"data"}');
    });

    it('handles wsDisconnect', () => {
      transport.start();

      sidecar.emit('wsConnect', 'conn-1', '100.64.0.2:443');

      const disconnectHandler = vi.fn();
      transport.on('disconnected', disconnectHandler);

      sidecar.emit('wsDisconnect', 'conn-1', 'remote_closed');

      expect(disconnectHandler).toHaveBeenCalledWith('incoming:conn-1', 'remote_closed');
      expect(transport.getConnections()).toHaveLength(0);
    });
  });

  describe('outgoing connections', () => {
    it('creates connection on dial', async () => {
      transport.start();

      const connectPromise = transport.connect('dev-2', 'myapp-desktop-dev-2');

      // Simulate sidecar dial response
      sidecar.emit('dialConnected', 'dev-2', '100.64.0.2:443');

      const conn = await connectPromise;
      expect(conn.deviceId).toBe('dev-2');
      expect(conn.direction).toBe('outgoing');
      expect(conn.status).toBe('connected');
    });

    it('emits data on dial message', async () => {
      transport.start();

      const connectPromise = transport.connect('dev-2', 'myapp-desktop-dev-2');
      sidecar.emit('dialConnected', 'dev-2', '100.64.0.2:443');
      await connectPromise;

      const dataHandler = vi.fn();
      transport.on('data', dataHandler);

      sidecar.emit('dialMessage', 'dev-2', '{"hello":"world"}');

      expect(dataHandler).toHaveBeenCalledWith('dial:dev-2', '{"hello":"world"}');
    });

    it('handles dial disconnect', async () => {
      transport.start();

      const connectPromise = transport.connect('dev-2', 'myapp-desktop-dev-2');
      sidecar.emit('dialConnected', 'dev-2', '100.64.0.2:443');
      await connectPromise;

      const disconnectHandler = vi.fn();
      transport.on('disconnected', disconnectHandler);

      sidecar.emit('dialDisconnect', 'dev-2', 'peer_gone');

      expect(disconnectHandler).toHaveBeenCalledWith('dial:dev-2', 'peer_gone');
    });

    it('rejects on dial error', async () => {
      transport.start();

      const connectPromise = transport.connect('dev-2', 'myapp-desktop-dev-2');
      sidecar.emit('dialError', 'dev-2', 'connection refused');

      await expect(connectPromise).rejects.toThrow('connection refused');
    });

    it('returns existing connection if already connected', async () => {
      transport.start();

      const p1 = transport.connect('dev-2', 'myapp-desktop-dev-2');
      sidecar.emit('dialConnected', 'dev-2', '100.64.0.2:443');
      const conn1 = await p1;

      const conn2 = await transport.connect('dev-2', 'myapp-desktop-dev-2');
      expect(conn2.id).toBe(conn1.id);
    });
  });

  describe('sendRaw', () => {
    it('sends via dialSend for outgoing', async () => {
      transport.start();

      const p = transport.connect('dev-2', 'myapp-desktop-dev-2');
      sidecar.emit('dialConnected', 'dev-2', '100.64.0.2:443');
      await p;

      transport.sendRaw('dial:dev-2', 'test-data');
      expect(sidecar.dialSend).toHaveBeenCalledWith('dev-2', 'test-data');
    });

    it('sends via sendToConnection for incoming', () => {
      transport.start();

      sidecar.emit('wsConnect', 'conn-1', '100.64.0.2:443');

      transport.sendRaw('incoming:conn-1', 'test-data');
      expect(sidecar.sendToConnection).toHaveBeenCalledWith('conn-1', 'test-data');
    });

    it('returns false for unknown connection', () => {
      transport.start();
      expect(transport.sendRaw('nonexistent', 'data')).toBe(false);
    });
  });

  describe('connection management', () => {
    it('getConnectionByDeviceId works after setConnectionDeviceId', () => {
      transport.start();

      sidecar.emit('wsConnect', 'conn-1', '100.64.0.2:443');

      transport.setConnectionDeviceId('incoming:conn-1', 'dev-2');

      const conn = transport.getConnectionByDeviceId('dev-2');
      expect(conn).not.toBeNull();
      expect(conn!.id).toBe('incoming:conn-1');
      expect(conn!.deviceId).toBe('dev-2');
    });

    it('closeConnection cleans up', async () => {
      transport.start();

      const p = transport.connect('dev-2', 'myapp-desktop-dev-2');
      sidecar.emit('dialConnected', 'dev-2', '100.64.0.2:443');
      await p;

      transport.closeConnection('dial:dev-2');

      expect(transport.getConnections()).toHaveLength(0);
      expect(transport.getConnectionByDeviceId('dev-2')).toBeNull();
      expect(sidecar.dialClose).toHaveBeenCalledWith('dev-2');
    });
  });

  describe('heartbeat', () => {
    it('handles ping/pong internally', () => {
      transport.start();

      sidecar.emit('wsConnect', 'conn-1', '100.64.0.2:443');

      const dataHandler = vi.fn();
      transport.on('data', dataHandler);

      // Send a ping - should be handled internally, not emitted as data
      sidecar.emit('wsMessage', 'conn-1', JSON.stringify({ type: 'ping', timestamp: Date.now() }));

      expect(dataHandler).not.toHaveBeenCalled();
      // Should have sent a pong back
      expect(sidecar.sendToConnection).toHaveBeenCalledWith(
        'conn-1',
        expect.stringContaining('"type":"pong"')
      );
    });

    it('does not intercept non-heartbeat messages', () => {
      transport.start();

      sidecar.emit('wsConnect', 'conn-1', '100.64.0.2:443');

      const dataHandler = vi.fn();
      transport.on('data', dataHandler);

      sidecar.emit('wsMessage', 'conn-1', '{"namespace":"sync","type":"update"}');

      expect(dataHandler).toHaveBeenCalledWith('incoming:conn-1', '{"namespace":"sync","type":"update"}');
    });
  });
});
