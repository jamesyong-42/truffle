# Truffle Examples

Each example demonstrates a different Truffle capability. Run from the project root.

## Prerequisites

- Node.js 18+
- Tailscale installed and running
- NAPI addon built: `cd crates/truffle-napi && pnpm run build`

The Go sidecar binary is resolved automatically via `resolveSidecarPath()`. For local development, build it manually:

```bash
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
