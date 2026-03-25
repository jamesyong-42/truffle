# RFC 012: Layered Architecture Redesign

**Status**: Implemented
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

## 8. Implementation Plan: Clean Rebuild (Bottom-Up)

**Strategy**: Build the new architecture from scratch alongside the old code. No migration, no wrapping, no facade pattern. The old code stays untouched until the new stack is fully built and tested, then we swap entirely.

**Why not migrate**: The old architecture has fundamental coupling issues (application code in core, transport-dependent discovery, bridge internals leaking everywhere). Trying to migrate incrementally means every phase must maintain backward compatibility with broken abstractions. A clean rebuild is faster and produces better code.

**Workspace setup**: The v2 crates have been promoted to primary names. Old crates deleted.

```
truffle/
├── crates/
│   ├── truffle-core/        ← v2 architecture (clean layered design)
│   ├── truffle-cli/         ← v2 CLI (Node API)
│   ├── truffle-napi/        ← needs update to new Node API (excluded from workspace)
│   └── truffle-tauri-plugin/← needs update to new Node API (excluded from workspace)
└── packages/
    └── sidecar-slim/        ← Go sidecar (updated for WatchIPNBus)
```

---

### Phase 1: Layer 3 — Network

**Build**: `truffle-core-v2/src/network/`

**Goal**: A working `TailscaleProvider` that starts, discovers peers via WatchIPNBus, and provides `dial_tcp()` / `listen_tcp()`.

**Deliverables**:
- `NetworkProvider` trait (complete API)
- `TailscaleProvider` implementation
  - Spawns Go sidecar
  - Bridge TCP listener + binary header protocol
  - stdin/stdout command/event channel
  - `WatchIPNBus` event loop (replaces `getPeers` polling)
  - `dial_tcp()` / `listen_tcp()` / `bind_udp()` with clean `TcpStream` / `UdpSocket` return
  - `ping()` via Tailscale TSMP
  - All bridge internals (pending_dials, session token, header format) are **private**
- Go sidecar update: add `WatchIPNBus` handler, emit `tsnet:peerChanged` events

**Tests**:
- Unit: sidecar spawn/stop, command serialization, header parse
- Integration: start provider, auth, discover peers, dial_tcp between two nodes
- **Key test**: `provider.peers()` returns peers WITHOUT any Layer 4 transport running

**Acceptance**: Two nodes on the same tailnet can `dial_tcp` to each other and exchange bytes. Zero WebSocket code exists. Zero application code exists. Just raw networking.

**Files**:
```
truffle-core-v2/src/
├── network/
│   ├── mod.rs              trait NetworkProvider + types
│   ├── types.rs            NetworkPeer, PeerAddr, NodeIdentity, PingResult, PeerEvent
│   └── tailscale/
│       ├── mod.rs           pub struct TailscaleProvider
│       ├── provider.rs      NetworkProvider impl
│       ├── sidecar.rs       GoSidecar (spawn, command, events) — PRIVATE
│       ├── bridge.rs        Bridge (TCP listener, header routing) — PRIVATE
│       ├── header.rs        Binary header format — PRIVATE
│       └── protocol.rs      JSON command/event types — PRIVATE
```

---

### Phase 2: Layer 4 — Transport

**Build**: `truffle-core-v2/src/transport/`

**Goal**: `WebSocketTransport` and `TcpTransport` that use Layer 3 to connect to peers.

**Deliverables**:
- `StreamTransport` trait + `FramedStream` trait
- `RawTransport` trait
- `DatagramTransport` trait (interface only, no impl yet)
- `WebSocketTransport` implementation
  - `connect(addr)` → dial via Layer 3, WS upgrade, return `FramedStream`
  - `listen(port)` → accept via Layer 3, WS upgrade on accept
  - Handshake: exchange `{ peer_id, capabilities, protocol_version }`
  - Heartbeat (configurable: 10s ping, 30s timeout)
  - Binary frame send/receive
- `TcpTransport` implementation
  - `open(addr, port)` → `Layer3.dial_tcp()`, return `TcpStream`
  - `listen(port)` → `Layer3.listen_tcp()`, return listener

**Tests**:
- Unit: WS handshake, heartbeat, frame encoding
- Integration: two nodes, WS connect, send/receive frames
- Integration: two nodes, TCP stream, bidirectional byte transfer
- **Key test**: WS and TCP work independently — WS failure doesn't break TCP

**Acceptance**: Two nodes can establish WS and TCP connections and exchange data. Zero message routing, zero peer registry, zero application code.

**Files**:
```
truffle-core-v2/src/
├── transport/
│   ├── mod.rs              StreamTransport, RawTransport, DatagramTransport traits
│   ├── websocket.rs        WebSocketTransport : StreamTransport
│   ├── tcp.rs              TcpTransport : RawTransport
│   ├── quic.rs             (stub — trait impl placeholder for future)
│   └── udp.rs              (stub — trait impl placeholder for future)
```

