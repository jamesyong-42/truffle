/**
 * Resolves the path to the `sidecar-slim` Go binary bundled with
 * `@vibecook/truffle`.
 *
 * In dev mode we delegate to the package's own `resolveSidecarPath()`, which
 * reads from `node_modules/@vibecook/truffle-sidecar-*`.
 *
 * In a packaged Electron app, the binary lives inside `app.asar.unpacked`
 * (the `asarUnpack` glob in `package.json` ensures it is extracted from the
 * asar archive). Paths inside asar are not executable, so we rewrite
 * `app.asar` → `app.asar.unpacked` in the app path to locate the unpacked
 * binary on disk.
 */

import { app } from 'electron';
import { join } from 'node:path';
import { resolveSidecarPath as resolveFromPackage } from '@vibecook/truffle';

type PlatformKey =
  | 'darwin-arm64'
  | 'darwin-x64'
  | 'linux-x64'
  | 'linux-arm64'
  | 'win32-x64';

const PLATFORM_PKG: Record<PlatformKey, string> = {
  'darwin-arm64': '@vibecook/truffle-sidecar-darwin-arm64',
  'darwin-x64': '@vibecook/truffle-sidecar-darwin-x64',
  'linux-x64': '@vibecook/truffle-sidecar-linux-x64',
  'linux-arm64': '@vibecook/truffle-sidecar-linux-arm64',
  'win32-x64': '@vibecook/truffle-sidecar-win32-x64',
};

export function resolveSidecarPath(): string {
  if (!app.isPackaged) {
    // Dev mode — let the package resolve from node_modules.
    return resolveFromPackage();
  }

  const platformKey = `${process.platform}-${process.arch}` as PlatformKey;
  const pkg = PLATFORM_PKG[platformKey];
  if (!pkg) {
    throw new Error(
      `Unsupported platform for sidecar: ${platformKey}. ` +
        `Expected one of: ${Object.keys(PLATFORM_PKG).join(', ')}`,
    );
  }

  const ext = process.platform === 'win32' ? '.exe' : '';
  // Rewrite asar → asar.unpacked so the OS can actually execute the binary.
  const unpackedBase = app.getAppPath().replace('app.asar', 'app.asar.unpacked');
  return join(unpackedBase, 'node_modules', pkg, 'bin', `sidecar-slim${ext}`);
}
