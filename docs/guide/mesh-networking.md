# Mesh Networking

## Creating a MeshNode

```typescript
import { createMeshNode } from '@vibecook/truffle';

const node = createMeshNode({
  deviceId: 'unique-id',
  deviceName: 'My Device',
  deviceType: 'desktop',
  hostnamePrefix: 'myapp',
  sidecarPath: './sidecar',
  stateDir: './.truffle-state',
});
```

## Configuration

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `deviceId` | `string` | Yes | Unique device identifier |
| `deviceName` | `string` | Yes | Human-readable name |
| `deviceType` | `string` | Yes | Type (desktop, mobile, server) |
| `hostnamePrefix` | `string` | Yes | Peer filter prefix on tailnet |
| `sidecarPath` | `string` | Yes | Path to sidecar binary |
| `stateDir` | `string` | Yes | Tailscale state directory |
| `authKey` | `string` | No | Tailscale auth key |
| `preferPrimary` | `boolean` | No | Prefer this device as primary |
| `logger` | `Logger` | No | Custom logger |
| `timing` | `MeshTimingConfig` | No | Timing overrides |

## Timing Configuration

```typescript
const node = createMeshNode({
  // ... required fields
  timing: {
    announceIntervalMs: 30000,   // Device announce interval
    discoveryTimeoutMs: 5000,    // Peer discovery timeout
    electionTimeoutMs: 3000,     // Election round timeout
    primaryLossGraceMs: 5000,    // Grace period before re-election
    heartbeatPingMs: 2000,       // Heartbeat ping interval
    heartbeatTimeoutMs: 5000,    // Heartbeat timeout
  },
});
```

## Events

```typescript
node.on('started', () => { /* Node is running */ });
node.on('stopped', () => { /* Node stopped */ });
node.on('deviceDiscovered', (device) => { /* New peer found */ });
node.on('deviceOffline', (deviceId) => { /* Peer went offline */ });
node.on('devicesChanged', (devices) => { /* Device list updated */ });
node.on('roleChanged', (role, isPrimary) => { /* Role changed */ });
node.on('authRequired', (authUrl) => { /* Tailscale auth needed */ });
node.on('error', (error) => { /* Error occurred */ });
```

## Using the Message Bus

```typescript
const bus = node.getMessageBus();

// Subscribe to messages in a namespace
const unsubscribe = bus.subscribe('chat', (message) => {
  console.log(`${message.from}: ${message.type}`, message.payload);
});

// Broadcast to all devices
bus.broadcast('chat', 'message', { text: 'Hello everyone!' });

// Send to a specific device
bus.publish('device-id', 'chat', 'message', { text: 'Hello!' });

// Unsubscribe
unsubscribe();
```

## Lifecycle

```typescript
// Start the node
await node.start();

// Check status
console.log(node.isRunning());     // true
console.log(node.isPrimary());     // true/false
console.log(node.getRole());       // 'primary' | 'secondary'
console.log(node.getDevices());    // BaseDevice[]
console.log(node.getLocalDevice()); // BaseDevice

// Stop the node
await node.stop();
```
