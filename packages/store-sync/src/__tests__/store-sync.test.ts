import { describe, it, expect, vi, beforeEach } from 'vitest';
import { EventEmitter } from 'events';
import { StoreSyncAdapter, type ISyncableStore } from '../index.js';
import type { DeviceSlice } from '@vibecook/truffle-types';
import { STORE_SYNC_MESSAGE_TYPES, STORE_SYNC_NAMESPACE } from '@vibecook/truffle-types';
import type { IMessageBus, BusMessage, BusMessageHandler } from '@vibecook/truffle-protocol';

// Quiet logger for tests
const quietLogger = {
  info: () => {},
  warn: () => {},
  error: () => {},
  debug: () => {},
};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

function createMockStore(storeId: string): ISyncableStore & {
  getLocalSlice: ReturnType<typeof vi.fn>;
  applyRemoteSlice: ReturnType<typeof vi.fn>;
  removeRemoteSlice: ReturnType<typeof vi.fn>;
  clearRemoteSlices: ReturnType<typeof vi.fn>;
} {
  const emitter = new EventEmitter();
  return Object.assign(emitter, {
    storeId,
    getLocalSlice: vi.fn<() => DeviceSlice | null>().mockReturnValue(null),
    applyRemoteSlice: vi.fn(),
    removeRemoteSlice: vi.fn(),
    clearRemoteSlices: vi.fn(),
  }) as any;
}

