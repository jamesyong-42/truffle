# Truffle Examples

Each example demonstrates a different Truffle capability. Run from the project root.

## Prerequisites

- Node.js 18+
- Tailscale installed and running
- Sidecar binary built: `cd packages/sidecar && make build`

## Examples

### Discovery

Find and list devices on the mesh network:

```bash
npx tsx examples/discovery/index.ts
```

### Chat

Simple cross-device chat using MessageBus pub/sub:

```bash
npx tsx examples/chat/index.ts
```

Run on multiple devices on the same Tailscale network to chat.

### Shared State

Todo list synced across devices using StoreSyncAdapter:

```bash
npx tsx examples/shared-state/index.ts
```

Commands: `add <text>`, `toggle <n>`, `rm <n>`, `list`, `quit`
