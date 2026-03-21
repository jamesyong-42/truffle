# Architecture

Truffle is organized as a layered stack. Each layer builds on the one below.

## Layer Diagram

```
┌─────────────────────────────────────────────────┐
│                  Your App                        │
├─────────────────────────────────────────────────┤
│  Layer 5: CLI (truffle up/ls/send/cp/proxy)     │
├─────────────────────────────────────────────────┤
│  Layer 4: HTTP services, file transfer           │
├─────────────────────────────────────────────────┤
│  Layer 3: StoreSyncAdapter                       │
├─────────────────────────────────────────────────┤
│  Layer 2: MessageBus (pub/sub)                   │
├─────────────────────────────────────────────────┤
│  Layer 1: MeshNode (P2P connections, discovery)  │
├─────────────────────────────────────────────────┤
│  Layer 0: WebSocketTransport (connections)       │
├─────────────────────────────────────────────────┤
│  BridgeManager (binary IPC to Go sidecar)        │
├─────────────────────────────────────────────────┤
│  Go Sidecar (embeds Tailscale tsnet)             │
└─────────────────────────────────────────────────┘
```

## Components

### Go Sidecar

A Go binary that embeds Tailscale's `tsnet` library. It handles joining the Tailscale network, listening for connections, and dialing peers. Communicates with the Rust layer via binary-framed IPC (stdin/stdout).

### BridgeManager

Binary-framed TCP bridge between the Go sidecar and Rust. Infrastructure layer that handles process spawning, health monitoring, and message framing.

### WebSocketTransport

Manages WebSocket connections over the Tailscale network. Handles incoming and outgoing connections, heartbeat monitoring, and auto-reconnection with exponential backoff.

### MeshNode

The core coordination layer. Handles:
- **Device discovery**: Announces local device, tracks remote devices via `device-announce` and `device-goodbye` messages
- **P2P connections**: Direct WebSocket connections to every discovered peer
- **Message dispatching**: Routes incoming messages to the right handler

Every node connects directly to every other node. There is no hub, no coordinator, and no single point of failure. This maps naturally onto Tailscale's underlying full-mesh WireGuard topology.

### MessageBus

High-level pub/sub API. Subscribe to namespaced message types and broadcast or send targeted messages. Used by `StoreSyncAdapter` and directly by apps.

### StoreSyncAdapter

Synchronizes application state across devices. Each device maintains its own "slice" of data. When a local slice changes, it broadcasts the update; when a remote update arrives, it applies the new slice.

### HTTP Layer

Reverse proxy, static file hosting, and PWA support. Built on the bridge layer for direct peer-to-peer HTTP.

### File Transfer

Direct P2P file transfer using HTTP PUT over the mesh. Progress reporting, resume support, and integrity verification.

### CLI

The `truffle` command-line tool. A daemon-based architecture where `truffle up` starts a persistent node and subsequent commands (`ls`, `send`, `cp`, `proxy`) communicate with it via a local Unix socket.

## Topology: P2P Full Mesh

Truffle uses a **P2P full-mesh topology**:

```
     A ──────── B
     │ ╲      ╱ │
     │   ╲  ╱   │
     │    ╳     │
     │   ╱  ╲   │
     │ ╱      ╲ │
     C ──────── D
```

1. All devices join the Tailscale network via the Go sidecar
2. Devices discover each other through Tailscale peer lists and `device-announce` messages
3. Every node establishes a direct WebSocket connection to every other node
4. Messages are sent directly between peers -- no relay, no routing
5. If a node goes offline, the remaining nodes continue operating without disruption

This is the simplest possible topology because Tailscale already provides full-mesh encrypted connectivity via WireGuard tunnels. Truffle leverages this directly rather than adding its own routing layer on top.

## Wire Protocol

Truffle uses a binary wire protocol (v3) with typed dispatch:

- **Binary framing**: Length-prefixed frames for efficient parsing
- **Three message types**: `device-announce`, `device-goodbye`, `device-list` (mesh management) plus namespace-based application messages
- **Typed dispatch**: Messages are routed to handlers based on namespace and type

See [RFC 009](https://github.com/jamesyong-42/truffle/blob/main/docs/rfcs/009-wire-protocol-redesign.md) for the full wire protocol specification.
