const { execSync } = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');

/**
 * Resolve the sidecar binary path for the current platform.
 *
 * Resolution order:
 * 1. Platform-specific npm package (@vibecook/truffle-sidecar-{platform}-{arch})
 * 2. Local workspace bin directories (for development)
 *
 * @returns {string} Absolute path to the sidecar binary.
 * @throws {Error} If the binary cannot be found.
 */
function resolveSidecarPath() {
  const key = `${process.platform}-${process.arch}`;
  const ext = process.platform === 'win32' ? '.exe' : '';
  const binName = `sidecar-slim${ext}`;

  // 1. Try platform-specific npm package
  const platformPackages = {
    'darwin-arm64': '@vibecook/truffle-sidecar-darwin-arm64',
    'darwin-x64': '@vibecook/truffle-sidecar-darwin-x64',
    'linux-x64': '@vibecook/truffle-sidecar-linux-x64',
    'linux-arm64': '@vibecook/truffle-sidecar-linux-arm64',
    'win32-x64': '@vibecook/truffle-sidecar-win32-x64',
  };
  const pkg = platformPackages[key];
  if (pkg) {
    try {
      const pkgJsonPath = require.resolve(`${pkg}/package.json`);
      const binPath = path.join(path.dirname(pkgJsonPath), 'bin', binName);
      if (fs.existsSync(binPath)) return binPath;
    } catch {}
  }

  // 2. Try workspace locations (local dev)
  // Walk up from this file's directory looking for packages/core/bin or packages/sidecar-slim/bin
  let dir = __dirname;
  for (let i = 0; i < 5; i++) {
    const coreBin = path.join(dir, 'packages', 'core', 'bin', binName);
    if (fs.existsSync(coreBin)) return coreBin;
    const slimBin = path.join(dir, 'packages', 'sidecar-slim', 'bin', binName);
    if (fs.existsSync(slimBin)) return slimBin;
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }

  const supported = Object.keys(platformPackages).join(', ');
  throw new Error(
    `Truffle sidecar binary not found for ${key}. ` +
    `Supported platforms: ${supported}. ` +
    `Install the platform package: npm install ${platformPackages[key] || '@vibecook/truffle-sidecar-<platform>'}`
  );
}

/**
 * Open a URL in the system's default browser.
 * Works cross-platform (macOS, Linux, Windows).
 *
 * @param {string} url - The URL to open.
 */
function openUrl(url) {
  const platform = process.platform;
  try {
    if (platform === 'darwin') {
      execSync(`open "${url}"`);
    } else if (platform === 'win32') {
      execSync(`start "" "${url}"`);
    } else {
      execSync(`xdg-open "${url}"`);
    }
  } catch {
    // Silently fail — caller can handle errors via the auth event
  }
}

module.exports = { resolveSidecarPath, openUrl };
