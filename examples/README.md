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
```

## Examples

### Discovery

Find and list devices on the mesh network:

```bash
npx tsx examples/discovery/index.ts
```

### Chat

Simple cross-device chat using broadcastEnvelope:

```bash
npx tsx examples/chat/index.ts
```

Run on multiple devices on the same Tailscale network to chat.

### Shared State

Todo list synced across devices using NapiStoreSyncAdapter:

```bash
npx tsx examples/shared-state/index.ts
```

Commands: `add <text>`, `toggle <n>`, `rm <n>`, `list`, `quit`
