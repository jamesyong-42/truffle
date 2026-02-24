/**
 * StoreSyncAdapter - Cross-device state synchronization
 *
 * Generic adapter that connects ISyncableStore implementations to an IMessageBus.
 * Works with any store shape - consumers define their own store implementations.
 */

import { EventEmitter } from 'events';
import type {
  DeviceSlice,
  SyncFullPayload,
  SyncUpdatePayload,
  SyncRequestPayload,
  SyncClearPayload,
  Logger,
} from '@vibecook/truffle-types';
import {
  STORE_SYNC_MESSAGE_TYPES,
  STORE_SYNC_NAMESPACE,
  createLogger,
} from '@vibecook/truffle-types';
import type { BusMessage, IMessageBus } from '@vibecook/truffle-protocol';

// ═══════════════════════════════════════════════════════════════════════════
// SYNCABLE STORE INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Interface that stores must implement to be syncable.
 * Consumers provide their own store implementations.
 */
export interface ISyncableStore<T = unknown> extends EventEmitter {
  /** Unique store identifier */
  readonly storeId: string;

  /** Get the local device's slice */
  getLocalSlice(): DeviceSlice<T> | null;

  /** Apply a remote device's slice */
  applyRemoteSlice(slice: DeviceSlice<T>): void;

  /** Remove a remote device's slice */
  removeRemoteSlice(deviceId: string, reason: string): void;

  /** Clear all remote slices */
  clearRemoteSlices(): void;

  // Events
  on(event: 'localChanged', listener: (slice: DeviceSlice<T>) => void): this;
  off(event: 'localChanged', listener: (slice: DeviceSlice<T>) => void): this;
}

// ═══════════════════════════════════════════════════════════════════════════
// STORE SYNC ADAPTER
// ═══════════════════════════════════════════════════════════════════════════

export interface StoreSyncAdapterConfig {
  /** Local device ID */
  localDeviceId: string;
  /** Message bus for communication */
  messageBus: IMessageBus;
  /** Stores to sync */
  stores: ISyncableStore[];
  /** Optional logger */
  logger?: Logger;
}

export class StoreSyncAdapter {
  private readonly localDeviceId: string;
  private readonly messageBus: IMessageBus;
  private readonly stores: Map<string, ISyncableStore>;
  private readonly log: Logger;
  private unsubscribeMessages: (() => void) | null = null;
  private storeListeners = new Map<string, () => void>();
  private disposed = false;

  constructor(config: StoreSyncAdapterConfig) {
    this.localDeviceId = config.localDeviceId;
    this.messageBus = config.messageBus;
    this.stores = new Map(config.stores.map((s) => [s.storeId, s]));
    this.log = config.logger ?? createLogger('StoreSyncAdapter');
  }

  // ─────────────────────────────────────────────────────────────────────────
  // LIFECYCLE
  // ─────────────────────────────────────────────────────────────────────────

  /**
   * Start syncing. Subscribes to message bus and store events.
   */
  start(): void {
    if (this.disposed) {
      this.log.warn('start: SKIPPED - disposed');
      return;
    }

    // Subscribe to sync messages via MessageBus
    this.unsubscribeMessages = this.messageBus.subscribe(
      STORE_SYNC_NAMESPACE,
      this.handleSyncMessage.bind(this),
    );

    // Subscribe to store 'localChanged' events
    this.setupStoreListeners();

    // Request sync from existing devices
    this.requestSyncFromDevices();

    // Broadcast our current state
    this.broadcastAllStores();

    this.log.info(`Started with ${this.stores.size} stores`);
  }

  /**
   * Stop syncing. Clears remote slices from all stores.
   */
  stop(): void {
    this.removeStoreListeners();

    if (this.unsubscribeMessages) {
      this.unsubscribeMessages();
      this.unsubscribeMessages = null;
    }

    for (const store of this.stores.values()) {
      store.clearRemoteSlices();
    }

    this.log.info('Stopped');
  }

