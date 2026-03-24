# RFC 012: Layered Architecture Redesign

**Status**: Proposed
**Created**: 2026-03-24
**Author**: James Yong + Claude
**Supersedes**: RFC 008 (6-layer vision — replaces with cleaner separation)
**Depends on**: All prior RFCs (007-011) as implementation experience

---

## 1. Motivation

The current truffle codebase evolved through 5 RFCs (007-011) and works end-to-end, but the architecture has accumulated coupling between layers:

- **File transfer** straddles two network paths (WS for signaling, HTTP for data) with its own parallel infrastructure (port 9418, TcpProxyHandler, DialFn, axum server)
- **Peer discovery** mixes Tailscale polling with WS device-announce broadcasts — two mechanisms for the same information
- **Application logic** (FileTransferManager, FileTransferAdapter, HttpRouter, PushManager) lives inside `truffle-core`, making the core library aware of specific applications
- **The bridge layer** (GoShim, BridgeManager, pending_dials) leaks into handler.rs and server.rs — application code must manage bridge internals
- **Connection identity** requires a WS `device-announce` exchange before `send_envelope()` works — discovery is gated behind transport

This RFC proposes a clean layered architecture where:
1. Layers below application are fully generic — zero application-specific concepts
2. Each layer has a trait boundary — implementations are swappable
3. The public API is a single `Node` struct with ~12 methods
4. Applications (file transfer, chat, proxy, video) are pure Layer 7

---

## 2. Architecture Overview

```
═══════════════════════════════════════════════════════════════
  Layer 7: APPLICATION
  User code. NOT part of truffle-core.
═══════════════════════════════════════════════════════════════
  Layer 6: ENVELOPE
  Namespace-based message framing.
═══════════════════════════════════════════════════════════════
  Layer 5: SESSION
  Peer identity, connection registry, lazy connect, auto-reconnect.
═══════════════════════════════════════════════════════════════
  Layer 4: TRANSPORT
  WebSocket, TCP, UDP, QUIC — protocol-agnostic traits.
═══════════════════════════════════════════════════════════════
  Layer 3: NETWORK
  Tailscale: encrypted tunnels, peer discovery, addressing.
═══════════════════════════════════════════════════════════════
  Layer 2: BRIDGE (implementation detail of Layer 3)
  Go sidecar ↔ Rust interop. Invisible above Layer 3.
═══════════════════════════════════════════════════════════════
  Layer 1: SIDECAR (implementation detail of Layer 3)
  Go process embedding tsnet.
═══════════════════════════════════════════════════════════════
```

---

## 3. Layer Specifications

### 3.1 Layer 7: Application

**Location**: `truffle-cli/src/apps/` and user code (NOT in truffle-core)

**Responsibility**: Application-specific logic built on the `Node` API.

**Rules**:
- Applications ONLY interact with the `Node` public API
- Applications NEVER touch GoShim, BridgeManager, pending_dials, or bridge headers
- Applications NEVER import from `truffle_core::bridge::*`
- Each application owns its namespace (e.g., `"ft"`, `"chat"`, `"bus"`)

**Current applications to extract from truffle-core**:
- `FileTransferManager` + `FileTransferAdapter` + `sender.rs` + `receiver.rs` → `truffle-cli/src/apps/file_transfer/`
- `HttpRouter` + `WsUpgradeHandler` + `HttpRouterBridgeHandler` → remove (replaced by `node.open_tcp()`)
- `PushManager` → `truffle-cli/src/apps/push/`

**Application examples**:

```rust
// File transfer (truffle cp)
node.send(peer, "ft", serialize(Offer { file, sha256, path })).await?;
let accept = node.subscribe("ft").next().await;
let stream = node.open_tcp(peer, 0).await?;
stream.write_all(&file_bytes).await?;

// Message bus (pub/sub)
node.broadcast("bus", serialize(Publish { topic: "sensors/temp", data: 22.5 }));

// Port forwarding (proxy)
let remote = node.open_tcp(peer, 5432).await?;
pipe(local_socket, remote).await;

// Video streaming (fondue)
let quic = node.open_quic(peer).await?;
let video = quic.open_stream().await?;
loop { video.write(&encode_frame(camera.read())).await?; }
```

