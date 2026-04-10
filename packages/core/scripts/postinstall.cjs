#!/usr/bin/env node

/**
 * Postinstall script for @vibecook/truffle.
 *
 * Verifies the sidecar binary is available via optionalDependencies.
 * If missing (e.g., --no-optional), falls back to downloading from GitHub Releases.
 *
 * This follows the esbuild pattern: optionalDependencies as primary, HTTP download as fallback.
 */

/* eslint-disable @typescript-eslint/no-require-imports */

const { existsSync, mkdirSync, chmodSync } = require('fs');
const { join, dirname } = require('path');
const { execSync } = require('child_process');

const GITHUB_REPO = 'jamesyong-42/truffle';

const PLATFORM_PACKAGES = {
  'darwin-arm64': '@vibecook/truffle-sidecar-darwin-arm64',
  'darwin-x64': '@vibecook/truffle-sidecar-darwin-x64',
  'linux-x64': '@vibecook/truffle-sidecar-linux-x64',
  'linux-arm64': '@vibecook/truffle-sidecar-linux-arm64',
  'win32-x64': '@vibecook/truffle-sidecar-win32-x64',
};

// Go asset names use GOOS/GOARCH conventions (amd64 instead of x64)
const GITHUB_ASSETS = {
  'darwin-arm64': 'tsnet-sidecar-darwin-arm64',
  'darwin-x64': 'tsnet-sidecar-darwin-amd64',
  'linux-x64': 'tsnet-sidecar-linux-amd64',
  'linux-arm64': 'tsnet-sidecar-linux-arm64',
  'win32-x64': 'tsnet-sidecar-windows-amd64.exe',
};

function main() {
  const key = `${process.platform}-${process.arch}`;
  const pkg = PLATFORM_PACKAGES[key];
  const ext = process.platform === 'win32' ? '.exe' : '';
  const binName = `sidecar-slim${ext}`;

  if (!pkg) {
    console.warn(`[truffle] No prebuilt sidecar for ${key}. Build from source: cd packages/sidecar-slim && go build`);
    return;
  }

  // Check if the optionalDependency was installed
  try {
    const pkgJson = require.resolve(`${pkg}/package.json`);
    const binPath = join(dirname(pkgJson), 'bin', binName);
    if (existsSync(binPath)) {
      // Ensure the binary is executable. Historically our tarballs were
      // published with mode 0644 because actions/upload-artifact dropped
      // POSIX bits in CI, so fix it here. No-op on Windows.
      if (process.platform !== 'win32') {
        try {
          chmodSync(binPath, 0o755);
        } catch {
          // Might already be correct, or filesystem is read-only; ignore.
        }
      }
      return; // All good
    }
  } catch {
    // Not installed — fall through to download
  }

  // Fallback: download from GitHub Releases
  const asset = GITHUB_ASSETS[key];
  if (!asset) return;

  const version = require('../package.json').version;
  const url = `https://github.com/${GITHUB_REPO}/releases/download/v${version}/${asset}`;

  console.log(`[truffle] Sidecar not found via npm. Downloading from GitHub Releases...`);

  try {
    const binDir = join(__dirname, '..', 'bin');
    mkdirSync(binDir, { recursive: true });
    const dest = join(binDir, binName);

    execSync(`curl -fsSL "${url}" -o "${dest}"`, { stdio: 'pipe' });

    if (process.platform !== 'win32') {
      chmodSync(dest, 0o755);
    }

    console.log(`[truffle] Sidecar downloaded successfully.`);
  } catch {
    console.warn(`[truffle] Could not download sidecar binary.`);
    console.warn(`[truffle] Download manually: ${url}`);
    console.warn(`[truffle] Or build from source: cd packages/sidecar-slim && go build -o bin/sidecar-slim`);
  }
}

main();
