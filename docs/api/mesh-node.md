# MeshNode (Legacy)

::: warning
This documents the **old Node.js API**. The v2 `Node` struct in Rust replaces `MeshNode`. See the [Architecture guide](/guide/architecture#node-api-public-entry-point) for the current API.
:::

The central coordination class for Truffle mesh networking.

## `createMeshNode(config)`

Factory function to create a new `MeshNode`.

```typescript
import { createMeshNode } from '@vibecook/truffle';

const node = createMeshNode({
  deviceId: 'my-device',
  deviceName: 'My Laptop',
  deviceType: 'desktop',
  hostnamePrefix: 'myapp',
  sidecarPath: './sidecar',
  stateDir: './.state',
});
```

### MeshNodeConfig

```typescript
interface MeshNodeConfig {
  deviceId: string;
  deviceName: string;
  deviceType: string;
  hostnamePrefix: string;
  sidecarPath: string;
  stateDir: string;
  authKey?: string;
  staticPath?: string;
  capabilities?: string[];
  metadata?: Record<string, unknown>;
  logger?: Logger;
  timing?: MeshTimingConfig;
}
```

## Methods

### `start(): Promise<void>`

Start the mesh node. Spawns the sidecar, joins the tailnet, and begins peer discovery.

### `stop(): Promise<void>`

Stop the mesh node and clean up connections.

### `isRunning(): boolean`

Returns whether the node is currently active.

### `getLocalDevice(): BaseDevice`

Returns the local device info.

### `getDevices(): BaseDevice[]`

Returns all known remote devices.

### `getMessageBus(): IMessageBus`

Returns the message bus for pub/sub messaging.

## Events

```typescript
interface MeshNodeEvents {
  started: () => void;
  stopped: () => void;
  authRequired: (authUrl: string) => void;
  deviceDiscovered: (device: BaseDevice) => void;
  deviceUpdated: (device: BaseDevice) => void;
  deviceOffline: (deviceId: string) => void;
  devicesChanged: (devices: BaseDevice[]) => void;
  message: (message: IncomingMeshMessage) => void;
  error: (error: Error) => void;
}
```