---

### Phase 3: Layer 5 — Session

**Build**: `truffle-core-v2/src/session/`

**Goal**: `PeerRegistry` that tracks peers from Layer 3, manages connections via Layer 4, and provides `send(peer_id, data)`.

**Deliverables**:
- `PeerRegistry`
  - Subscribes to Layer 3 `peer_events()` → maintains peer list
  - Peers exist with `online: true, connected: false` before any transport
  - Lazy connect: first `send()` triggers WS connect
  - Connection cache: reuse WS for subsequent sends
  - Multi-transport tracking: a peer can have WS + TCP + QUIC simultaneously
  - Auto-reconnect on WS drop (with backoff)
  - `send(peer_id, data)` → find/create WS → send frame
  - `broadcast(data)` → send to all connected peers
  - `subscribe()` → receive frames from any peer
- `PeerEvent` emissions: Joined, Left, Updated, Connected, Disconnected

**Tests**:
- Unit: peer add/remove, connection lookup, lazy connect logic
- Integration: two nodes, Layer 3 discovers peer, first `send()` auto-connects WS, message delivered
- Integration: kill WS, verify auto-reconnect, second `send()` succeeds
- **Key test**: `peers()` returns peers with zero connections (pure Layer 3)
- **Key test**: `send()` to an offline peer returns error (not hang)

**Acceptance**: `registry.send(peer_id, bytes)` delivers data. `registry.peers()` lists all tailnet peers. Connection lifecycle is fully managed. Zero namespace routing, zero envelope format.

**Files**:
```
truffle-core-v2/src/
├── session/
│   ├── mod.rs              PeerRegistry, PeerState, PeerEvent
│   ├── connection.rs       Connection tracking (multi-transport)
│   └── reconnect.rs        Auto-reconnect with backoff
```

---

### Phase 4: Layer 6 — Envelope

**Build**: `truffle-core-v2/src/envelope/`

**Goal**: Namespace-routed message framing that Layer 5 uses for WS messages.

**Deliverables**:
- `Envelope` struct: `{ namespace, msg_type, payload, timestamp, from }`
- `EnvelopeCodec`: serialize/deserialize (JSON default, MessagePack future)
- Integration with Layer 5: `PeerRegistry.send()` wraps data in envelope before sending on WS
- Namespace-based `subscribe(namespace)` filtering

**Tests**:
- Unit: serialize/deserialize roundtrip, namespace filtering
- Integration: two nodes, send envelope with namespace "test", subscriber receives it, other namespace subscriber does NOT receive it

**Acceptance**: Applications can send namespaced messages and subscribe to specific namespaces. truffle-core never inspects payload contents.

**Files**:
```
truffle-core-v2/src/
├── envelope/
│   ├── mod.rs              Envelope struct
│   └── codec.rs            JSON codec (+ future MessagePack)
```

---

### Phase 5: Node API — Wire It All Together

**Build**: `truffle-core-v2/src/node.rs`

**Goal**: The `Node` struct that exposes the complete public API.

**Deliverables**:
- `Node::builder()` → `NodeBuilder` (configure name, sidecar path, etc.)
- `node.peers()`, `node.on_peer_change()` → Layer 5 → Layer 3
- `node.send()`, `node.broadcast()`, `node.subscribe()` → Layer 6 → Layer 5 → Layer 4
- `node.open_tcp()`, `node.listen_tcp()` → Layer 4 → Layer 3
- `node.open_quic()`, `node.open_udp()` → stubs returning `Err("not yet implemented")`
- `node.ping()`, `node.health()` → Layer 3
- `node.stop()` → cascade shutdown through all layers

**Tests**:
- Integration: full Node lifecycle — build, start, discover peers, send message, receive, stop
- Integration: `node.open_tcp()` → raw TCP stream works
- Integration: `node.send()` → lazy WS connect → message delivered
- Integration: `node.peers()` → returns peers with no connections
- **Key test**: the "multiplayer game" litmus test:
  ```rust
  let node = Node::builder().name("test").build().await?;
  node.send(&peer, "game", b"state-update").await?;
  let stream = node.open_tcp(&peer, 0).await?;
  // Both work independently
  ```

**Acceptance**: The full Node API works end-to-end between two nodes. This is `truffle-core-v2` complete.

**Files**:
```
truffle-core-v2/src/
├── lib.rs                  pub mod network, transport, session, envelope, node
└── node.rs                 pub struct Node, NodeBuilder
```

---

### Phase 6: Layer 7 — Rebuild CLI Apps

**Build**: `truffle-cli/src/apps/` (modify existing truffle-cli to use v2)

