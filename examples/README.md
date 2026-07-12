# Truffle Examples

Each example demonstrates a different Truffle capability.

## Prerequisites

- Node.js 18+
- Tailscale installed and running
- `npm install @vibecook/truffle` (prebuilt native addon + sidecar binary included)

For local development (cloning this repo), build from source instead:

```bash
pnpm install
cd crates/truffle-napi && pnpm run build
cd packages/sidecar-slim && go build -o bin/sidecar-slim
cp bin/sidecar-slim ../../packages/core/bin/sidecar-slim
```

## Examples

Run from the project root:

### Discovery

Find and list devices on the mesh network:

```bash
pnpm --filter @vibecook/example-discovery exec tsx index.ts
```

### Chat

Simple cross-device chat using broadcastEnvelope:

```bash
pnpm --filter @vibecook/example-chat exec tsx index.ts
```

Run on multiple devices on the same Tailscale network to chat.

### Shared State

Todo list synced across devices using NapiSyncedStore:

```bash
pnpm --filter @vibecook/example-shared-state exec tsx index.ts
```

Commands: `add <text>`, `toggle <n>`, `rm <n>`, `list`, `quit`

### Netcat (raw TCP over the mesh)

netcat-style CLI built on `mesh.net` (RFC 021) — pipe bytes between any two
devices with no server:

```bash
# device A
pnpm --filter @vibecook/example-netcat exec tsx src/main.ts listen 9000 > received.png
# device B
pnpm --filter @vibecook/example-netcat exec tsx src/main.ts connect <device-name> 9000 < photo.png
```

See `examples/netcat/README.md` for all commands (`listen`, `connect`, `peers`).

### QUIC streams (multiplexing)

Echo server + bench client over `mesh.quic` (RFC 021) — N concurrent
streams on one connection, timed individually to show there's no
head-of-line blocking:

```bash
# device A
pnpm --filter @vibecook/example-quic-streams exec tsx src/main.ts serve 9420
# device B
pnpm --filter @vibecook/example-quic-streams exec tsx src/main.ts bench <device-name> 9420 8
```

### Express over the mesh (HTTP interop)

A stock Express app served to the tailnet with `mesh.http.createServer`
(RFC 023) and queried with `mesh.http` (RFC 021). Every request carries the
caller's verified identity on `req.socket` — the server's `/api/whoami` route
reflects it back:

```bash
# device A
pnpm --filter @vibecook/example-express-over-mesh run server
# device B
pnpm --filter @vibecook/example-express-over-mesh run client <device-name>
```

See `examples/express-over-mesh/README.md` for details, or the
[Serving HTTP guide](../docs/guide/serving-http.md) for the full `createServer`
surface — TLS for browsers, Fastify, WebSocket upgrade, and reaching a server
from a phone.

### Serve a static SPA over the mesh

Publish a directory of static files to the whole tailnet with
`mesh.serve({ dir, fallback })` (RFC 023) — no handler, no Express. The sidecar
serves the files, and `fallback` keeps single-page-app routes working on a hard
refresh:

```bash
pnpm --filter @vibecook/example-serve-static-spa start
```

Prints an `https://` URL any tailnet device (phone browsers included) can open.
See `examples/serve-static-spa/README.md`.

### Expose a local dev server over the mesh

Publish something already listening on `localhost` — a Vite dev server, Grafana,
an internal API — to your tailnet with `mesh.serve({ target })` (RFC 023). The
sidecar reverse-proxies it; the bytes never touch JS:

```bash
pnpm --filter @vibecook/example-expose-dev-server start
```

See `examples/expose-dev-server/README.md`.

### WebSocket over the mesh

Group chat built on `mesh.ws` (RFC 021) — the `ws` package running over mesh
TCP, zero new protocol code. One device hosts and broadcasts; the others
connect and chat:

```bash
# device A (host)
pnpm --filter @vibecook/example-ws-chat-over-mesh exec tsx src/main.ts serve 9500
# devices B, C, … (join)
pnpm --filter @vibecook/example-ws-chat-over-mesh exec tsx src/main.ts connect <device-name> 9500
```

`ws` is an optional peer dependency of `@vibecook/truffle` — install it
alongside (this example does). See `examples/ws-chat-over-mesh/README.md`.
