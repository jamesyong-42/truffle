# API Reference

## Core Package

Install `@vibecook/truffle` to get all the core exports.

```bash
npm install @vibecook/truffle
```

### Exports

| Export | Source Package | Description |
|--------|---------------|-------------|
| `createMeshNode` | truffle-mesh | Create a new mesh node |
| `MeshNode` | truffle-mesh | Mesh node class |
| `DeviceManager` | truffle-mesh | Device state manager |
| `PrimaryElection` | truffle-mesh | Election coordinator |
| `MeshMessageBus` | truffle-mesh | Message bus implementation |
| `StoreSyncAdapter` | truffle-store-sync | Store synchronization adapter |
| `createLogger` | truffle-types | Default logger factory |
| `TypedEventEmitter` | truffle-types | Type-safe event emitter base |

### Types

| Type | Description |
|------|-------------|
| `MeshNodeConfig` | Configuration for creating a mesh node |
| `MeshNodeEvents` | Event map for MeshNode |
| `MeshTimingConfig` | Timing configuration |
| `BaseDevice` | Device information |
| `DeviceRole` | `'primary' \| 'secondary'` |
| `DeviceSlice<T>` | Store slice with data, version, timestamps |
| `MeshEnvelope` | Wire format for mesh messages |
| `ISyncableStore<T>` | Interface for syncable stores |
| `IMessageBus` | Interface for the message bus |
| `BusMessage` | Message received from the bus |
| `Logger` | Logger interface |

## Additional Packages

| Package | Description |
|---------|-------------|
| [`@vibecook/truffle-react`](/api/react) | React hooks |
| [`@vibecook/truffle-cli`](https://github.com/jamesyong-42/truffle/tree/main/packages/cli) | CLI tool |