**Goal**: Rewrite all CLI commands against the `Node` API.

**Deliverables**:
- `apps/file_transfer/` — cp command using `node.send()` + `node.open_tcp()`
  - Raw TCP streaming (no HTTP, no axum)
  - OFFER/ACCEPT via WS, data via TCP
  - SHA-256 verification, resume support
- `apps/messaging/` — send + chat using `node.send()` + `node.subscribe()`
- `apps/proxy/` — proxy + expose using `node.open_tcp()` + `node.listen_tcp()`
- `apps/diagnostics/` — status, ls, ping, doctor using `node.peers()` + `node.ping()` + `node.health()`
- `daemon/handler.rs` — rewrite dispatch to use `Node` API only
- `daemon/server.rs` — simplified: create `Node`, run IPC loop, done

**Tests**:
- End-to-end: `truffle up` → `truffle ls` → `truffle ping` → `truffle send` → `truffle cp` → `truffle down`
- Cross-machine: macOS ↔ EC2 Linux peer test (same as current test protocol)
- Stress: multiple concurrent file transfers
- Edge: offline peer, killed sidecar, network partition

**Acceptance**: All current working commands (`up`, `down`, `status`, `ls`, `ping`, `send`, `cp`, `tcp`, `doctor`) work identically to v0.2.5 but using the v2 core.

---

### Phase 7: Swap + Cleanup -- COMPLETE

**Goal**: Replace `truffle-core` with `truffle-core-v2`, delete old code.

**Steps**:
1. Rename `truffle-core/` -> `truffle-core-old/` (keep as reference) -- DONE
2. Rename `truffle-core-v2/` -> `truffle-core/` -- DONE
3. Update all `Cargo.toml` dependencies -- DONE
4. Run full test suite -- DONE
5. Cross-machine peer test -- deferred (requires two nodes)
6. Delete `truffle-core-old/` -- DONE
7. Update truffle-napi and truffle-tauri-plugin to use new Node API -- deferred (removed from workspace for now)
8. Tag v0.3.0 -- pending

**Acceptance**: `cargo test --workspace` passes. All CLI commands work. `truffle-core` has zero application-specific code. The `pending_dials` HashMap, `GoShim`, and `BridgeManager` are invisible outside `network/tailscale/`.

---

### Phase 8: Future Transports

**Goal**: Add QUIC and UDP transport implementations.

**Steps**:
1. Implement `QuicTransport` using `quinn` crate
2. Implement `UdpTransport` using `tokio::net::UdpSocket`
3. `node.open_quic()` and `node.open_udp()` become functional
4. Video streaming app (`fondue`) becomes possible

**No rush**: This phase happens when an application (fondue) needs it.

---

### Timeline Estimate

| Phase | Layer | Effort | Can parallelize? |
|-------|-------|--------|-----------------|
| 1 | Layer 3: Network | 2-3 sessions | No (foundation) |
| 2 | Layer 4: Transport | 1-2 sessions | No (needs Layer 3) |
| 3 | Layer 5: Session | 1-2 sessions | No (needs Layer 4) |
| 4 | Layer 6: Envelope | 0.5 session | No (needs Layer 5) |
| 5 | Node API | 1 session | No (wires all layers) |
| 6 | Layer 7: CLI Apps | 2-3 sessions | Yes (per app) |
| 7 | Swap + Cleanup | 0.5 session | No |
| 8 | QUIC + UDP | Future | Independent |

**Total: ~8-12 focused sessions to complete the rebuild.**

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

---

## 11. CLI Command Flows Under New Architecture

Every command uses ONLY the `Node` API. No command touches GoShim, BridgeManager, pending_dials, or bridge headers.

### 11.1 `truffle up`

