/**
 * @vibecook/truffle-sidecar-bin
 *
 * Provides the path to the prebuilt tsnet-sidecar binary.
 */

import { join, dirname } from 'node:path';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const BINARY_NAME = process.platform === 'win32' ? 'tsnet-sidecar.exe' : 'tsnet-sidecar';

/**
 * Get the path to the sidecar binary.
 * @returns {string} Absolute path to the binary.
 * @throws {Error} If the binary is not found.
 */
export function getSidecarPath() {
  const binPath = join(__dirname, '..', 'bin', BINARY_NAME);
  if (!existsSync(binPath)) {
    throw new Error(
      `Sidecar binary not found at ${binPath}. ` +
        'Run "cd packages/sidecar && make build" to build from source.',
    );
  }
  return binPath;
}