  /**
   * Dispose the adapter.
   */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.stop();
    this.log.info('Disposed');
  }

  // ─────────────────────────────────────────────────────────────────────────
  // DEVICE EVENTS
  // ─────────────────────────────────────────────────────────────────────────

  /**
   * Call when a device goes offline to clear its slices.
   */
  handleDeviceOffline(deviceId: string): void {
    this.log.info(`Device ${deviceId} went offline`);

    for (const store of this.stores.values()) {
      store.removeRemoteSlice(deviceId, 'offline');
    }

    // Broadcast SYNC_CLEAR for all stores
    for (const [storeId] of this.stores) {
      const payload: SyncClearPayload = {
        storeId,
        deviceId,
        reason: 'offline',
      };
      this.messageBus.broadcast(STORE_SYNC_NAMESPACE, STORE_SYNC_MESSAGE_TYPES.SYNC_CLEAR, payload);
    }
  }

  /**
   * Call when a new device is discovered to sync state.
   */
  handleDeviceDiscovered(deviceId: string): void {
    this.log.info(`Device ${deviceId} discovered`);
    this.broadcastAllStores();

    for (const [storeId] of this.stores) {
      const payload: SyncRequestPayload = {
        storeId,
        fromDeviceId: deviceId,
      };
      this.messageBus.broadcast(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_REQUEST,
        payload,
      );
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - SETUP
  // ─────────────────────────────────────────────────────────────────────────

  private setupStoreListeners(): void {
    for (const [storeId, store] of this.stores) {
      const listener = (slice: DeviceSlice<unknown>) => {
        this.handleLocalChanged(storeId, slice);
      };

      store.on('localChanged', listener);
      this.storeListeners.set(storeId, () => store.off('localChanged', listener));
    }
  }

  private removeStoreListeners(): void {
    for (const [, cleanup] of this.storeListeners) {
      cleanup();
    }
    this.storeListeners.clear();
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - MESSAGE HANDLING
  // ─────────────────────────────────────────────────────────────────────────

  private handleSyncMessage(message: BusMessage): void {
    const payload = message.payload as { storeId?: string };
    if (!payload?.storeId) return;

    const store = this.stores.get(payload.storeId);
    if (!store) {
      this.log.warn(`Unknown store in sync message: ${payload.storeId}`);
      return;
    }

    switch (message.type) {
      case STORE_SYNC_MESSAGE_TYPES.SYNC_FULL:
        this.handleSyncFull(store, message.from, message.payload as SyncFullPayload<unknown>);
        break;
      case STORE_SYNC_MESSAGE_TYPES.SYNC_UPDATE:
        this.handleSyncUpdate(store, message.from, message.payload as SyncUpdatePayload<unknown>);
        break;
      case STORE_SYNC_MESSAGE_TYPES.SYNC_REQUEST:
        this.handleSyncRequest(message.from, message.payload as SyncRequestPayload);
        break;
      case STORE_SYNC_MESSAGE_TYPES.SYNC_CLEAR:
        this.handleSyncClear(store, message.payload as SyncClearPayload);
        break;
    }
  }

  private handleSyncFull(
    store: ISyncableStore,
    from: string | undefined,
    payload: SyncFullPayload<unknown>,
  ): void {
    if (!from || from === this.localDeviceId) return;

    const slice: DeviceSlice<unknown> = {
      deviceId: payload.deviceId,
      data: payload.data,
      updatedAt: payload.updatedAt,
      version: payload.version,
    };

    store.applyRemoteSlice(slice);
  }

  private handleSyncUpdate(
    store: ISyncableStore,
    from: string | undefined,
    payload: SyncUpdatePayload<unknown>,
  ): void {
    if (!from || from === this.localDeviceId) return;

    const slice: DeviceSlice<unknown> = {
      deviceId: payload.deviceId,
      data: payload.data,
      updatedAt: payload.updatedAt,
      version: payload.version,
    };

    store.applyRemoteSlice(slice);
  }

  private handleSyncRequest(from: string | undefined, payload: SyncRequestPayload): void {
    if (!from || from === this.localDeviceId) return;

    if (payload.fromDeviceId && payload.fromDeviceId !== this.localDeviceId) {
      return;
    }

    const store = this.stores.get(payload.storeId);
    if (store) {
      this.broadcastStoreFull(payload.storeId, store);
    }
  }

  private handleSyncClear(store: ISyncableStore, payload: SyncClearPayload): void {
    if (payload.deviceId === this.localDeviceId) return;
    store.removeRemoteSlice(payload.deviceId, payload.reason ?? 'unknown');
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - BROADCASTING
  // ─────────────────────────────────────────────────────────────────────────

  private handleLocalChanged(storeId: string, slice: DeviceSlice<unknown>): void {
    this.log.debug(`Broadcasting local change for ${storeId}: v${slice.version}`);

    const payload: SyncUpdatePayload<unknown> = {
      storeId,
      deviceId: slice.deviceId,
      data: slice.data,
      version: slice.version,
      updatedAt: slice.updatedAt,
    };

    this.messageBus.broadcast(STORE_SYNC_NAMESPACE, STORE_SYNC_MESSAGE_TYPES.SYNC_UPDATE, payload);
  }

  private broadcastStoreFull(storeId: string, store: ISyncableStore): void {
    const localSlice = store.getLocalSlice();
    if (!localSlice) return;

    const payload: SyncFullPayload<unknown> = {
      storeId,
      deviceId: localSlice.deviceId,
      data: localSlice.data,
      version: localSlice.version,
      updatedAt: localSlice.updatedAt,
    };

    this.messageBus.broadcast(STORE_SYNC_NAMESPACE, STORE_SYNC_MESSAGE_TYPES.SYNC_FULL, payload);
  }

  private broadcastAllStores(): void {
    for (const [storeId, store] of this.stores) {
      this.broadcastStoreFull(storeId, store);
    }
  }

  private requestSyncFromDevices(): void {
    for (const [storeId] of this.stores) {
      const payload: SyncRequestPayload = { storeId };
      this.messageBus.broadcast(
        STORE_SYNC_NAMESPACE,
        STORE_SYNC_MESSAGE_TYPES.SYNC_REQUEST,
        payload,
      );
    }
  }
}