```
User                     CLI                    Node                Layer 5          Layer 4          Layer 3
 │                        │                      │                    │                │                │
 │── truffle up ─────────→│                      │                    │                │                │
 │                        │                      │                    │                │                │
 │                        │── Node::builder()    │                    │                │                │
 │                        │   .name(hostname)    │                    │                │                │
 │                        │   .sidecar(path)     │                    │                │                │
 │                        │   .build().await ───→│                    │                │                │
 │                        │                      │                    │                │                │
 │                        │                      │── Layer 3: TailscaleProvider::start()               │
 │                        │                      │   (spawn sidecar, auth, get IP)    │                │
 │                        │                      │   [sidecar + bridge are internal,  │                │
 │                        │                      │    invisible to all other layers]   │                │
 │                        │                      │                    │                │                │
 │                        │                      │── Layer 4: WebSocketTransport::listen()              │
 │                        │                      │   (ready to accept WS from peers)  │                │
 │                        │                      │                    │                │                │
 │                        │                      │── Layer 5: PeerRegistry::start()   │                │
 │                        │                      │   Subscribe to Layer 3 peer events  │                │
 │                        │                      │                    │                │                │
 │                        │                      │── Ready            │                │                │
 │                        │                      │                    │                │                │
 │                        │── DaemonServer.run()  │                    │                │                │
 │                        │   (IPC accept loop)  │                    │                │                │
 │                        │                      │                    │                │                │
 │                        │── node.on_peer_change()                   │                │                │
 │                        │   → stream to dashboard                   │                │                │
 │                        │                      │                    │                │                │
 │   Background (inside Node, automatic):        │                    │                │                │
 │                        │                      │                    │                │                │
 │   Layer 3 emits PeerEvent::Joined(peer)       │                    │                │                │
 │     → Layer 5: PeerRegistry adds peer         │                    │                │                │
 │       (online=true, connected=false)          │                    │                │                │
 │     → NO dial, NO WS — just awareness         │                    │                │                │
 │     → Layer 5 emits PeerEvent::Joined         │                    │                │                │
 │                        │                      │                    │                │                │
 │   First node.send() to this peer:             │                    │                │                │
 │     → Layer 5: no WS exists, auto-connect     │                    │                │                │
 │     → Layer 4: WS handshake (peer_id exchange)│                    │                │                │
 │     → Layer 5: cache connection               │                    │                │                │
 │     → Layer 5 emits PeerEvent::Connected      │                    │                │                │
```

**What happens**: Sidecar starts, Tailscale authenticates, WatchIPNBus begins. Peers appear instantly via events. NO polling, NO dialing, NO announce. Connections happen lazily on first use.

---

### 11.2 `truffle down`

```
User               CLI              Node             Layer 5           Layer 4          Layer 3
 │                  │                 │                 │                 │                │
 │── truffle down ─→│                 │                 │                 │                │
 │                  │── JSON-RPC ────→│                 │                 │                │
 │                  │  "shutdown"     │                 │                 │                │
 │                  │                 │── node.stop()   │                 │                │
 │                  │                 │                 │                 │                │
 │                  │                 │── Layer 5: for each connected peer:               │
 │                  │                 │   node.broadcast("_mesh", GOODBYE)                │
 │                  │                 │   close all WS/TCP/QUIC sessions  │                │
 │                  │                 │                 │                 │                │
 │                  │                 │── Layer 4: drop all transports    │                │
 │                  │                 │                 │                 │                │
 │                  │                 │── Layer 3: TailscaleProvider.stop()               │
 │                  │                 │   [internal: sidecar stop, bridge close]          │
 │                  │                 │                 │                 │                │
 │                  │── remove PID    │                 │                 │                │
 │                  │── remove socket │                 │                 │                │
 │←── "stopped" ────│                 │                 │                 │                │
```

**What happens**: Broadcast goodbye via existing WS connections, close everything, stop sidecar. One `node.stop()` call — clean cascade through layers.

---

### 11.3 `truffle status`

```
User               CLI              Node
 │                  │                 │
 │── truffle status→│                 │
 │                  │                 │
 │                  │── node.local_info()
 │                  │     → { id, name, ip, dns }     ← from Layer 3
 │                  │                 │
 │                  │── node.peers().len()
 │                  │     → peer count                 ← from Layer 5 (fed by Layer 3)
 │                  │                 │
 │                  │── node.health()
 │                  │     → { uptime, key_expiry }     ← from Layer 3
 │                  │                 │
 │←── dashboard ────│                 │
```

**What happens**: Three reads from the Node. Zero network calls. All data already cached from WatchIPNBus events.

---

### 11.4 `truffle ls`

```
User             CLI              Node           Layer 5
 │                │                 │               │
 │── truffle ls ─→│                 │               │
 │                │                 │               │
 │                │── node.peers()  │               │
 │                │                 │── PeerRegistry.all()
 │                │                 │               │
 │                │                 │← Vec<Peer>    │
 │                │                 │   id           │ ← from Layer 3 (hostname parse)
 │                │                 │   name         │ ← from Layer 3 (Tailscale HostName)
 │                │                 │   ip           │ ← from Layer 3 (TailscaleIPs)
 │                │                 │   online       │ ← from Layer 3 (peer.Online)
 │                │                 │   connected    │ ← from Layer 5 (has active WS?)
 │                │                 │   connection   │ ← from Layer 3 (CurAddr/Relay)
 │                │                 │               │
 │                │── filter --all  │               │
 │                │── format table  │               │
 │                │                 │               │
 │←── table ──────│                 │               │
 │                │                 │               │
 │  NODE      STATUS   CONNECTION  CONNECTED       │
 │  laptop    ● online direct      yes             │
 │  server    ● online relay:ord   no              │  ← online but no WS (lazy)
 │  phone     ○ offline —          no              │
```

