#!/usr/bin/env node

/**
 * Postinstall script for @vibecook/truffle-sidecar-bin.
 *
 * Downloads the correct prebuilt tsnet-sidecar binary for the current platform.
 * Falls back to a helpful error message with build-from-source instructions.
 */

import { createWriteStream, mkdirSync, chmodSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { get } from 'node:https';

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_ROOT = join(__dirname, '..');
const BIN_DIR = join(PKG_ROOT, 'bin');
const BINARY_NAME = process.platform === 'win32' ? 'tsnet-sidecar.exe' : 'tsnet-sidecar';
const BINARY_PATH = join(BIN_DIR, BINARY_NAME);

// Platform/arch mapping to GitHub release asset names
const PLATFORM_MAP = {
  'darwin-arm64': 'tsnet-sidecar-darwin-arm64',
  'darwin-x64': 'tsnet-sidecar-darwin-amd64',
  'linux-x64': 'tsnet-sidecar-linux-amd64',
  'linux-arm64': 'tsnet-sidecar-linux-arm64',
  'win32-x64': 'tsnet-sidecar-windows-amd64.exe',
};

function getPlatformKey() {
  return `${process.platform}-${process.arch}`;
}

function getDownloadUrl(version) {
  const key = getPlatformKey();
  const asset = PLATFORM_MAP[key];
  if (!asset) {
    return null;
  }
  return `https://github.com/jamesyong-42/truffle/releases/download/v${version}/${asset}`;
}

function download(url) {
  return new Promise((resolve, reject) => {
    const request = (downloadUrl) => {
      get(downloadUrl, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          request(res.headers.location);
          return;
        }
        if (res.statusCode !== 200) {
          reject(new Error(`Download failed: HTTP ${res.statusCode}`));
          return;
        }

        mkdirSync(BIN_DIR, { recursive: true });
        const file = createWriteStream(BINARY_PATH);
        res.pipe(file);
        file.on('finish', () => {
          file.close();
          if (process.platform !== 'win32') {
            chmodSync(BINARY_PATH, 0o755);
          }
          resolve();
        });
        file.on('error', reject);
      }).on('error', reject);
    };
    request(url);
  });
}

async function main() {
  // Skip if binary already exists (e.g., from a previous install)
  if (existsSync(BINARY_PATH)) {
    console.log(`[truffle-sidecar-bin] Binary already exists at ${BINARY_PATH}`);
    return;
  }

  const key = getPlatformKey();
  if (!PLATFORM_MAP[key]) {
    printBuildInstructions(
      `Unsupported platform: ${key}\nSupported: ${Object.keys(PLATFORM_MAP).join(', ')}`,
    );
    return;
  }

  // Read version from package.json
  const { default: pkg } = await import(join(PKG_ROOT, 'package.json'), {
    with: { type: 'json' },
  });
  const version = pkg.version;

  const url = getDownloadUrl(version);
  console.log(`[truffle-sidecar-bin] Downloading sidecar binary for ${key}...`);
  console.log(`[truffle-sidecar-bin] URL: ${url}`);

  try {
    await download(url);
    console.log(`[truffle-sidecar-bin] Installed to ${BINARY_PATH}`);
  } catch (err) {
    printBuildInstructions(`Download failed: ${err.message}`);
  }
}

function printBuildInstructions(reason) {
  console.warn(`
[truffle-sidecar-bin] ${reason}

  No prebuilt binary available. Build from source:

  Prerequisites: Go 1.25+

  Steps:
    cd packages/sidecar
    make build

  Then pass the binary path to MeshNode:
    sidecarPath: './packages/sidecar/bin/tsnet-sidecar'
`);
}

main().catch((err) => {
  // Don't fail the install - just warn
  console.warn(`[truffle-sidecar-bin] Warning: ${err.message}`);
  printBuildInstructions(err.message);
});