---

### 3.2 Layer 6: Envelope

**Location**: `truffle-core/src/envelope/`

**Responsibility**: Namespace-routed message framing over Layer 4 streams.

**Components**:

```rust
/// A namespace-routed message. All application messages use this format
/// over WebSocket connections. truffle-core NEVER inspects the payload.
#[derive(Serialize, Deserialize)]
pub struct Envelope {
    /// Namespace owned by the application (e.g., "ft", "chat", "bus").
    /// Used for routing to the correct subscriber.
    pub namespace: String,

    /// Application-defined message type within the namespace.
    pub msg_type: String,

    /// Opaque payload. truffle-core never deserializes this.
    pub payload: serde_json::Value,

    /// Millisecond timestamp (set by sender).
    pub timestamp: Option<u64>,

    /// Sender peer ID (set by Layer 5 on receive).
    pub from: Option<String>,
}
```

**Rules**:
- Layer 6 defines the envelope format, serialization, and deserialization
- Layer 6 does NOT route messages — that's Layer 5's job
- Layer 6 does NOT know about WebSocket, TCP, or any transport
- The `namespace` field is an opaque string — Layer 6 never matches against known values
- No `"mesh"` namespace, no `"device-announce"`, no `"file-transfer"` constants in this layer

**Codec**:
- Default: JSON (human-readable, debuggable)
- Future: MessagePack or Protobuf (for high-throughput applications)
- The codec is pluggable — applications can choose

---

### 3.3 Layer 5: Session

**Location**: `truffle-core/src/session/`

**Responsibility**: Peer identity, connection lifecycle, message routing.

**Components**:

```rust
/// Manages peer state and connections.
pub struct PeerRegistry {
    /// All known peers (from Layer 3 NetworkProvider).
    /// Peers exist here even with zero active connections.
    peers: HashMap<PeerId, PeerState>,

    /// Active connections (WS, QUIC, etc.) indexed by peer.
    connections: HashMap<PeerId, Vec<Connection>>,
}

/// A peer's state in the registry.
pub struct PeerState {
    pub id: PeerId,
    pub name: String,
    pub ip: IpAddr,
    pub online: bool,              // from Layer 3 (Tailscale)
    pub connection_type: String,   // "direct" | "relay:ord" — from Layer 3
    pub connected: bool,           // true if any active transport session
    pub transports: Vec<TransportType>, // which transports are active
}

/// Events emitted by the session layer.
pub enum PeerEvent {
    Joined(PeerState),     // Tailscale reports a new truffle peer online
    Left(PeerId),          // Tailscale reports peer offline
    Updated(PeerState),    // Peer metadata changed (IP, connection type)
    Connected(PeerId),     // Transport session established
    Disconnected(PeerId),  // Transport session lost
}
```

**Key behaviors**:

