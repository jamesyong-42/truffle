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

Todo list synced across devices using NapiStoreSyncAdapter:

```bash
pnpm --filter @vibecook/example-shared-state exec tsx index.ts
```

Commands: `add <text>`, `toggle <n>`, `rm <n>`, `list`, `quit`
