# StoreSyncAdapter

Synchronizes `ISyncableStore` instances across devices using the message bus.

## Usage

```typescript
import { StoreSyncAdapter } from '@vibecook/truffle';

const sync = new StoreSyncAdapter({
  localDeviceId: node.getLocalDevice().id,
  messageBus: node.getMessageBus(),
  stores: [myStore],
});

sync.start();
```

## StoreSyncAdapterConfig

```typescript
interface StoreSyncAdapterConfig {
  localDeviceId: string;
  messageBus: IMessageBus;
  stores: ISyncableStore[];
  logger?: Logger;
}
```

## ISyncableStore Interface

Implement this interface to make your store syncable:

```typescript
interface ISyncableStore<T = unknown> extends EventEmitter {
  readonly storeId: string;
  getLocalSlice(): DeviceSlice<T> | null;
  applyRemoteSlice(slice: DeviceSlice<T>): void;
  removeRemoteSlice(deviceId: string, reason: string): void;
  clearRemoteSlices(): void;

  // Events
  on(event: 'localChanged', listener: (slice: DeviceSlice<T>) => void): this;
  off(event: 'localChanged', listener: (slice: DeviceSlice<T>) => void): this;
}
```

## DeviceSlice

```typescript
interface DeviceSlice<T = unknown> {
  deviceId: string;
  data: T;
  updatedAt: number;
  version: number;
}
```