**What happens**: Pure read from PeerRegistry. Zero network calls. Zero WS connections needed. The `CONNECTION` column (direct/relay) comes from Tailscale, not from our WS.

---

### 11.5 `truffle ping`

```
User                CLI              Node             Layer 3
 │                   │                │                  │
 │── truffle ping ──→│                │                  │
 │   laptop          │                │                  │
 │                   │                │                  │
 │                   │── resolve("laptop")               │
 │                   │   node.peers() → find by name     │
 │                   │   → peer_id                       │
 │                   │                │                  │
 │   [for i in 1..count]:            │                  │
 │                   │                │                  │
 │                   │── node.ping(peer_id)              │
 │                   │                │── NetworkProvider.ping(addr)
 │                   │                │                  │── Tailscale TSMP ping
 │                   │                │                  │   (real network round-trip)
 │                   │                │                  │
 │                   │                │←── PingResult {  │
 │                   │                │      latency: 0.25ms,
 │                   │                │      connection: "direct"
 │                   │                │    }             │
 │                   │                │                  │
 │←── "reply: 0.25ms (direct)" ──────│                  │
 │                   │                │                  │
 │   [print stats: 4 sent, 4 received, 0% loss]         │
 │   [round-trip min/avg/max = 0.15/0.25/0.31 ms]       │
```

**What happens**: Real Tailscale ping (TSMP/Disco/ICMP). Measures actual network latency and reports connection type. NO WS connection needed. Pure Layer 3.

---

### 11.6 `truffle send`

```
User                 CLI              Node           Layer 5          Layer 6         Layer 4
 │                    │                │                │                │               │
 │── truffle send ───→│                │                │                │               │
 │   laptop "hello"   │                │                │                │               │
 │                    │                │                │                │               │
 │                    │── resolve("laptop") → peer_id   │                │               │
 │                    │                │                │                │               │
 │                    │── node.send(peer_id, "chat", b"hello")          │               │
 │                    │                │                │                │               │
 │                    │                │── Layer 6: Envelope {           │               │
 │                    │                │     ns: "chat",                 │               │
 │                    │                │     type: "text",               │               │
 │                    │                │     payload: b"hello"           │               │
 │                    │                │   }                             │               │
 │                    │                │                │                │               │
 │                    │                │── Layer 5: PeerRegistry         │               │
 │                    │                │   Has WS to laptop? ────────────────────────────│
 │                    │                │                │                │               │
 │                    │                │   [if NO]:     │                │               │
 │                    │                │   Layer 4: WebSocketTransport   │               │
 │                    │                │     .connect(laptop.addr)───────────────────────│
 │                    │                │   WS handshake: exchange peer_id│               │
 │                    │                │   Cache connection              │               │
 │                    │                │                │                │               │
 │                    │                │   [then]:      │                │               │
 │                    │                │   ws.send(serialize(envelope))──────────────────│
 │                    │                │                │                │    WS binary   │
 │                    │                │                │                │    → bridge    │
 │                    │                │                │                │    → Go tsnet  │
 │                    │                │                │                │    → WireGuard │
 │                    │                │                │                │    → peer      │
 │                    │                │                │                │               │
 │←── "Sent to laptop"               │                │                │               │
```

**What happens**: Envelope created (Layer 6), routed to peer (Layer 5), lazy-connect WS if needed (Layer 4), send. Second call to same peer skips the dial — reuses cached WS.

---

### 11.7 `truffle cp` — Upload (`file.txt → server:/tmp/`)

