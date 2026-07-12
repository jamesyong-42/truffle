# Serving HTTP over the tailnet

`mesh.http.createServer` is a drop-in `node:http.createServer` whose `listen()`
binds the **mesh** instead of a host TCP port. Everything that attaches to a
`node:http.Server` — Express, Koa, Fastify, socket.io, `ws` — attaches to it
unchanged, because it *is* a real `http.Server`.

The reach is the point. A dynamic tsnet listener is raw TCP on the tailnet wire,
so **any Tailscale device can reach your server**: a phone browser, `curl`, a
teammate's laptop — no ports opened to the public internet, no reverse proxy.
And every request arrives with a verified caller identity you can read off the
socket (`req.socket.remotePeer`), spoof-proof, because it comes from the
WireGuard tunnel and not from anything the client sent.

> **Serving vs. messaging.** Mesh messaging (`mesh.send`, `mesh.onMessage`) is
> scoped to your `appId`. A served port is not — it is reachable by the whole
> tailnet, gated only by your Tailscale ACLs (and any middleware you add). For
> HTTP that is usually what you want; when it isn't, gate on identity (below).

## Quickstart

A plain handler, served over the mesh:

```ts
import { createMeshNode } from '@vibecook/truffle';

const mesh = await createMeshNode({ appId: 'my-app' });

const server = mesh.http.createServer((req, res) => {
  res.writeHead(200, { 'content-type': 'text/plain' });
  res.end('hello from the tailnet\n');
});

server.listen(8080, () => {
  console.log(`serving on http://${mesh.dnsName ?? 'this-device'}:8080/`);
});
```

An Express app is the same one line — hand the app to `createServer` instead of
`node:http`:

```ts
import express from 'express';
import { createMeshNode } from '@vibecook/truffle';

const mesh = await createMeshNode({ appId: 'my-app' });

const app = express();
app.get('/', (_req, res) => res.send('hello from the tailnet'));

mesh.http.createServer(app).listen(8080);
```

That's the whole migration from a host server: `app.listen(8080)` becomes
`mesh.http.createServer(app).listen(8080)`. Nothing in your routes knows or
cares that it's on a mesh.

## Who's calling? Identity on every request

`req.socket` is a mesh `TruffleSocket`, which carries the caller's Tailscale
WhoIs identity:

| Property | What it is |
| --- | --- |
| `req.socket.remotePeerName` | WhoIs display name — always set for a tailnet caller |
| `req.socket.remotePeerId` | Stable id for the caller (WhoIs node id) |
| `req.socket.remotePeer` | The full [`Peer`](../rfcs/022-peer-handle-api.md) handle (`deviceId`, `hostname`, `os`, `online`, …), resolved live from the node's registry; `null` if not yet interned |

Frameworks type `req.socket` as a `net.Socket`, so cast it to read the mesh
fields:

```ts
import type { TruffleSocket } from '@vibecook/truffle';

app.get('/api/whoami', (req, res) => {
  const sock = req.socket as unknown as TruffleSocket;
  res.json({
    name: sock.remotePeer?.displayName ?? sock.remotePeerName ?? null,
    deviceId: sock.remotePeer?.deviceId ?? null,
  });
});
```

Because the identity is verified, it makes a real access gate. Drop a
middleware in front of your routes and 403 anyone who isn't on the list:

```ts
import type { TruffleSocket } from '@vibecook/truffle';

const ALLOWED = new Set(['alice-laptop', 'bob-phone']); // device display names

app.use((req, res, next) => {
  const peer = (req.socket as unknown as TruffleSocket).remotePeer;
  if (!peer || !ALLOWED.has(peer.displayName)) {
    res.status(403).json({ error: 'forbidden' });
    return;
  }
  next();
});
```

Two notes on that gate:

- **It can't be spoofed.** The identity comes from the WireGuard tunnel's WhoIs
  lookup, not a client-supplied header, so there is nothing for a caller to
  forge.
- **Prefer `deviceId` for a durable allow-list.** A `displayName` is a mutable
  label; `peer.deviceId` is a stable ULID. Match on the id when the list has to
  survive a device rename. `remotePeer` is `null` only for a caller with no
  interned handle — on the tailnet that just means "not resolved yet"; a `null`
  here is your cue to reject rather than assume.

## TLS for browsers — the `tls` option

WireGuard already encrypts and authenticates every byte on the tailnet, so
app-to-app traffic needs nothing more. That's why `tls` defaults to **false**.

Turn it on when a **browser** is the client:

```ts
const server = mesh.http.createServer({ tls: true }, app);
server.listen(443, () => {
  console.log(`serving on https://${mesh.dnsName}/`);
});
```

`tls: true` terminates TLS in the sidecar with an automatic MagicDNS
certificate. It doesn't buy you secrecy — WireGuard already did that — it buys
the things browsers gate on TLS: the padlock, secure-context APIs (clipboard,
service workers, and friends), and `wss:`. Inbound sockets then carry
`encrypted: true`, so Express's `req.secure` and `req.protocol === 'https'`
report correctly.

What to know before you flip it on:

- **Your tailnet must have MagicDNS and HTTPS certificates enabled** in the
  admin console. Without them the handshake fails.
- **The certificate matches only the full `.ts.net` FQDN.** Put
  `mesh.dnsName` in your URLs — `https://<short-name>/` fails by design,
  while plain `http://<short-name>/` still works.
