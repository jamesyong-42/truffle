# React Hooks

```bash
npm install @vibecook/truffle-react
```

Peer dependency: `react >= 18`

## useMesh

Subscribe to mesh node state.

```typescript
import { useMesh } from '@vibecook/truffle-react';

function MeshStatus({ node }: { node: MeshNode | null }) {
  const {
    devices,
    localDevice,
    isPrimary,
    isConnected,
    role,
    broadcast,
    sendTo,
  } = useMesh(node);

  return (
    <div>
      <p>Status: {isConnected ? 'Connected' : 'Disconnected'}</p>
      <p>Role: {role} {isPrimary && '(primary)'}</p>
      <p>Devices: {devices.length}</p>
      <ul>
        {devices.map(d => (
          <li key={d.id}>{d.name} - {d.status}</li>
        ))}
      </ul>
    </div>
  );
}
```

### Parameters

| Param | Type | Description |
|-------|------|-------------|
| `node` | `MeshNode \| null` | The mesh node instance (pass `null` before initialization) |

### Return Value

| Field | Type | Description |
|-------|------|-------------|
| `devices` | `BaseDevice[]` | All known remote devices |
| `localDevice` | `BaseDevice \| null` | Local device info |
| `isPrimary` | `boolean` | Whether this node is primary |
| `isConnected` | `boolean` | Whether the node is running |
| `role` | `DeviceRole` | Current role |
| `broadcast` | `(ns, type, payload) => void` | Broadcast a message |
| `sendTo` | `(deviceId, ns, type, payload) => boolean` | Send to a device |

## useSyncedStore

Subscribe to a synced store's local data.

```typescript
import { useSyncedStore } from '@vibecook/truffle-react';

function DataView({ store }: { store: ISyncableStore<MyData> }) {
  const { localData, localSlice, version } = useSyncedStore(store);

  if (!localData) return <p>No data</p>;

  return (
    <div>
      <p>Version: {version}</p>
      <pre>{JSON.stringify(localData, null, 2)}</pre>
    </div>
  );
}
```

### Parameters

| Param | Type | Description |
|-------|------|-------------|
| `store` | `ISyncableStore<T> \| null` | The syncable store instance |

### Return Value

| Field | Type | Description |
|-------|------|-------------|
| `localSlice` | `DeviceSlice<T> \| null` | Full local slice |
| `localData` | `T \| null` | Shortcut to `localSlice.data` |
| `version` | `number` | Current version (0 if no slice) |