function createMockMessageBus(): IMessageBus & {
  subscribe: ReturnType<typeof vi.fn>;
  publish: ReturnType<typeof vi.fn>;
  broadcast: ReturnType<typeof vi.fn>;
  _handlers: Map<string, BusMessageHandler>;
  _simulateMessage: (msg: BusMessage) => void;
} {
  const handlers = new Map<string, BusMessageHandler>();

  const bus = {
    subscribe: vi.fn((namespace: string, handler: BusMessageHandler) => {
      handlers.set(namespace, handler);
      return () => { handlers.delete(namespace); };
    }),
    publish: vi.fn(),
    broadcast: vi.fn(),
    getSubscribedNamespaces: vi.fn(() => [...handlers.keys()]),
    dispose: vi.fn(),
    _handlers: handlers,
    _simulateMessage: (msg: BusMessage) => {
      const handler = handlers.get(msg.namespace);
      if (handler) handler(msg);
    },
  };

  return bus as any;
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

describe('@vibecook/truffle-store-sync', () => {
  let adapter: StoreSyncAdapter;
  let bus: ReturnType<typeof createMockMessageBus>;
  let store1: ReturnType<typeof createMockStore>;
  let store2: ReturnType<typeof createMockStore>;

  beforeEach(() => {
    bus = createMockMessageBus();
    store1 = createMockStore('tasks');
    store2 = createMockStore('settings');
  });

  function createAdapter() {
    adapter = new StoreSyncAdapter({
      localDeviceId: 'dev-1',
      messageBus: bus,
      stores: [store1, store2],
      logger: quietLogger,
    });
    return adapter;
  }

  describe('lifecycle', () => {
    it('subscribes to sync namespace on start', () => {
      createAdapter();
      adapter.start();

      expect(bus.subscribe).toHaveBeenCalledWith(
        STORE_SYNC_NAMESPACE,
        expect.any(Function)
      );
    });

    it('broadcasts all stores on start', () => {
      const slice: DeviceSlice = {
        deviceId: 'dev-1',
        data: { items: ['a', 'b'] },
        updatedAt: 1000,
        version: 1,
      };
      store1.getLocalSlice.mockReturnValue(slice);

      createAdapter();
      adapter.start();

      // Should broadcast SYNC_FULL for store1 (store2 has no local slice)
      expect(bus.broadcast).toHaveBeenCalledWith(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_FULL,
        expect.objectContaining({
          storeId: 'tasks',
          deviceId: 'dev-1',
          data: { items: ['a', 'b'] },
        })
      );
    });

    it('requests sync from existing devices on start', () => {
      createAdapter();
      adapter.start();

      expect(bus.broadcast).toHaveBeenCalledWith(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_REQUEST,
        expect.objectContaining({ storeId: 'tasks' })
      );
      expect(bus.broadcast).toHaveBeenCalledWith(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_REQUEST,
        expect.objectContaining({ storeId: 'settings' })
      );
    });

    it('clears remote slices on stop', () => {
      createAdapter();
      adapter.start();
      adapter.stop();

      expect(store1.clearRemoteSlices).toHaveBeenCalled();
      expect(store2.clearRemoteSlices).toHaveBeenCalled();
    });

    it('does not start when disposed', () => {
      createAdapter();
      adapter.dispose();
      adapter.start();

      expect(bus.subscribe).not.toHaveBeenCalled();
    });
  });

  describe('incoming sync messages', () => {
    it('handles SYNC_FULL from remote device', () => {
      createAdapter();
      adapter.start();

      bus._simulateMessage({
        from: 'dev-2',
        namespace: STORE_SYNC_NAMESPACE,
        type: STORE_SYNC_MESSAGE_TYPES.SYNC_FULL,
        payload: {
          storeId: 'tasks',
          deviceId: 'dev-2',
          data: { items: ['x'] },
          version: 3,
          updatedAt: 2000,
        },
      });

      expect(store1.applyRemoteSlice).toHaveBeenCalledWith({
        deviceId: 'dev-2',
        data: { items: ['x'] },
        version: 3,
        updatedAt: 2000,
      });
    });

    it('handles SYNC_UPDATE from remote device', () => {
      createAdapter();
      adapter.start();

      bus._simulateMessage({
        from: 'dev-2',
        namespace: STORE_SYNC_NAMESPACE,
        type: STORE_SYNC_MESSAGE_TYPES.SYNC_UPDATE,
        payload: {
          storeId: 'tasks',
          deviceId: 'dev-2',
          data: { items: ['y'] },
          version: 4,
          updatedAt: 3000,
        },
      });

      expect(store1.applyRemoteSlice).toHaveBeenCalledWith({
        deviceId: 'dev-2',
        data: { items: ['y'] },
        version: 4,
        updatedAt: 3000,
      });
    });

    it('ignores SYNC_FULL from self', () => {
      createAdapter();
      adapter.start();

      bus._simulateMessage({
        from: 'dev-1',
        namespace: STORE_SYNC_NAMESPACE,
        type: STORE_SYNC_MESSAGE_TYPES.SYNC_FULL,
        payload: {
          storeId: 'tasks',
          deviceId: 'dev-1',
          data: {},
          version: 1,
          updatedAt: 1000,
        },
      });

      expect(store1.applyRemoteSlice).not.toHaveBeenCalled();
    });

    it('handles SYNC_REQUEST by broadcasting full state', () => {
      const slice: DeviceSlice = {
        deviceId: 'dev-1',
        data: { items: ['local'] },
        updatedAt: 5000,
        version: 5,
      };
      store1.getLocalSlice.mockReturnValue(slice);

      createAdapter();
      adapter.start();

      // Clear calls from start()
      bus.broadcast.mockClear();

      bus._simulateMessage({
        from: 'dev-3',
        namespace: STORE_SYNC_NAMESPACE,
        type: STORE_SYNC_MESSAGE_TYPES.SYNC_REQUEST,
        payload: { storeId: 'tasks' },
      });

      expect(bus.broadcast).toHaveBeenCalledWith(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_FULL,
        expect.objectContaining({
          storeId: 'tasks',
          deviceId: 'dev-1',
          data: { items: ['local'] },
        })
      );
    });

    it('handles targeted SYNC_REQUEST', () => {
      const slice: DeviceSlice = {
        deviceId: 'dev-1',
        data: {},
        updatedAt: 1000,
        version: 1,
      };
      store1.getLocalSlice.mockReturnValue(slice);

      createAdapter();
      adapter.start();
      bus.broadcast.mockClear();

      // Targeted at dev-1 - should respond
      bus._simulateMessage({
        from: 'dev-3',
        namespace: STORE_SYNC_NAMESPACE,
        type: STORE_SYNC_MESSAGE_TYPES.SYNC_REQUEST,
        payload: { storeId: 'tasks', fromDeviceId: 'dev-1' },
      });

      expect(bus.broadcast).toHaveBeenCalled();
    });

    it('ignores SYNC_REQUEST targeted at another device', () => {
      createAdapter();
      adapter.start();
      bus.broadcast.mockClear();

      bus._simulateMessage({
        from: 'dev-3',
        namespace: STORE_SYNC_NAMESPACE,
        type: STORE_SYNC_MESSAGE_TYPES.SYNC_REQUEST,
        payload: { storeId: 'tasks', fromDeviceId: 'dev-99' },
      });

      expect(bus.broadcast).not.toHaveBeenCalled();
    });

    it('handles SYNC_CLEAR from remote device', () => {
      createAdapter();
      adapter.start();

      bus._simulateMessage({
        from: 'dev-2',
        namespace: STORE_SYNC_NAMESPACE,
        type: STORE_SYNC_MESSAGE_TYPES.SYNC_CLEAR,
        payload: {
          storeId: 'tasks',
          deviceId: 'dev-2',
          reason: 'offline',
        },
      });

      expect(store1.removeRemoteSlice).toHaveBeenCalledWith('dev-2', 'offline');
    });

    it('ignores SYNC_CLEAR for self', () => {
      createAdapter();
      adapter.start();

      bus._simulateMessage({
        from: 'dev-2',
        namespace: STORE_SYNC_NAMESPACE,
        type: STORE_SYNC_MESSAGE_TYPES.SYNC_CLEAR,
        payload: {
          storeId: 'tasks',
          deviceId: 'dev-1',
          reason: 'offline',
        },
      });

      expect(store1.removeRemoteSlice).not.toHaveBeenCalled();
    });

    it('ignores messages for unknown stores', () => {
      createAdapter();
      adapter.start();

      // Should not throw
      bus._simulateMessage({
        from: 'dev-2',
        namespace: STORE_SYNC_NAMESPACE,
        type: STORE_SYNC_MESSAGE_TYPES.SYNC_FULL,
        payload: {
          storeId: 'nonexistent',
          deviceId: 'dev-2',
          data: {},
          version: 1,
          updatedAt: 1000,
        },
      });

      expect(store1.applyRemoteSlice).not.toHaveBeenCalled();
      expect(store2.applyRemoteSlice).not.toHaveBeenCalled();
    });
  });

  describe('outgoing sync messages', () => {
    it('broadcasts SYNC_UPDATE on local store change', () => {
      createAdapter();
      adapter.start();
      bus.broadcast.mockClear();

      const slice: DeviceSlice = {
        deviceId: 'dev-1',
        data: { items: ['updated'] },
        updatedAt: 6000,
        version: 6,
      };

      store1.emit('localChanged', slice);

      expect(bus.broadcast).toHaveBeenCalledWith(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_UPDATE,
        expect.objectContaining({
          storeId: 'tasks',
          deviceId: 'dev-1',
          data: { items: ['updated'] },
          version: 6,
        })
      );
    });

    it('does not broadcast after stop', () => {
      createAdapter();
      adapter.start();
      adapter.stop();
      bus.broadcast.mockClear();

      const slice: DeviceSlice = {
        deviceId: 'dev-1',
        data: {},
        updatedAt: 1000,
        version: 1,
      };

      store1.emit('localChanged', slice);

      expect(bus.broadcast).not.toHaveBeenCalled();
    });
  });

  describe('device events', () => {
    it('handleDeviceOffline clears slices and broadcasts SYNC_CLEAR', () => {
      createAdapter();
      adapter.start();
      bus.broadcast.mockClear();

      adapter.handleDeviceOffline('dev-2');

      expect(store1.removeRemoteSlice).toHaveBeenCalledWith('dev-2', 'offline');
      expect(store2.removeRemoteSlice).toHaveBeenCalledWith('dev-2', 'offline');

      expect(bus.broadcast).toHaveBeenCalledWith(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_CLEAR,
        expect.objectContaining({
          storeId: 'tasks',
          deviceId: 'dev-2',
          reason: 'offline',
        })
      );
      expect(bus.broadcast).toHaveBeenCalledWith(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_CLEAR,
        expect.objectContaining({
          storeId: 'settings',
          deviceId: 'dev-2',
          reason: 'offline',
        })
      );
    });

    it('handleDeviceDiscovered broadcasts state and requests sync', () => {
      const slice: DeviceSlice = {
        deviceId: 'dev-1',
        data: { tasks: [] },
        updatedAt: 1000,
        version: 1,
      };
      store1.getLocalSlice.mockReturnValue(slice);

      createAdapter();
      adapter.start();
      bus.broadcast.mockClear();

      adapter.handleDeviceDiscovered('dev-3');

      // Should broadcast full state
      expect(bus.broadcast).toHaveBeenCalledWith(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_FULL,
        expect.objectContaining({
          storeId: 'tasks',
          deviceId: 'dev-1',
        })
      );

      // Should request sync from discovered device
      expect(bus.broadcast).toHaveBeenCalledWith(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_REQUEST,
        expect.objectContaining({
          storeId: 'tasks',
          fromDeviceId: 'dev-3',
        })
      );
    });
  });

  describe('dispose', () => {
    it('stops and prevents restart', () => {
      createAdapter();
      adapter.start();
      adapter.dispose();

      expect(store1.clearRemoteSlices).toHaveBeenCalled();

      // Starting again should be a no-op
      bus.subscribe.mockClear();
      adapter.start();
      expect(bus.subscribe).not.toHaveBeenCalled();
    });

    it('double dispose is safe', () => {
      createAdapter();
      adapter.dispose();
      adapter.dispose(); // should not throw
    });
  });
});
