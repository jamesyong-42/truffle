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

### Express over the mesh (HTTP interop)

A stock Express app served over the tailnet via `mesh.net`, queried with
`mesh.http` (RFC 021):

```bash
# device A
pnpm --filter @vibecook/example-express-over-mesh run server
# device B
pnpm --filter @vibecook/example-express-over-mesh run client <device-name>
```

See `examples/express-over-mesh/README.md` for details.