```
User              CLI             Daemon (Layer 7 App)          Node

 │── truffle cp ──→│                │                            │
 │  file.txt       │                │                            │
 │  server:/tmp/   │                │                            │
 │                 │── JSON-RPC ───→│                            │
 │                 │  "push_file"   │                            │
 │                 │                │                            │
 │                 │                │── 1. SHA-256 hash file     │
 │                 │                │                            │
 │                 │                │── 2. OFFER signaling       │
 │                 │                │   node.send(server, "ft",  │
 │                 │                │     Offer { file_name,     │
 │                 │                │       size, sha256,        │
 │                 │                │       save_path, token })  │
 │                 │                │                            │── [Layer 5 → WS → peer]
 │                 │                │                            │
 │                 │                │   ┌── server receives OFFER via node.subscribe("ft")
 │                 │                │   │   validates save_path
 │                 │                │   │   sends Accept { token } back
 │                 │                │   └──────────────────────────── [WS → back to us]
 │                 │                │                            │
 │                 │                │── 3. Receive ACCEPT        │
 │                 │                │   via node.subscribe("ft") │
 │                 │                │                            │
 │                 │                │── 4. Open raw TCP          │
 │                 │                │   stream = node.open_tcp(  │
 │                 │                │     server, 0)             │
 │                 │                │                            │── [Layer 4 → Layer 3]
 │                 │                │                            │   [TailscaleProvider.dial_tcp]
 │                 │                │                            │   [internal: sidecar dial + bridge]
 │                 │                │                            │── TcpStream returned
 │                 │                │                            │
 │                 │                │── 5. Stream file           │
 │                 │                │   stream.write([8B size])  │
 │                 │                │   stream.write([32B sha])  │
 │                 │                │   loop:                    │
 │                 │                │     chunk = file.read(64KB)│
 │←── progress ────│                │     stream.write(chunk)    │── [raw bytes → tsnet → peer]
 │                 │                │                            │
 │                 │                │   ┌── server reads stream:
 │                 │                │   │   read size, sha256
 │                 │                │   │   read chunks → write to /tmp/file.txt
 │                 │                │   │   verify SHA-256
 │                 │                │   │   stream.write([0x01]) ← ACK
 │                 │                │   └────────────────────────────
 │                 │                │                            │
 │                 │                │── 6. Read ACK              │
 │                 │                │   ack = stream.read(1)     │
 │                 │                │   0x01 = verified OK       │
 │                 │                │                            │
 │←── complete ────│←── result ─────│                            │
 │  "SHA-256 ✓"    │                │                            │
```

**What happens**: Signaling via existing WS (`node.send`). Data via one-off raw TCP (`node.open_tcp`). No HTTP, no axum, no TcpProxy, no port 9418. The daemon app reads the file and streams it — truffle-core has no knowledge of files.

---

### 11.8 `truffle cp` — Download (`server:/tmp/file.txt → ./`)

```
User              CLI             Daemon (Layer 7 App)          Node            Server's Daemon

 │── truffle cp ──→│                │                            │                   │
 │  server:/tmp/f  │                │                            │                   │
 │  ./             │                │                            │                   │
 │                 │── JSON-RPC ───→│                            │                   │
 │                 │  "get_file"    │                            │                   │
 │                 │                │                            │                   │
 │                 │                │── 1. PULL_REQUEST          │                   │
 │                 │                │   node.send(server, "ft",  │                   │
 │                 │                │     PullRequest {          │                   │
 │                 │                │       path, requester_id })│                   │
 │                 │                │                            │── [WS → server] ─→│
 │                 │                │                            │                   │
 │                 │                │   ┌── server receives PullRequest              │
 │                 │                │   │   validate path exists                     │
 │                 │                │   │   SHA-256 hash file                        │
 │                 │                │   │   send Offer back via WS                   │
 │                 │                │   └────────────────────── [WS → back to us] ──→│
 │                 │                │                            │                   │
 │                 │                │── 2. Receive OFFER         │                   │
 │                 │                │   validate, send ACCEPT    │                   │
 │                 │                │                            │── [WS → server]   │
 │                 │                │                            │                   │
 │                 │                │── 3. Listen for TCP        │                   │
 │                 │                │   listener = node          │                   │
 │                 │                │     .listen_tcp(0)         │                   │
 │                 │                │                            │                   │
 │                 │                │   ┌── server receives ACCEPT                   │
 │                 │                │   │   stream = node.open_tcp(us, port)         │
 │                 │                │   │   write [size][sha256][file_bytes]          │
 │                 │                │   └──────────────── [raw TCP → to us] ─────────│
 │                 │                │                            │                   │
 │                 │                │── 4. Accept + receive      │                   │
 │                 │                │   stream = listener.accept()                   │
 │                 │                │   read size, sha256        │                   │
 │                 │                │   read chunks → write to ./file.txt            │
 │←── progress ────│                │   verify SHA-256           │                   │
 │                 │                │   stream.write([0x01]) ACK │                   │
 │                 │                │                            │                   │
 │←── complete ────│←── result ─────│                            │                   │
```

**What happens**: PULL_REQUEST via WS, server prepares and sends OFFER back via WS, we accept, server opens raw TCP to us and streams the file. Reversed roles, same protocol.

---

### 11.9 `truffle tcp`

```
User              CLI               Node              Layer 4         Layer 3
 │                 │                  │                   │               │
 │── truffle tcp ─→│                  │                   │               │
 │   server:5432   │                  │                   │               │
 │                 │                  │                   │               │
 │  [check mode]:  │                  │                   │               │
 │                 │── node.open_tcp( │                   │               │
 │                 │     server, 5432)│                   │               │
 │                 │                  │── TcpTransport    │               │
 │                 │                  │     .open(addr,   │               │
 │                 │                  │       5432)       │               │
 │                 │                  │                   │── tsnet.Dial  │
 │                 │                  │                   │  (addr:5432)  │
 │                 │                  │← TcpStream (or err)               │
 │←── "succeeded" ─│                  │                   │               │
 │                 │                  │                   │               │
 │  [stream mode]: │                  │                   │               │
 │                 │── stream = node  │                   │               │
 │                 │   .open_tcp(     │                   │               │
 │                 │     server, 5432)│                   │               │
 │                 │                  │                   │               │
 │                 │── pipe(stdin/stdout ↔ stream)        │               │
 │                 │                  │                   │               │
 │── type data ───→│───→ stream ────→ peer               │               │
 │←── recv data ───│←─── stream ←─── peer               │               │
```

