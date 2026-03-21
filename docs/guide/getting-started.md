# Getting Started

## Prerequisites

- **Tailscale** installed and authenticated on each device you want to connect
- That's it. Truffle handles the rest.

## Install

::: code-group

```sh [macOS / Linux]
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh
```

```powershell [Windows]
iwr -useb https://jamesyong-42.github.io/truffle/install.ps1 | iex
```

```sh [Homebrew]
brew install jamesyong-42/tap/truffle
```

:::

The installer downloads the `truffle` CLI and the `sidecar-slim` binary, places them in `~/.config/truffle/bin` (Unix) or `%LOCALAPPDATA%\truffle\bin` (Windows), and adds them to your PATH.

## Quick Start

### 1. Start your node

```sh
truffle up
```

Output:

```
  truffle
  ───────────────────────────────────────

  Node        james-macbook
  Status      ● online
  IP          100.64.0.3
  DNS         truffle-desktop-a1b2.tailnet.ts.net
  Mesh        3 nodes online
  Uptime      just started
```

### 2. See who's on the mesh

```sh
truffle ls
```

```
  NAME              IP             OS       STATUS
  james-macbook     100.64.0.3     macOS    ● online
  work-desktop      100.64.0.5     Linux    ● online
  home-server       100.64.0.7     Linux    ● online
```

### 3. Ping a node

```sh
truffle ping work-desktop
```

### 4. Send a message

```sh
truffle send work-desktop "build is done"
```

### 5. Copy a file

```sh
truffle cp ./report.pdf work-desktop:/tmp/
```

### 6. Stop your node

```sh
truffle down
```

## What's happening under the hood

1. `truffle up` starts the Go sidecar, which joins your Tailscale network
2. Truffle discovers other truffle nodes on the tailnet automatically
3. Direct P2P WebSocket connections are established between all nodes -- no coordinator, no hub
4. You can now send messages, copy files, proxy ports, and sync state across your devices

## Next Steps

- [CLI Quick Start](/guide/cli) -- full command reference
- [Installation](/guide/install) -- detailed install options
- [Architecture](/guide/architecture) -- how it works
- [Mesh Networking](/guide/mesh-networking) -- programmatic usage with the Rust/Node.js library
