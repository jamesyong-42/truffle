# Express over the mesh

Run a normal [Express](https://expressjs.com) app and reach it from another
device over the mesh — no ports opened on the public internet, no reverse
proxy. Every request travels the tailnet, WireGuard-encrypted, and only other
devices running this app can connect.

The trick is a single line. Instead of `httpServer.listen(port)` on a host TCP
port, connections come from `mesh.net` and are handed to Node's `http.Server`:

```ts
const httpServer = http.createServer(app); // a plain Express app
mesh.net.createServer((socket) => httpServer.emit('connection', socket)).listen(8080);
```

The client side uses `mesh.http`, an `http.Agent` that dials over the mesh:

```ts
const { status, body } = await mesh.http.fetchText('kitchen-pi', 8080, '/api/status');
// or drop to raw node:http:
mesh.http.get('http://100.64.0.7:8080/api/status', (res) => …);
```

## Prerequisites

- Node.js 18+
- Tailscale installed and running on both devices
- Two devices logged into the same tailnet

## Setup (from the repo root)

```bash
pnpm install
pnpm --filter @vibecook/truffle build
```

On first run each node authenticates with Tailscale; the auth URL opens in your
browser automatically. For a headless device (CI, a server), set a
[Tailscale auth key](https://tailscale.com/kb/1085/auth-keys) instead:

```bash
export TS_AUTHKEY=tskey-auth-...
```

## Run it on two devices

**Device A — the server:**

```bash
pnpm --filter @vibecook/example-express-over-mesh run server
```

It prints its device name and the exact client command to run elsewhere.

**Device B — the client:** pass the server's device name (or its Tailscale IP,
or a device-id prefix):

```bash
pnpm --filter @vibecook/example-express-over-mesh run client "Device A name"
# or:  ... run client 100.64.0.7
```

The client hits `/api/status` twice (via `fetchText` and raw `http.get`), then
`/api/peers`, prints each response, and stops its node.

## Routes

| Method | Path          | Returns                                    |
| ------ | ------------- | ------------------------------------------ |
| GET    | `/api/status` | This device's identity, IP, uptime         |
| GET    | `/api/peers`  | Peers this node currently sees on the mesh |
| POST   | `/api/echo`   | Echoes the posted JSON body back           |

## Notes

- **Peer references with spaces:** a device name like `"living room pi"` is a
  valid peer reference for `mesh.http.fetchText(...)` and the request options
  form, but **not** a valid URL hostname — so the `mesh.http.get('http://…')`
  URL form needs a Tailscale IP or a space-free name. `fetchText` always works.
- **Same `appId`:** both devices must use the same `appId`
  (`'express-over-mesh'` here) to see each other as peers.
- **Not line-rate:** traffic crosses the userspace tailnet netstack plus a
  loopback hop. Great for APIs, control traffic, and app data; not for bulk
  throughput.