- **First handshake per name does ACME issuance** (a few seconds). The mesh
  pre-warms it at listen time, so the latency lands on you at startup rather
  than on your first visitor.
- **TLS termination and port 443 need a current sidecar build.** Listening on
  `443` — the bare `https://name.ts.net/` URL with no port suffix — works on
  RFC 023 sidecars; against an older sidecar the bind fails with an explicit
  "upgrade the sidecar" error (its legacy internal listener still squats 443).
  Any other port works on both.

## How it differs from a host server

`createServer` returns a genuine `http.Server` (so `instanceof` holds and
frameworks are none the wiser), but three things behave differently because the
mesh isn't a host socket:

- **`listen()` takes a port and nothing else.** No host, backlog, or socket
  path — a mesh node has exactly one address. Passing a host throws. (This is
  what trips `serverFactory` frameworks; see Fastify below.) `listen(0)` binds
  an ephemeral port; read it back from `server.port` after `'listening'`.
- **Idle keep-alive sockets aren't reaped on a timer.** The bridge doesn't
  surface socket-level idle activity to Node, so there's no inactivity timeout.
  `server.close()` handles it for you: it sweeps idle connections
  (`closeIdleConnections()`), lets in-flight responses finish, then releases the
  mesh port. For a hard stop that also cuts active responses, call
  `server.closeAllConnections()` — it works because the server tracks every
  socket it was handed.
- **`address()` is `{ port } | null`**, not an `AddressInfo`. Mesh listeners
  have no local IP or family.

## Framework recipes

### Fastify (via `serverFactory`)

Fastify builds its own server, so hand it `createServer` through the documented
`serverFactory` hook. The one catch is `listen()`: `app.listen()` passes a host,
which a mesh server rejects — so build the routes, then drive the underlying
server directly.

```ts
import Fastify from 'fastify';
import { createMeshNode } from '@vibecook/truffle';

const mesh = await createMeshNode({ appId: 'my-app' });

const app = Fastify({
  serverFactory: (handler) => mesh.http.createServer(handler),
});

app.get('/api/status', async () => ({ ok: true }));

await app.ready();          // wire the routes into the handler
app.server.listen(8080);    // drive the mesh listener (no host arg)
```

### WebSocket (via the `ws` package)

Because the mesh server is a real `http.Server`, `ws`'s standard "attach to a
server" mode just works — `WebSocketServer({ server })` hooks the server's
`'upgrade'` event, which the mesh server emits exactly like a host server would.

```ts
import { WebSocketServer } from 'ws'; // optional peer dependency: `npm i ws`
import type { TruffleSocket } from '@vibecook/truffle';
import { createMeshNode } from '@vibecook/truffle';

const mesh = await createMeshNode({ appId: 'my-app' });

const server = mesh.http.createServer();
const wss = new WebSocketServer({ server }); // attaches to 'upgrade'

wss.on('connection', (ws, req) => {
  const who = (req.socket as unknown as TruffleSocket).remotePeerName;
  ws.send(`hello ${who ?? 'stranger'}`);
  ws.on('message', (data) => ws.send(`echo: ${data}`));
});

server.listen(8080);
```

`ws` is an optional peer dependency of `@vibecook/truffle` — install it
alongside. If you only want a WebSocket surface and no HTTP routes,
`mesh.ws.createServer({ port })` wraps this same wiring for you (see the
`ws-chat-over-mesh` example).

## Reaching it from a phone

Any device on your tailnet can hit the server by MagicDNS name — no app install,
just a browser:

1. Run the server on a machine that's on your tailnet: `server.listen(8080)`.
2. Get its MagicDNS name from `mesh.getLocalInfo()` — `tailscaleHostname` is the
   short name (`truffle-my-app-laptop`), `dnsName` is the full
   `<name>.ts.net` FQDN.
3. On your phone (same tailnet, Tailscale connected, MagicDNS on), open
   `http://truffle-my-app-laptop:8080/`. The request rides the tailnet
   end-to-end; nothing is exposed to the public internet.

The same URL works from `curl`, another laptop, or a teammate's machine —
anything your tailnet ACLs allow. If that's broader than you want, put the
identity middleware from above in front of it.

## Publishing existing services with `mesh.serve`

Not everything you want on the tailnet is a handler you wrote. When the bytes
are a **static directory** or **another process already listening on localhost**
— a Vite dev server, Grafana, a container — you don't write a handler at all.
`mesh.serve(config)` publishes it in one call, the way `tailscale serve` does.
The bytes stay in the Go sidecar (they never cross into JS), and TLS is on by
default because the audience is browsers.

The rule of thumb:

> You wrote the request handler? `mesh.http.createServer`. It's a directory or
> another process? `mesh.serve`.