**What happens**: One call: `node.open_tcp(peer, port)`. Check mode verifies connectivity. Stream mode pipes stdin/stdout to the raw TCP stream. The daemon is a thin pipe — zero protocol knowledge.

---

### 11.10 `truffle ping`

(Shown in 11.5 above)

---

### 11.11 `truffle doctor`

```
User               CLI              Node             Layer 3
 │                  │                 │                 │
 │── truffle doctor→│                 │                 │
 │                  │                 │                 │
 │                  │── Check sidecar binary exists     │
 │                  │   (filesystem check, no Node)     │
 │                  │                 │                 │
 │                  │── node.health() │                 │
 │                  │                 │── NetworkProvider.health()
 │                  │                 │                 │── Tailscale state
 │                  │                 │                 │── Key expiry
 │                  │                 │                 │── Health warnings
 │                  │                 │                 │
 │                  │── node.peers().len() > 0          │
 │                  │   (mesh reachable?)               │
 │                  │                 │                 │
 │                  │── Check config file               │
 │                  │   (filesystem, no Node)           │
 │                  │                 │                 │
 │←── checklist ────│                 │                 │
 │  ✓ Sidecar found                  │                 │
 │  ✓ Tailscale connected            │                 │
 │  ✓ Mesh reachable (3 peers)       │                 │
 │  ✓ Key expiry: 75 days            │                 │
 │  ✓ Config valid                   │                 │
```

**What happens**: Mix of filesystem checks and `Node` API reads. Zero network calls — all data from cached Layer 3 state.

---

### 11.12 Future: `truffle proxy`

```
User               CLI              Daemon (Layer 7 App)      Node
 │                  │                 │                         │
 │── truffle proxy ─→│                │                         │
 │   5432            │                │                         │
 │   server:5432     │                │                         │
 │                  │── JSON-RPC ────→│                         │
 │                  │                 │                         │
 │                  │                 │── local = TcpListener   │
 │                  │                 │   ::bind("127.0.0.1:5432")
 │                  │                 │                         │
 │                  │                 │── loop:                 │
 │                  │                 │   local_conn = local    │
 │                  │                 │     .accept()           │
 │                  │                 │                         │
 │                  │                 │   remote = node         │
 │                  │                 │     .open_tcp(server,   │
 │                  │                 │       5432)             │
 │                  │                 │                         │── [Layer 3 dial]
 │                  │                 │                         │
 │                  │                 │   tokio::spawn(         │
 │                  │                 │     pipe(local ↔ remote)│
 │                  │                 │   )                     │
 │                  │                 │                         │
 │ psql -h 127.0.0.1 -p 5432        │                         │
 │   → local:5432 → remote:5432 ────│── [raw TCP through tsnet]
```

**What happens**: Pure Layer 7. Listen locally, `node.open_tcp()` for each accepted connection, pipe bidirectionally. The proxy app is ~30 lines of code.

---

### 11.13 Future: `truffle expose`

```
User               CLI              Daemon (Layer 7 App)      Node
 │                  │                 │                         │
 │── truffle expose─→│                │                         │
 │   3000            │                │                         │
 │                  │── JSON-RPC ────→│                         │
 │                  │                 │                         │
 │                  │                 │── listener = node       │
 │                  │                 │   .listen_tcp(3000)     │
 │                  │                 │                         │── [Layer 3: tsnet.Listen(:3000)]
 │                  │                 │                         │
 │                  │                 │── loop:                 │
 │                  │                 │   remote_conn =         │
 │                  │                 │     listener.accept()   │
 │                  │                 │                         │
 │                  │                 │   local = TcpStream     │
 │                  │                 │     ::connect(          │
 │                  │                 │       "127.0.0.1:3000") │
 │                  │                 │                         │
 │                  │                 │   tokio::spawn(         │
 │                  │                 │     pipe(remote ↔ local)│
 │                  │                 │   )                     │
 │                  │                 │                         │
 │ Peer runs: truffle tcp me:3000    │                         │
 │   → tsnet → listener:3000        │                         │
 │   → pipe → localhost:3000        │                         │
```

**What happens**: Reverse of proxy. Listen on tsnet, accept remote connections, pipe to local service. Also ~30 lines.

