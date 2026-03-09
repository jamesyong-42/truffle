/**
 * Sidecar binary resolution for @vibecook/truffle.
 *
 * Resolves the Go sidecar binary path using a 3-level fallback:
 *   1. Platform-specific npm package (installed via optionalDependencies)
 *   2. Postinstall-downloaded binary (fallback for --no-optional)
 *   3. Error with clear instructions
 *
 * Follows the esbuild pattern for distributing platform-specific binaries.
 */

import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);

const PLATFORM_PACKAGES: Record<string, string> = {
  'darwin-arm64': '@vibecook/truffle-sidecar-darwin-arm64',
  'darwin-x64': '@vibecook/truffle-sidecar-darwin-x64',
  'linux-x64': '@vibecook/truffle-sidecar-linux-x64',
  'linux-arm64': '@vibecook/truffle-sidecar-linux-arm64',
  'win32-x64': '@vibecook/truffle-sidecar-win32-x64',
};

/**
 * Resolve the path to the Go sidecar binary for the current platform.
 *
 * Resolution order:
 *   1. Platform-specific `@vibecook/truffle-sidecar-{os}-{arch}` npm package
 *   2. Binary downloaded by postinstall into `packages/core/bin/`
 *
 * @throws {Error} if no binary is found for the current platform
 */
export function resolveSidecarPath(): string {
  const key = `${process.platform}-${process.arch}`;
  const ext = process.platform === 'win32' ? '.exe' : '';
  const binName = `sidecar-slim${ext}`;

  // 1. Try platform-specific npm package (optionalDependency)
  const pkg = PLATFORM_PACKAGES[key];
  if (pkg) {
    try {
      const pkgJsonPath = require.resolve(`${pkg}/package.json`);
      const binPath = join(dirname(pkgJsonPath), 'bin', binName);
      if (existsSync(binPath)) return binPath;
    } catch {
      // Package not installed — fall through
    }
  }

  // 2. Try postinstall-downloaded binary (fallback)
  const __dirname = dirname(fileURLToPath(import.meta.url));
  const localBin = join(__dirname, '..', 'bin', binName);
  if (existsSync(localBin)) return localBin;

  // 3. Nothing found
  const supported = Object.keys(PLATFORM_PACKAGES).join(', ');
  throw new Error(
    `[truffle] Sidecar binary not found for platform "${key}".\n` +
      `Supported platforms: ${supported}\n` +
      `Try reinstalling: npm install @vibecook/truffle\n` +
      `Or build from source: cd packages/sidecar-slim && go build -o bin/sidecar-slim`,
  );
}