### Three shapes

```ts
// A. Expose a local process (dev server, dashboard, container)
const h = await mesh.serve({ port: 443, target: 'http://localhost:3000' });
console.log(h.url); // https://my-app.tail1234.ts.net

// B. Serve a static directory (single-page app)
await mesh.serve({ port: 443, dir: './public', fallback: '/index.html' });

// C. Mixed path routes — an SPA plus its APIs, on one port
await mesh.serve({
  port: 443,
  routes: {
    '/api':     'http://localhost:8000',
    '/grafana': { target: 'http://localhost:3001' },
    '/':        { dir: './public', fallback: '/index.html' },
  },
});
```

Each call resolves to a `ServeHandle` once the listener is up.

### The handle

```ts
const h = await mesh.serve({ port: 443, target: 'http://localhost:3000' });
h.on('serveError', ({ code, message }) => console.error(`serve ${code}: ${message}`));
console.log(`published at ${h.url}`);
// …later
await h.close();
```

- **`url` / `port`** — the public URL and the tailnet port. Read `h.url` rather
  than string-building it: Tailscale dedupes name collisions with `-1`/`-2`
  suffixes, so the granted URL isn't always the one you'd guess.
- **`id`** — the proxy id, unique per node. Defaults to `serve-${port}`, so two
  serves on the same port collide; pass a distinct `id` to run more than one.
- **`config`** — the normalized config that created it (defaults filled, `dir`s
  resolved to absolute), frozen. Persist it and replay `mesh.serve(h.config)` to
  recreate the same serve after a restart; the library never persists on its
  own.
- **`close()`** — stop serving and release the port (idempotent; emits
  `'close'`).
- It's an **`EventEmitter`** for sidecar runtime errors. Listen on
  `'serveError'` — `(info: { code, message })` — which is always safe to leave
  unlistened. (A conventional `'error'` fires too, but only when something is
  listening, since an unhandled `'error'` would crash the process.)

### TLS, targets, and access

- **TLS defaults on** here (`createServer` defaults it off). The serve audience
  is browsers, so you get the padlock, secure-context APIs, and `wss:` out of
  the box — the same MagicDNS-certificate prerequisites as the TLS section above
  apply. Pass `tls: false` for plain HTTP; WireGuard still encrypts the tailnet
  hop.
- **Targets are loopback-only by default.** A `target` must be a full
  `http(s)://` URL, and it must resolve to localhost unless you pass
  `allowNonLoopback: true`. This is deliberate: a LAN target turns your node into
  a pivot into its network, so opening that up is an explicit opt-in.
- **Restrict who can reach it with `allow`.** The serve engine is the one place
  your own code isn't running to gate requests, so it takes an allow-list:
  `allow: ['*@corp.com']` — Tailscale loginName globs, matched against the
  caller's verified WhoIs identity; a non-match gets a bare 403. Absent = the
  whole tailnet. Set `allow` on a single route to override the config-level list
  for that prefix.

### Routes and static serving

`routes` keys are path prefixes, matched by **longest prefix** (order doesn't
matter). A value is either a backend URL string or an object for finer control:

- `{ target, stripPrefix?, allow? }` — proxy to a backend. `stripPrefix`
  defaults **false** (Grafana-style backends want the prefix kept); set it true
  to strip the matched prefix before proxying.
- `{ dir, fallback?, allow? }` — serve a directory. Static serving is Go's
  `http.FileServer`: `index.html` at directory roots, ETag/Range/mime for free,
  **no directory listing**, dotfiles denied. `fallback` rewrites misses to one
  path — the SPA trick (`/index.html`, so client-side routes resolve).

### Not supported yet

`funnel: true` (public-internet exposure) is reserved and **rejected** today with
a pointer to RFC 023 §9.6 — it lands in a later phase. Announcing serves for
other peers to browse is also future work: there's no `announce` option on
`serve` yet.

## Constraints

| Constraint | Detail |
| --- | --- |
| `createServer` path | Rides the raw-transport bridge: up to 256 concurrent connections, HTTP/1.1 only |
| `createServer` reaping | No inactivity timer; `close()` sweeps idle sockets, `closeAllConnections()` is the hard stop |
| `serve` path | Bytes stay in Go — its own connections (no 256-conn bridge cap), 120 s idle timeout, HTTP/2 to browsers for free |
| `serve` targets | Loopback-only unless `allowNonLoopback`; a `target` must be a full `http(s)://` URL |
| Exposure | Reachable by the entire tailnet, gated by Tailscale ACLs (and `serve`'s `allow`) — not scoped to `appId` |
| TLS | Default off for `createServer`, on for `serve`; needs MagicDNS + HTTPS certs enabled; cert matches the full `.ts.net` FQDN; current sidecar |
| Ports | One listener per port per node; `9417` (the mesh's session port) is reserved; `443` needs an RFC 023 sidecar |

Throughput note: mesh traffic crosses the userspace tailnet netstack plus a
loopback hop. It's great for APIs, dashboards, control traffic, and app data —
not for bulk line-rate transfer.
