# Raw Transports

Alongside the namespaced messaging API (`send`/`broadcast`/`onMessage`), every mesh node exposes the tailnet's raw transports directly to TypeScript: TCP, UDP, and QUIC, plus an HTTP convenience layer on top of TCP. This is the RFC 021 surface — `mesh.net`, `mesh.http`, `mesh.dgram`, and `mesh.quic`.

Each namespace mimics the Node.js stdlib module it corresponds to (`node:net`, `node:dgram`, `node:http`), so existing networking code usually ports by swapping the import and pointing `host`/`address` at a peer instead of a hostname. All four are peer-first: anywhere you'd pass a hostname or IP, you can instead pass a peer's device name, its device id (or a unique ≥4-character prefix), its Tailscale hostname, or its Tailscale IP — the same resolver `send()` uses internally.

## mesh.net — raw TCP

`mesh.net` looks like `node:net`. Sockets are real `stream.Duplex` instances, so anything that accepts a Node socket — piping, `http.Server`, the `ws` package, undici — works over the mesh unmodified.

```ts
import { createMeshNode } from '@vibecook/truffle';

const mesh = await createMeshNode({ appId: 'my-app' });

// Server — port 0 binds an ephemeral port; read the real one off `server.port`
// once 'listening' fires.
const server = mesh.net.createServer((socket) => {
  console.log(`connection from ${socket.remotePeerName ?? socket.remoteAddress}`);
  socket.pipe(socket); // echo
});
server.listen(8080, () => console.log(`listening on ${server.port}`));

// Client — `host` accepts a device name, device id/prefix, Tailscale hostname,
// or Tailscale IP.
const client = mesh.net.connect({ host: 'other-machine', port: 8080 });
client.on('connect', () => client.end('hello over the mesh'));
client.pipe(process.stdout);
```

`end()` half-closes the write side (FIN) while reads keep delivering until the peer's own FIN arrives — the same trick BSD/GNU `nc` uses. See `examples/netcat` for the full two-device walkthrough, including piping `stdin`/`stdout` through a mesh socket.

## mesh.http — HTTP over the mesh

`mesh.http` doesn't add a new protocol — it wires `node:http` (and anything built on it, like Express) onto `mesh.net` through the documented custom-`Duplex`/custom-`Agent` seams. Nothing here is mesh-specific beyond that seam, which is what makes it free: no HTTP reimplementation in Rust.

```ts
import http from 'node:http';
import express from 'express';

// Inbound: feed mesh TCP connections into a stock http.Server.
const app = express();
app.get('/api/status', (_req, res) => res.json({ ok: true }));
const httpServer = http.createServer(app);
mesh.net
  .createServer((socket) => httpServer.emit('connection', socket))
  .listen(8080);

// Outbound: fetchText sugar, or drop to raw node:http with the mesh agent.
const { status, body } = await mesh.http.fetchText('other-machine', 8080, '/api/status');
mesh.http.get('http://100.64.0.7:8080/api/status', (res) => {
  // Tailscale IPs work directly as URL hostnames; device names with spaces
  // don't (invalid URL syntax) — use fetchText, or the options-object form:
  // mesh.http.get({ host: 'kitchen pi', port: 8080, path: '/x' }, (res) => …)
});
```

See `examples/express-over-mesh` for a full round trip: a real Express server and a client that exercises it over three routes.

## mesh.dgram — raw UDP

`mesh.dgram` looks like `node:dgram`. Datagram boundaries are preserved end to end.

```ts
const sock = mesh.dgram.createSocket();
sock.on('message', (msg, rinfo) => {
  console.log(`${rinfo.peerName ?? rinfo.address}:${rinfo.port}: ${msg}`);
});
await sock.bind(8081);
sock.send(Buffer.from('ping'), 8081, 'other-machine');
```

Two deliberate deviations from `node:dgram`: `bind()` returns a `Promise<this>` instead of signalling readiness only through the `'listening'` event (binding genuinely round-trips to the sidecar), and calling `send()` before `bind()` throws instead of silently auto-binding an ephemeral port.

## mesh.quic — sessions and streams

`mesh.quic` has no direct `node:` analog — it follows QUIC's own shape (and iroh's precedent): connect to a peer to get a **connection**, then open as many concurrent, independent **streams** on it as you like, with no head-of-line blocking between them. Connections and streams are async-iterable; each stream is a `stream.Duplex`.

