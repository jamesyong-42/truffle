# Architecture

Truffle uses a clean 7-layer architecture (RFC 012), built bottom-up with trait boundaries at each layer. The old codebase (~40k LOC, 4 crates) was replaced with ~15k LOC across 2 Rust crates + a Go sidecar.

## Layer Diagram

```
┌─────────────────────────────────────────────────────┐
│  Layer 7: Applications                               │
│  CLI commands — up, ls, ping, cp, send, tcp, doctor  │
├─────────────────────────────────────────────────────┤
│  Node API — 15-method public entry point             │
│  (the only import applications need)                 │
├─────────────────────────────────────────────────────┤
│  Layer 6: Envelope                                   │
│  Namespace-routed message framing (EnvelopeCodec)    │
├─────────────────────────────────────────────────────┤
│  Layer 5: Session                                    │
│  PeerRegistry, lazy connections, message fan-out     │
├─────────────────────────────────────────────────────┤
│  Layer 4: Transport                                  │
│  WebSocket, TCP, UDP, QUIC protocols                 │
├─────────────────────────────────────────────────────┤
│  Layer 3: Network                                    │
│  TailscaleProvider, WatchIPNBus peer discovery       │
├─────────────────────────────────────────────────────┤
│  Go Sidecar (~1.8k LOC)                              │
│  tsnet integration, encrypted WireGuard tunnels      │
└─────────────────────────────────────────────────────┘
```

## Components

### Go Sidecar (Layer 3 infrastructure)

A Go binary (~1.8k LOC) that embeds Tailscale's `tsnet` library. It handles:
- Joining the Tailscale network and obtaining a stable node identity
- WatchIPNBus — streaming peer discovery events (peers known before any transport)
- TLS listener on port 443 for incoming WebSocket connections
- TCP listener on port 9417 for direct TCP mesh connections
- UDP relay via `ListenPacket` for QUIC support

Communicates with Rust via JSON lines on stdin/stdout.

### Layer 3: Network (`TailscaleProvider`)

Wraps the Go sidecar process and provides a `NetworkProvider` trait:
- `start()` / `stop()` lifecycle
- `local_identity()` — node ID, IP, DNS name
- `peers()` — current peer list from WatchIPNBus
- `ping()` — Tailscale-level connectivity check (direct or DERP relay)
- `health()` — sidecar and Tailscale health diagnostics

Peer discovery is passive — peers appear via WatchIPNBus events without any transport connections.

### Layer 4: Transport

Four transport implementations behind a common trait interface:

| Transport | Port | Use Case |
|-----------|------|----------|
| **WebSocket** | 443 (TLS) | Primary message transport, namespace routing |
| **TCP** | 9417 | Raw byte streams (like netcat), file transfer |
| **UDP** | dynamic | Low-latency datagrams via tsnet relay |
| **QUIC** | dynamic | Multiplexed streams over UDP (via quinn) |

All four transports verified working cross-machine over real Tailscale (macOS <-> EC2 Linux).

### Layer 5: Session (`PeerRegistry`)

Manages the peer lifecycle and message routing:
- Tracks peer state (discovered, connected, disconnected)
- Lazy connection establishment — connects on first message send
- Auto-reconnection with exponential backoff
- Fan-out broadcast to all connected peers
- Concurrent send protection (serialized writes per peer)

### Layer 6: Envelope (`EnvelopeCodec`)

Namespace-based message framing:
- Messages carry `namespace`, `msg_type`, and JSON `payload`
- Codec serializes/deserializes envelopes over any Layer 4 transport
- Enables multiple independent protocols over a single connection

### Node API (public entry point)

The `Node` struct is the **only** public interface. Applications never import from lower layers:

```rust
// Key methods on Node:
node.peers()                          // List discovered peers
node.send(peer_id, namespace, data)   // Send namespaced message
node.broadcast(namespace, data)       // Fan-out to all peers
node.subscribe(namespace)             // Receive messages on a namespace
node.open_tcp(peer_id, port)          // Raw TCP stream
node.listen_tcp(port)                 // Accept TCP connections
node.ping(peer_id)                    // Tailscale-level ping
node.health()                         // Diagnostics
node.local_info()                     // Node identity
node.stop()                           // Graceful shutdown
```

### Layer 7: Applications (CLI)

The `truffle` CLI is a daemon-based architecture:
1. `truffle up` starts a persistent daemon that owns the `Node`
2. Subsequent commands (`ls`, `send`, `cp`, `tcp`) connect via Unix socket
3. Commands are thin wrappers that call Node API methods and format output

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
2. Devices discover each other through WatchIPNBus (passive, no announce messages)
3. Every node establishes WebSocket connections to peers on first message send
4. Messages are sent directly between peers -- no relay, no routing
5. If a node goes offline, the remaining nodes continue operating without disruption

This is the simplest possible topology because Tailscale already provides full-mesh encrypted connectivity via WireGuard tunnels. Truffle leverages this directly rather than adding its own routing layer on top.

## Port Map

| Port | Protocol | Purpose |
|------|----------|---------|
| 443  | TLS/WS   | Incoming WebSocket connections (tsnet HTTPS listener) |
| 9417 | TCP       | Direct TCP mesh connections |
| dynamic | UDP   | UDP relay via tsnet ListenPacket |
