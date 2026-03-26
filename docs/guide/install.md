# Installation

## Quick Install

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

The installer downloads two binaries:
- **`truffle`** -- the CLI
- **`sidecar-slim`** -- the Go sidecar that embeds Tailscale's `tsnet`

## Install Locations

| Platform | Default directory |
|----------|-------------------|
| macOS / Linux | `~/.config/truffle/bin` |
| Windows | `%LOCALAPPDATA%\truffle\bin` |

Override with `--dir <path>` or the `TRUFFLE_INSTALL_DIR` environment variable.

## Installer Options

### Unix (install.sh)

```sh
# Install a specific version
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh -s -- --version v0.1.0

# Custom install directory
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh -s -- --dir /usr/local/bin

# Skip post-install verification
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh -s -- --no-verify
```

### Windows (install.ps1)

```powershell
# Install a specific version
.\install.ps1 -Version v0.1.0

# Custom install directory
.\install.ps1 -Dir "C:\tools\truffle"

# Skip post-install verification
.\install.ps1 -NoVerify
```

## Prerequisites

The only prerequisite is **Tailscale**. Install it from [tailscale.com/download](https://tailscale.com/download) and sign in before running truffle.

Truffle does not require Node.js, Go, or any other runtime. The CLI and sidecar are self-contained binaries.

## Verify Installation

After installing, run:

```sh
truffle doctor
```

This checks that:
- The sidecar binary is found and executable
- Tailscale is installed and running
- Network connectivity is working
- Configuration is valid

## Supported Platforms

| OS | Architecture | Status |
|----|-------------|--------|
| macOS | arm64 (Apple Silicon) | Supported |
| macOS | x64 (Intel) | Supported |
| Linux | x64 | Supported |
| Linux | arm64 | Supported |
| Windows | x64 | Supported |

## Building from Source

If prebuilt binaries are not available for your platform:

```sh
# Clone the repo
git clone https://github.com/jamesyong-42/truffle.git
cd truffle

# Build the CLI (requires Rust toolchain)
cargo build --release -p truffle-cli

# Build the sidecar (requires Go 1.22+)
cd packages/sidecar-slim && go build -o sidecar-slim .

# Install
cp target/release/truffle ~/.config/truffle/bin/
cp packages/sidecar-slim/sidecar-slim ~/.config/truffle/bin/
```

## Updating

Re-run the install script to update to the latest version:

```sh
curl -fsSL https://jamesyong-42.github.io/truffle/install.sh | sh
```

Or with Homebrew:

```sh
brew upgrade truffle
```
