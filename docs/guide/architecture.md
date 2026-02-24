# Architecture

Truffle is organized as a layered stack, each layer building on the one below.

## Layer Diagram

```
┌─────────────────────────────────────────────────┐
│                  Your App                        │
├─────────────────────────────────────────────────┤
│  Layer 2: MessageBus (pub/sub)                  │
│           StoreSyncAdapter                       │
├─────────────────────────────────────────────────┤
│  Layer 1: MeshNode (routing, election, discovery)│
├─────────────────────────────────────────────────┤
│  Layer 0: WebSocketTransport (connections)       │
├─────────────────────────────────────────────────┤
│  SidecarClient (IPC to Go binary)               │
├─────────────────────────────────────────────────┤
│  Go Sidecar (embeds Tailscale tsnet)            │
└─────────────────────────────────────────────────┘
```

## Components

### Go Sidecar

A Go binary that embeds Tailscale's `tsnet` library. It handles joining the Tailscale network, listening for connections, and dialing peers. Communicates with the Node.js layer via IPC (stdin/stdout JSON messages).

### SidecarClient

TypeScript client that spawns and manages the Go sidecar process. Sends commands (dial, send, get peers) and receives events (connect, disconnect, data, peers list).

### WebSocketTransport

Manages WebSocket connections over the Tailscale network. Handles incoming and outgoing connections, heartbeat monitoring, and auto-reconnection with exponential backoff.

### MeshNode

The core coordination layer. Handles:
- **Device discovery**: Announces local device, tracks remote devices
- **Primary election**: Bully-style election to choose one primary node
- **STAR routing**: Primary acts as message router between secondaries
- **Message dispatching**: Routes incoming messages to the right handler

### MessageBus

High-level pub/sub API. Subscribe to namespaced message types and broadcast or send targeted messages. Used by `StoreSyncAdapter` and directly by apps.

### StoreSyncAdapter

Synchronizes application state across devices. Each device maintains its own "slice" of data. When a local slice changes, it broadcasts the update; when a remote update arrives, it applies the new slice.

## Topology

Truffle uses a **STAR topology**:

1. All devices join the Tailscale network via the sidecar
2. Devices discover each other through Tailscale peer lists
3. An election selects one **primary** node
4. All **secondary** nodes connect to the primary
5. Messages between secondaries are routed through the primary
6. If the primary goes offline, a new election triggers automatically
