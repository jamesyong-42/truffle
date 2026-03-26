# API Reference

::: warning
This section documents the **old Node.js API** (`@vibecook/truffle`). The v2 architecture (RFC 012) replaced the entire networking stack with a Rust-native `Node` API. The NAPI-RS bindings have not yet been updated. For the current Rust API, see the [Architecture guide](/guide/architecture#node-api-public-entry-point).
:::

## Core Package (Legacy)

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