```ts
// Server
const server = await mesh.quic.listen(9420); // explicit port required — see below
for await (const conn of server) {
  for await (const stream of conn.streams()) {
    stream.pipe(stream); // echo, independent of every other stream on this connection
  }
}

// Client
const conn = await mesh.quic.connect('other-machine', 9420);
const stream = await conn.openStream();
stream.end('hello');
```

See `examples/quic-streams` for a benchmark that opens several concurrent streams and shows wall time tracking the slowest stream rather than the sum of all of them — the payoff of no head-of-line blocking.

## Constraints

| Constraint | Value | Consequence |
|---|---|---|
| Datagram payload | keep ≤ ~1,200 B | tailnet MTU ≈1280; larger datagrams fragment in the userspace netstack and become loss-prone |
| Datagram/QUIC addressing | IPv4 (`100.x`) only | v6-only peers are unreachable via `mesh.dgram`/`mesh.quic` until a future IPv6 header format ships |
| Bridge concurrency | 256 concurrent TCP connections | shared cap across every `mesh.net` socket on a node |
| Idle reap | 10 minutes by default | the sidecar reaps quiet bridged connections; configurable via the Rust `NodeBuilder::idle_timeout_secs` knob (not yet surfaced in `CreateMeshNodeOptions`) — send app-level keepalives on long-lived quiet sockets meanwhile |
| Reserved ports | 443, 9417 | rejected by `listenTcp`/`listenQuic` with a clear error; `mesh.quic.listen` also has no ephemeral (port `0`) support — the tsnet relay can't report an ephemeral QUIC port back, so pick an explicit port (9420+ is a reasonable default) |
| Throughput | "good, not line-rate" | every byte crosses the userspace tailnet netstack plus a loopback hop to the Go sidecar; no GSO/GRO — fine for control traffic, APIs, and file transfer, not bulk line-rate UDP |
| WS envelope cap | 16 MiB/frame | unrelated to raw transports — `send()`/`broadcast()` are capped; `mesh.net`/`mesh.quic` streams have no frame-size cap of their own |

## Security model

Inbound raw connections are **tailnet-authenticated but not app-authenticated**. Any device on your tailnet (subject to your Tailscale ACLs) can dial a `mesh.net`/`mesh.dgram` listener — unlike the envelope layer (`send`/`broadcast`/`onMessage`), whose WebSocket hello checks both `appId` and WhoIs identity before treating a peer as a peer at all.

Each transport gives you a different amount of identity to defend with:

- **TCP**: every inbound socket (one your server accepted) carries the bridge's WhoIs identity. Check `socket.remotePeerId` / `socket.remotePeerName` before trusting it — both are `null` for "anonymous but tailnet-authenticated" (a real tailnet device truffle can't yet name), so decide up front what your app does with an unnamed peer rather than treating that absence as automatically disqualifying.
- **QUIC**: scoped to your app at the TLS layer via ALPN (`truffle-raw.{appId}`) — a peer running a different `appId` fails the handshake before your code ever sees the connection. `remotePeerId` can still be `null` the same way as TCP.
- **UDP**: no app scoping at all. Identity is the sender's source IP only — WireGuard-authenticated by Tailscale (so it can't be spoofed), but any tailnet device can send to a bound port regardless of `appId`.

QUIC's certificate verification is intentionally permissive (self-signed certs, accept-any client-side verifier): WireGuard already authenticates and encrypts the path, so QUIC's TLS layer exists for protocol compliance, not identity. That's deliberate, not a bug.

## Backing Rust APIs

Working in Rust directly instead of through Node.js? The same functionality lives on `Node`:

| TS namespace | Rust `Node` method(s) |
|---|---|
| `mesh.net` | `open_tcp(peer_ref, port)`, `listen_tcp(port)` |
| `mesh.dgram` | `bind_udp(port)` |
| `mesh.quic` | `connect_quic(peer_ref, port)`, `listen_quic(port)` |

`mesh.http` has no Rust equivalent — it's a TypeScript-only convenience built on `mesh.net` using `node:http`'s Agent/connection seams, so there's nothing to bridge.

## Next Steps

- [`examples/netcat`](https://github.com/jamesyong-42/truffle/tree/main/examples/netcat) — `mesh.net`, end to end
- [`examples/express-over-mesh`](https://github.com/jamesyong-42/truffle/tree/main/examples/express-over-mesh) — `mesh.http` serving a real Express app
- [`examples/quic-streams`](https://github.com/jamesyong-42/truffle/tree/main/examples/quic-streams) — `mesh.quic`, benchmarking concurrent streams
- [Architecture](/guide/architecture) — how the layers underneath this API fit together
