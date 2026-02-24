# Store Sync

Truffle provides cross-device state synchronization through the `StoreSyncAdapter` and `ISyncableStore` interface.

## How It Works

1. Each device maintains its own **slice** of data
2. When local data changes, the store emits `localChanged`
3. `StoreSyncAdapter` broadcasts the slice to all peers
4. Remote slices are applied to the store via `applyRemoteSlice()`
5. When a device goes offline, its slice is removed

## Implementing a Store

```typescript
import { EventEmitter } from 'node:events';
import type { DeviceSlice } from '@vibecook/truffle';
import type { ISyncableStore } from '@vibecook/truffle';

interface TodoState {
  items: string[];
}

class TodoStore extends EventEmitter implements ISyncableStore<TodoState> {
  readonly storeId = 'todos';
  private localSlice: DeviceSlice<TodoState> | null = null;
  private remoteSlices = new Map<string, DeviceSlice<TodoState>>();

  init(deviceId: string) {
    this.localSlice = {
      deviceId,
      data: { items: [] },
      version: 0,
      updatedAt: Date.now(),
    };
  }

  addItem(text: string) {
    if (!this.localSlice) return;
    this.localSlice = {
      ...this.localSlice,
      data: { items: [...this.localSlice.data.items, text] },
      version: this.localSlice.version + 1,
      updatedAt: Date.now(),
    };
    this.emit('localChanged', this.localSlice);
  }

  getLocalSlice(): DeviceSlice<TodoState> | null {
    return this.localSlice;
  }

  applyRemoteSlice(slice: DeviceSlice<TodoState>) {
    this.remoteSlices.set(slice.deviceId, slice);
  }

  removeRemoteSlice(deviceId: string) {
    this.remoteSlices.delete(deviceId);
  }

  clearRemoteSlices() {
    this.remoteSlices.clear();
  }
}
```

## Wiring Up

```typescript
import { createMeshNode, StoreSyncAdapter } from '@vibecook/truffle';

const node = createMeshNode({ /* config */ });
const store = new TodoStore();

await node.start();
store.init(node.getLocalDevice().id);

const sync = new StoreSyncAdapter({
  localDeviceId: node.getLocalDevice().id,
  messageBus: node.getMessageBus(),
  stores: [store],
});

sync.start();

// Now any changes to store will sync across devices
store.addItem('Buy groceries');
```

## React Integration

```tsx
import { useSyncedStore } from '@vibecook/truffle-react';

function TodoList({ store }: { store: TodoStore }) {
  const { localData, version } = useSyncedStore(store);

  return (
    <div>
      <h2>Todos (v{version})</h2>
      <ul>
        {localData?.items.map((item, i) => (
          <li key={i}>{item}</li>
        ))}
      </ul>
    </div>
  );
}
```