1. **Peer source of truth**: Layer 3 (WatchIPNBus). NOT transport connections.
   - `peers()` returns all truffle nodes Tailscale knows about
   - A peer can be `online: true, connected: false` (we see them but haven't dialed)

2. **Lazy connection**: Transport connections are established on first use.
   ```
   node.send(peer, ns, data)
     → Layer 5 checks: do we have a WS to this peer?
     → No → Layer 4 WebSocketTransport.connect(peer.addr)
     → WS handshake (exchange peer_id, capabilities — ONE TIME, not every 30s)
     → Cache connection
     → Send envelope
   ```

3. **Connection cache**: Subsequent `send()` calls reuse the existing WS.

4. **Auto-reconnect**: If a WS drops, Layer 5 marks `connected: false`. Next `send()` re-establishes. No background reconnect loops.

5. **Multi-transport**: A peer can have a WS (for messaging) AND a TCP stream (for file transfer) AND a QUIC connection (for video) simultaneously. Layer 5 tracks all of them.

**Rules**:
- Layer 5 does NOT know what namespaces mean
- Layer 5 does NOT inspect envelope payloads
- Layer 5 does NOT do peer discovery — it consumes Layer 3 events
- Layer 5 does NOT implement any transport protocol — it delegates to Layer 4

---

### 3.4 Layer 4: Transport

**Location**: `truffle-core/src/transport/`

**Responsibility**: Protocol-specific connection management.

**Traits**:

```rust
/// A persistent, framed, bidirectional connection.
/// Used for: messaging, pub/sub, signaling.
/// Implementations: WebSocket, QUIC streams.
pub trait StreamTransport: Send + Sync {
    type Stream: FramedStream;

    /// Connect to a peer address.
    async fn connect(&self, addr: &PeerAddr) -> Result<Self::Stream>;

    /// Listen for incoming connections.
    async fn listen(&self, port: u16) -> Result<StreamListener<Self::Stream>>;
}

/// A framed bidirectional stream (message-oriented, not byte-oriented).
pub trait FramedStream: Send + Sync + 'static {
    async fn send(&mut self, data: &[u8]) -> Result<()>;
    async fn recv(&mut self) -> Result<Option<Vec<u8>>>;
    async fn close(&mut self) -> Result<()>;
    fn peer_addr(&self) -> &PeerAddr;
}

/// A raw byte stream (not framed).
/// Used for: file transfer, TCP proxy, HTTP.
/// Implementations: TCP, QUIC unidirectional streams.
pub trait RawTransport: Send + Sync {
    /// Open a one-off byte stream to a peer.
    async fn open(&self, addr: &PeerAddr, port: u16) -> Result<TcpStream>;

    /// Listen for incoming byte streams.
    async fn listen(&self, port: u16) -> Result<RawListener>;
}

/// Unreliable datagrams.
/// Used for: real-time video/audio, game state.
/// Implementations: UDP, QUIC datagrams.
pub trait DatagramTransport: Send + Sync {
    async fn bind(&self, addr: &PeerAddr) -> Result<DatagramSocket>;
}
```

**Implementations**:

| Struct | Trait | Protocol | Use case |
|--------|-------|----------|----------|
| `WebSocketTransport` | `StreamTransport` | WS over TCP | Messaging, signaling |
| `TcpTransport` | `RawTransport` | Raw TCP | File transfer, proxy |
| `QuicTransport` | `StreamTransport` + `RawTransport` | QUIC over UDP | Video, multiplexed streams |
| `UdpTransport` | `DatagramTransport` | Raw UDP | Ultra-low-latency |

**Rules**:
- Layer 4 does NOT know about peers — it works with addresses
- Layer 4 does NOT know about envelopes or namespaces
- Layer 4 does NOT do discovery
- Each transport implementation is independent — adding QUIC doesn't change WS
- All transports use Layer 3's `dial()` / `listen()` to establish the underlying network connection

**WebSocket specifics**:
- Handshake exchanges `{ peer_id, capabilities, protocol_version }` — this is transport-level, not application-level
- Heartbeat (configurable interval) for keep-alive
- Binary frames for envelopes, ping/pong for liveness

---

### 3.5 Layer 3: Network

**Location**: `truffle-core/src/network/`

**Responsibility**: Peer discovery, addressing, encrypted tunnels.

**Trait**:

```rust
/// Provides network-level peer discovery and raw connectivity.
/// The primary implementation is TailscaleProvider (tsnet).
pub trait NetworkProvider: Send + Sync {
    /// Start the network provider.
    async fn start(&mut self) -> Result<()>;

    /// Stop the network provider.
    async fn stop(&mut self) -> Result<()>;

    /// Local node's identity.
    fn local_identity(&self) -> &NodeIdentity;

    /// Local node's network address.
    fn local_addr(&self) -> &PeerAddr;

    // ── Discovery (event-driven, NOT polling) ──

    /// Stream of peer events. Fires immediately when peers join/leave.
    fn peer_events(&self) -> broadcast::Receiver<NetworkPeerEvent>;

    /// Snapshot of all known peers.
    async fn peers(&self) -> Vec<NetworkPeer>;

    // ── Connectivity primitives for Layer 4 ──

    /// Dial a TCP connection to a peer.
    async fn dial_tcp(&self, addr: &str, port: u16) -> Result<TcpStream>;

    /// Listen for TCP connections on a port.
    async fn listen_tcp(&self, port: u16) -> Result<TcpListener>;

    /// Send/receive UDP datagrams.
    async fn bind_udp(&self, port: u16) -> Result<UdpSocket>;

    // ── Diagnostics ──

    /// Ping a peer via the network layer (not application layer).
    async fn ping(&self, addr: &str) -> Result<PingResult>;

    /// Node health info (key expiry, connection quality, etc.).
    async fn health(&self) -> HealthInfo;
}

/// A peer as seen by the network layer.
pub struct NetworkPeer {
    pub id: String,           // stable node ID from Tailscale
    pub hostname: String,     // "truffle-cli-{uuid}"
    pub ip: IpAddr,           // 100.x.x.x
    pub online: bool,
    pub cur_addr: Option<String>,  // direct endpoint or empty
    pub relay: Option<String>,     // DERP relay name or empty
    pub os: Option<String>,
    pub last_seen: Option<DateTime>,
    pub key_expiry: Option<DateTime>,
}

pub enum NetworkPeerEvent {
    Joined(NetworkPeer),
    Left(String),        // peer ID
    Updated(NetworkPeer),
}

pub struct PeerAddr {
    pub ip: IpAddr,
    pub hostname: String,
}

pub struct NodeIdentity {
    pub id: String,
    pub hostname: String,
    pub name: String,    // human-readable display name
}

pub struct PingResult {
    pub latency: Duration,
    pub connection: String,  // "direct" | "relay:sfo"
}
```

**TailscaleProvider implementation**:

```rust
pub struct TailscaleProvider {
    sidecar: GoSidecar,    // Layer 1 — Go child process
    bridge: Bridge,        // Layer 2 — TCP bridge for raw streams
    peer_tx: broadcast::Sender<NetworkPeerEvent>,
}

impl TailscaleProvider {
    pub async fn start(&mut self) -> Result<()> {
        // 1. Spawn Go sidecar (Layer 1)
        // 2. Start bridge TCP listener (Layer 2)
        // 3. Send tsnet:start command
        // 4. Wait for auth + running state
        // 5. Start WatchIPNBus loop → emit NetworkPeerEvents
        //    (replaces BOTH getPeers polling AND device-announce)
    }

    pub async fn dial_tcp(&self, addr: &str, port: u16) -> Result<TcpStream> {
        // 1. Send bridge:dial command to Go sidecar
        // 2. Go calls tsnet.Dial(addr, port)
        // 3. Go bridges connection back to Rust via bridge TCP
        // 4. Return the TcpStream
        //
        // All bridge internals (pending_dials, binary headers, session tokens)
        // are HIDDEN inside this method. Callers get a plain TcpStream.
    }
}
```

**Rules**:
- Layer 3 does NOT know about WebSocket, QUIC, or any Layer 4 protocol
- Layer 3 does NOT know about envelopes, namespaces, or messages
- Layer 3 provides raw `TcpStream` / `UdpSocket` — not framed connections
- `WatchIPNBus` is the ONLY source of peer events — no polling, no announce
- The `NetworkProvider` trait is swappable — future providers could use mDNS (LAN), STUN/TURN (internet), or Bluetooth (local)

---

### 3.6 Layers 1-2: Bridge + Sidecar (internal to TailscaleProvider)

**Location**: `truffle-core/src/network/tailscale/bridge/` and `truffle-core/src/network/tailscale/sidecar/`

**These are implementation details of `TailscaleProvider`.** They are NOT exported. They have NO public API. Nothing above Layer 3 knows they exist.

If a pure-Rust Tailscale client becomes available, Layers 1-2 are deleted entirely and `TailscaleProvider` is rewritten to use it directly. Zero impact on anything above.

---

## 4. The Node API

**Location**: `truffle-core/src/node.rs`

The `Node` struct is the **single public entry point** for all truffle functionality. It wires Layers 3-6 together and exposes a clean API to Layer 7.

```rust
pub struct Node {
    network: Arc<dyn NetworkProvider>,
    session: Arc<PeerRegistry>,
    // Internal: transport instances, envelope codec, etc.
}

impl Node {
    // ── Builder ──

    pub fn builder() -> NodeBuilder;

    // ── Lifecycle ──

    pub async fn stop(&self);

    // ── Identity ──

    pub fn local_info(&self) -> &NodeIdentity;

    // ── Discovery (from Layer 3, no transport needed) ──

    pub async fn peers(&self) -> Vec<Peer>;
    pub fn on_peer_change(&self) -> broadcast::Receiver<PeerEvent>;

    // ── Diagnostics (from Layer 3, no transport needed) ──

    pub async fn ping(&self, peer_id: &str) -> Result<PingResult>;
    pub async fn health(&self) -> HealthInfo;

    // ── Messaging (Layer 6 envelope over Layer 4 WS) ──

    /// Send a namespaced message to a specific peer.
    /// Lazy-connects WS if not already connected.
    pub async fn send(&self, peer_id: &str, namespace: &str, data: &[u8]) -> Result<()>;

    /// Broadcast a namespaced message to all connected peers.
    pub async fn broadcast(&self, namespace: &str, data: &[u8]);

    /// Subscribe to messages in a namespace.
    pub fn subscribe(&self, namespace: &str) -> broadcast::Receiver<IncomingMessage>;

    // ── Raw streams (Layer 4 direct) ──

    /// Open a raw TCP stream to a peer.
    pub async fn open_tcp(&self, peer_id: &str, port: u16) -> Result<TcpStream>;

    /// Listen for incoming TCP connections on a port.
    pub async fn listen_tcp(&self, port: u16) -> Result<TcpListener>;

    /// Open a QUIC connection to a peer (multiplexed streams).
    pub async fn open_quic(&self, peer_id: &str) -> Result<QuicConnection>;

    /// Open a UDP datagram socket to a peer.
    pub async fn open_udp(&self, peer_id: &str) -> Result<UdpSocket>;
}

pub struct Peer {
    pub id: String,
    pub name: String,
    pub ip: IpAddr,
    pub online: bool,           // from Tailscale — always known
    pub connected: bool,        // any active transport session
    pub connection_type: String, // "direct" | "relay:ord"
    pub os: Option<String>,
    pub last_seen: Option<String>,
}

pub struct IncomingMessage {
    pub from: String,       // sender peer_id
    pub namespace: String,
    pub msg_type: String,
    pub payload: Vec<u8>,
    pub timestamp: Option<u64>,
}
```

---

## 5. Network Paths

Exactly **3 network paths** exist in the new architecture:

| # | Path | When used | Transport | Purpose |
|---|------|-----------|-----------|---------|
| 1 | **IPC** | Always | Unix socket (local) | CLI ↔ daemon JSON-RPC |
| 2 | **WS** | On first `send()` | WebSocket over Tailscale TCP | Framed messages between peers |
| 3 | **Raw stream** | On `open_tcp()` / `open_quic()` / `open_udp()` | TCP/QUIC/UDP over Tailscale | Bulk data, proxy, video |

Port 9418 and TcpProxyHandler are eliminated. The bridge is invisible (internal to Layer 3). There is no separate "file transfer network path" — file transfer uses path 3 (`open_tcp`).

---

## 6. Crate Structure

```
truffle-core/src/
├── node.rs                    ← Public API: Node struct
│
├── network/                   ← Layer 3
│   ├── mod.rs                     trait NetworkProvider
│   ├── types.rs                   NetworkPeer, PeerAddr, NodeIdentity, PingResult
│   └── tailscale/                 TailscaleProvider implementation
│       ├── provider.rs                TailscaleProvider (public)
│       ├── sidecar.rs                 GoSidecar (private — Layer 1)
│       ├── bridge.rs                  Bridge (private — Layer 2)
│       ├── header.rs                  Binary header format (private)
│       └── protocol.rs               Command/event JSON types (private)
│
├── transport/                 ← Layer 4
│   ├── mod.rs                     StreamTransport, RawTransport, DatagramTransport traits
│   ├── websocket.rs               WebSocketTransport
│   ├── tcp.rs                     TcpTransport
│   ├── quic.rs                    QuicTransport (future)
│   └── udp.rs                     UdpTransport (future)
│
├── session/                   ← Layer 5
│   ├── mod.rs                     PeerRegistry
│   ├── connection.rs              Connection tracking
│   └── reconnect.rs               Auto-reconnect logic
│
└── envelope/                  ← Layer 6
    ├── mod.rs                     Envelope struct
    └── codec.rs                   JSON / MessagePack codecs


truffle-cli/src/
├── daemon/                    ← IPC server (unchanged)
│   ├── server.rs
│   ├── handler.rs                 JSON-RPC dispatch (uses Node API only)
│   ├── client.rs
│   └── protocol.rs
│
├── apps/                      ← Layer 7 applications
│   ├── file_transfer/
│   │   ├── mod.rs                 cp command orchestration
│   │   ├── sender.rs              Read file → write to TcpStream
│   │   ├── receiver.rs            Read from TcpStream → write to disk
│   │   └── protocol.rs           OFFER, ACCEPT, PULL_REQUEST types
│   │
│   ├── messaging/
│   │   ├── send.rs                node.send() wrapper
│   │   └── chat.rs                Interactive chat TUI
│   │
│   ├── proxy/
│   │   ├── forward.rs             Local port → node.open_tcp() → remote
│   │   └── expose.rs              node.listen_tcp() → accept → pipe
│   │
│   └── diagnostics/
│       ├── ping.rs                node.ping() wrapper
│       ├── doctor.rs              node.health() + system checks
│       └── status.rs              node.local_info() + node.peers()
│
└── resolve.rs                 ← Name resolution (uses node.peers())
```

---

## 7. What Gets Removed from truffle-core

| Current module | Disposition | Reason |
|---------------|-------------|--------|
| `services/file_transfer/` (6 files, ~3,400 LOC) | Move to `truffle-cli/src/apps/file_transfer/` | Application-specific |
| `services/http/` (HttpRouter, WsUpgradeHandler) | Delete | Replaced by `node.open_tcp()` |
| `services/push/` (PushManager) | Move to `truffle-cli/src/apps/push/` | Application-specific |
| `mesh/device.rs` (DeviceManager) | Replace with `session/registry.rs` | Peer state from Layer 3, not announces |
| `mesh/handler.rs` (TransportHandler with device-announce) | Simplify — no app-specific message handling | Namespace routing only |
| `mesh/node.rs` (MeshNode with send_envelope, broadcast) | Replace with `Node` | Cleaner API |
| `integration.rs` (wire_file_transfer) | Delete | File transfer is Layer 7 |
| `protocol/hostname.rs` (generate/parse hostname) | Move to `network/tailscale/` | Tailscale-specific |
| `bridge/manager.rs` (BridgeManager, TcpProxyHandler) | Move to `network/tailscale/bridge.rs` | Internal to Layer 3 |
| `bridge/shim.rs` (GoShim) | Move to `network/tailscale/sidecar.rs` | Internal to Layer 3 |

---

## 8. Migration Path

This is a significant refactor. Suggested phased approach:

### Phase 1: Extract NetworkProvider trait
- Create `network/mod.rs` with `NetworkProvider` trait
- Implement `TailscaleProvider` wrapping existing GoShim + BridgeManager
- Move bridge/* and shim.rs into `network/tailscale/`
- `Node` wraps `TruffleRuntime` initially (facade pattern)
- **Test**: all existing commands still work

### Phase 2: Replace polling with WatchIPNBus
- Add `WatchIPNBus` to Go sidecar
- Remove `get_peers()` polling loop
- Remove `device-announce` broadcast
- PeerRegistry populated purely from Layer 3 events
- **Test**: `truffle ls` shows peers without any WS connections

### Phase 3: Extract transport traits
- Create `StreamTransport` and `RawTransport` traits
- Wrap existing ConnectionManager as `WebSocketTransport`
- `TcpTransport` wraps `NetworkProvider.dial_tcp()`
- **Test**: `node.send()` and `node.open_tcp()` work

### Phase 4: Move file transfer to Layer 7
- Move `services/file_transfer/` to `truffle-cli/src/apps/`
- Replace HTTP PUT with raw TCP streaming
- Remove axum file server, TcpProxyHandler, port 9418
- `truffle cp` uses `node.send()` (signaling) + `node.open_tcp()` (data)
- **Test**: file transfer works without any bridge-level infrastructure

### Phase 5: Move remaining apps to Layer 7
- Move HttpRouter, PushManager out of truffle-core
- Clean up `mesh/` module — collapse into `session/`
- **Test**: truffle-core has zero application-specific code

### Phase 6: Add QUIC + UDP transports
- Implement `QuicTransport` (for video streaming)
- Implement `UdpTransport` (for low-latency)
- **Test**: `node.open_quic()` and `node.open_udp()` work

---

## 9. Design Decisions

### 9.1 Why not libp2p?

libp2p is the industry-standard P2P framework, but:
- It handles NAT traversal, encryption, and peer discovery — Tailscale already does ALL of this
- It adds ~10 crates of dependency for features we don't need
- Our "network layer" is simpler: Tailscale gives us authenticated, encrypted, routable connections to any peer. We just need to build app-layer services on top.

### 9.2 Why a Node struct instead of separate services?

A single `Node` struct provides:
- One place to configure and start everything
- Consistent peer resolution across all operations
- Shared connection pool (one WS per peer, reused by all apps)
- Clean shutdown (one `stop()` call)

### 9.3 Why lazy connections?

Eager connection (current: dial all peers on discovery) wastes resources when you have 50 tailnet nodes but only chat with 2. Lazy connection means:
- `node.peers()` is instant (no dials)
- `node.send(peer, ...)` dials on first message
- Subsequent messages reuse the connection
- Idle connections can be closed after timeout

### 9.4 Why raw TCP for file transfer instead of HTTP?

HTTP adds:
- Request/response framing overhead
- Header parsing
- Content-Length / Transfer-Encoding negotiation
- Keep-alive complexity (the TcpProxy deadlock bug)
- An entire axum server + hyper client dependency

Raw TCP:
- Zero overhead — just bytes on the wire
- Resume: sender asks "how many bytes?" via WS, seeks, streams
- Integrity: SHA-256 in the OFFER message, verified on completion
- Maximum throughput — no HTTP framing between file chunks
- No axum, no hyper for the data path

### 9.5 Why keep WebSocket (not just raw TCP for everything)?

WebSocket provides:
- **Message framing** — TCP is a byte stream, WS gives message boundaries
- **Persistent connection** — no dial overhead per message
- **Bidirectional** — both peers can send anytime
- **Multiplexing via namespaces** — multiple apps share one connection

Raw TCP is better for bulk data (files, proxy), but messaging needs framing.

---

## 10. Success Criteria

The refactor is complete when:

1. `truffle-core` has zero imports from `services/file_transfer/`, `services/http/`, or `services/push/`
2. The `Node` public API has ≤15 methods
3. Adding a new application (e.g., clipboard sync) requires ZERO changes to truffle-core
4. Adding a new transport (e.g., QUIC) requires ZERO changes to applications
5. Adding a new network provider (e.g., mDNS) requires ZERO changes to transports or applications
6. `truffle ls` works with zero active WS connections (pure Layer 3 discovery)
7. `truffle cp` works without axum, TcpProxyHandler, or port 9418
8. All existing tests pass
9. The `pending_dials` HashMap is invisible outside `network/tailscale/`
10. The `GoShim` struct is invisible outside `network/tailscale/`
