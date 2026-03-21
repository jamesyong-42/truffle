---
layout: home
hero:
  name: truffle
  text: Mesh networking for your devices
  tagline: Built on Tailscale. P2P. Zero config. Cross-platform.
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started
    - theme: alt
      text: View on GitHub
      link: https://github.com/jamesyong-42/truffle
features:
  - title: Zero Config
    details: One command to start. Auto-discovers peers on your Tailscale network.
  - title: P2P Mesh
    details: Direct connections between all nodes. No coordinator, no single point of failure.
  - title: Cross-Platform
    details: macOS, Linux, Windows. CLI + Rust library + Node.js NAPI + Tauri plugin.
---

<div class="install-block">

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

## Quick Start

```sh
# Start your node and join the mesh
truffle up

# See all nodes on your network
truffle ls

# Send a message to another node
truffle send laptop "deploy is done"

# Copy a file to another node
truffle cp ./report.pdf server:/tmp/

# Check connectivity
truffle ping laptop
```

</div>
