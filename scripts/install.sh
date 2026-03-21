#!/bin/sh
# Install truffle CLI + sidecar
# Usage: curl -fsSL https://truffle.sh/install | sh
#
# Options:
#   --version <tag>    Install a specific version (e.g., v0.1.0)
#   --dir <path>       Custom install directory
#   --no-verify        Skip running 'truffle doctor' after install
#
# Environment:
#   TRUFFLE_INSTALL_DIR   Override default install directory

set -e

# ═══════════════════════════════════════════════════════════════════════════
# Defaults
# ═══════════════════════════════════════════════════════════════════════════

REPO="jamesyong-42/truffle"
VERSION="latest"
INSTALL_DIR=""
VERIFY=true

# ═══════════════════════════════════════════════════════════════════════════
# Parse arguments
# ═══════════════════════════════════════════════════════════════════════════

while [ $# -gt 0 ]; do
    case "$1" in
        --version)
            VERSION="$2"
            shift 2
            ;;
        --dir)
            INSTALL_DIR="$2"
            shift 2
            ;;
        --no-verify)
            VERIFY=false
            shift
            ;;
        --help|-h)
            echo "Usage: install.sh [--version <tag>] [--dir <path>] [--no-verify]"
            echo ""
            echo "Options:"
            echo "  --version <tag>    Install a specific version (e.g., v0.1.0)"
            echo "  --dir <path>       Custom install directory"
            echo "  --no-verify        Skip running 'truffle doctor' after install"
            echo ""
            echo "Environment:"
            echo "  TRUFFLE_INSTALL_DIR   Override default install directory"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Run with --help for usage."
            exit 1
            ;;
    esac
done

# ═══════════════════════════════════════════════════════════════════════════
# Detect platform
# ═══════════════════════════════════════════════════════════════════════════

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
    darwin|linux) ;;
    *)
        echo "Error: Unsupported operating system: $OS"
        echo "truffle supports macOS (darwin) and Linux."
        echo "For Windows, use install.ps1 instead."
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH="x64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *)
        echo "Error: Unsupported architecture: $ARCH"
        echo "truffle supports x86_64 (x64) and aarch64 (arm64)."
        exit 1
        ;;
esac

# ═══════════════════════════════════════════════════════════════════════════
# Resolve install directory
# ═══════════════════════════════════════════════════════════════════════════

if [ -z "$INSTALL_DIR" ]; then
    INSTALL_DIR="${TRUFFLE_INSTALL_DIR:-$HOME/.config/truffle/bin}"
fi

# ═══════════════════════════════════════════════════════════════════════════
# Check dependencies
# ═══════════════════════════════════════════════════════════════════════════

if ! command -v curl >/dev/null 2>&1; then
    echo "Error: curl is required but not installed."
    echo "Install curl and try again."
    exit 1
fi

if ! command -v tar >/dev/null 2>&1; then
    echo "Error: tar is required but not installed."
    echo "Install tar and try again."
    exit 1
fi

# ═══════════════════════════════════════════════════════════════════════════
# Build download URL
# ═══════════════════════════════════════════════════════════════════════════

ASSET="truffle-${OS}-${ARCH}.tar.gz"

if [ "$VERSION" = "latest" ]; then
    URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
else
    URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
fi

# ═══════════════════════════════════════════════════════════════════════════
# Download and install
# ═══════════════════════════════════════════════════════════════════════════

echo "truffle installer"
echo "═══════════════════════════════════════"
echo ""
echo "  Platform:  ${OS}-${ARCH}"
echo "  Version:   ${VERSION}"
echo "  Directory: ${INSTALL_DIR}"
echo ""

mkdir -p "$INSTALL_DIR"

echo "Downloading ${ASSET}..."
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

HTTP_CODE=$(curl -fsSL -w "%{http_code}" -o "$TEMP_DIR/$ASSET" "$URL" 2>/dev/null) || true

if [ ! -f "$TEMP_DIR/$ASSET" ] || [ "$HTTP_CODE" = "404" ]; then
    echo ""
    echo "Error: Failed to download ${ASSET}"
    echo ""
    echo "  URL: ${URL}"
    echo ""
    if [ "$VERSION" != "latest" ]; then
        echo "  Check that version '${VERSION}' exists at:"
        echo "  https://github.com/${REPO}/releases"
    else
        echo "  Check that a release exists at:"
        echo "  https://github.com/${REPO}/releases/latest"
    fi
    echo ""
    echo "  This platform/arch combination may not be available yet."
    exit 1
fi

echo "Extracting to ${INSTALL_DIR}..."
tar xzf "$TEMP_DIR/$ASSET" -C "$INSTALL_DIR"

# Make binaries executable
if [ -f "$INSTALL_DIR/truffle" ]; then
    chmod +x "$INSTALL_DIR/truffle"
fi
if [ -f "$INSTALL_DIR/sidecar-slim" ]; then
    chmod +x "$INSTALL_DIR/sidecar-slim"
fi

echo ""
echo "Installed:"
[ -f "$INSTALL_DIR/truffle" ] && echo "  truffle      -> ${INSTALL_DIR}/truffle"
[ -f "$INSTALL_DIR/sidecar-slim" ] && echo "  sidecar-slim -> ${INSTALL_DIR}/sidecar-slim"
echo ""

# ═══════════════════════════════════════════════════════════════════════════
# PATH check
# ═══════════════════════════════════════════════════════════════════════════

case ":$PATH:" in
    *":$INSTALL_DIR:"*)
        # Already in PATH
        ;;
    *)
        echo "Add truffle to your PATH by adding this line to your shell config:"
        echo ""

        # Detect which shell config to suggest
        SHELL_NAME=$(basename "${SHELL:-/bin/sh}")
        case "$SHELL_NAME" in
            zsh)
                echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.zshrc"
                echo ""
                echo "Then reload:"
                echo "  source ~/.zshrc"
                ;;
            bash)
                echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc"
                echo ""
                echo "Then reload:"
                echo "  source ~/.bashrc"
                ;;
            fish)
                echo "  fish_add_path ${INSTALL_DIR}"
                ;;
            *)
                echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
                echo ""
                echo "Add this to your shell's configuration file."
                ;;
        esac
        echo ""
        ;;
esac

# ═══════════════════════════════════════════════════════════════════════════
# Verify installation
# ═══════════════════════════════════════════════════════════════════════════

if [ "$VERIFY" = true ]; then
    if [ -f "$INSTALL_DIR/truffle" ]; then
        echo "Running 'truffle doctor' to verify installation..."
        echo ""
        "$INSTALL_DIR/truffle" doctor || true
    fi
fi

echo "Run 'truffle up' to start your node and join the mesh."