---

### 11.14 Future: `truffle chat`

```
User               CLI              Daemon (Layer 7 App)      Node
 │                  │                 │                         │
 │── truffle chat ──→│                │                         │
 │   laptop          │                │                         │
 │                  │── JSON-RPC ────→│                         │
 │                  │                 │                         │
 │                  │                 │── msgs = node           │
 │                  │                 │   .subscribe("chat")    │
 │                  │                 │                         │
 │                  │                 │── spawn receiver:       │
 │                  │                 │   loop:                 │
 │                  │                 │     msg = msgs.next()   │
 │                  │                 │     → write to IPC      │
 │                  │                 │       (JSON event line) │
 │                  │                 │                         │
 │ [interactive]:   │                 │                         │
 │── "hello" ──────→│── read IPC ───→│                         │
 │                  │                 │── node.send(laptop,     │
 │                  │                 │     "chat",             │
 │                  │                 │     {text:"hello"})     │
 │                  │                 │                         │── [WS → peer]
 │                  │                 │                         │
 │ [receive]:       │                 │                         │
 │                  │                 │← msg from peer via WS   │
 │                  │                 │── write to IPC          │
 │←── "[14:23] laptop: hey" ─────────│                         │
```

**What happens**: `node.subscribe("chat")` for incoming, `node.send()` for outgoing. The chat app is pure message routing — zero transport knowledge.

---

### 11.15 Future: `truffle stream` (video — fondue)

```
User               CLI              Daemon (Layer 7 App)      Node           Layer 4
 │                  │                 │                         │               │
 │── truffle stream→│                │                         │               │
 │   start          │                │                         │               │
 │                  │── JSON-RPC ────→│                         │               │
 │                  │                 │                         │               │
 │                  │                 │── node.send(peer,       │               │
 │                  │                 │   "video",              │               │
 │                  │                 │   StreamOffer{codec,    │               │
 │                  │                 │    resolution,fps})     │── [WS → peer] │
 │                  │                 │                         │               │
 │                  │                 │── wait StreamAccept     │               │
 │                  │                 │   via node.subscribe()  │               │
 │                  │                 │                         │               │
 │                  │                 │── quic = node           │               │
 │                  │                 │   .open_quic(peer)      │               │
 │                  │                 │                         │── QuicTransport
 │                  │                 │                         │   .connect()  │
 │                  │                 │                         │               │── tsnet UDP
 │                  │                 │                         │               │
 │                  │                 │── video = quic          │               │
 │                  │                 │   .open_stream()        │               │
 │                  │                 │── audio = quic          │               │
 │                  │                 │   .open_stream()        │               │
 │                  │                 │── ctrl = quic           │               │
 │                  │                 │   .open_stream()        │               │
 │                  │                 │                         │               │
 │                  │                 │── loop:                 │               │
 │                  │                 │   frame = camera.read() │               │
 │                  │                 │   encoded = h264(frame) │               │
 │                  │                 │   video.write(encoded)  │── [QUIC → tsnet → peer]
 │                  │                 │                         │               │
 │                  │                 │   audio_pkt = mic.read()│               │
 │                  │                 │   audio.write(opus(pkt))│── [QUIC → tsnet → peer]
 │                  │                 │                         │               │
 │                  │                 │   ctrl_msg = ctrl.read()│               │
 │                  │                 │   handle(pause/seek/    │               │
 │                  │                 │    quality_change)      │               │
```

**What happens**: Signaling via WS (`node.send`). Media via QUIC (`node.open_quic` → independent streams for video/audio/control). Each stream is loss-independent — dropped video frame doesn't stall audio. Pure Layer 7 application.

---

### Summary: Node API usage per command

| Command | `peers()` | `send()` | `subscribe()` | `open_tcp()` | `listen_tcp()` | `open_quic()` | `ping()` | `health()` |
|---------|-----------|----------|---------------|-------------|----------------|---------------|----------|-----------|
| `up` | | | | | | | | |
| `down` | | broadcast | | | | | | |
| `status` | count | | | | | | | ✓ |
| `ls` | ✓ | | | | | | | |
| `ping` | resolve | | | | | | ✓ | |
| `send` | resolve | ✓ | | | | | | |
| `cp` (up) | resolve | ✓ | ✓ | ✓ | | | | |
| `cp` (down) | resolve | ✓ | ✓ | | ✓ | | | |
| `tcp` | resolve | | | ✓ | | | | |
| `proxy` | resolve | | | ✓ | | | | |
| `expose` | | | | | ✓ | | | |
| `chat` | resolve | ✓ | ✓ | | | | | |
| `doctor` | count | | | | | | | ✓ |
| `stream` | resolve | ✓ | ✓ | | | ✓ | | |
