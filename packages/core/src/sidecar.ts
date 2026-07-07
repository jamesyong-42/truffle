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

import { closeSync, existsSync, openSync, readSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);

/**
 * Best-effort check that a local binary's architecture matches this process.
 *
 * The `bin/` fallback is populated by postinstall for the *current* platform,
 * but a stray/leftover binary of the wrong arch would otherwise be returned and
 * fail later with a cryptic `ENOEXEC`. We sniff the executable's magic bytes and
 * only reject on a *positive* mismatch; when we can't tell, we allow it.
 */
function binaryMatchesPlatform(path: string): boolean {
  let fd: number;
  try {
    fd = openSync(path, 'r');
  } catch {
    return false;
  }
  try {
    const buf = Buffer.alloc(20);
    if (readSync(fd, buf, 0, 20, 0) < 20) return true; // too small to judge

    // Mach-O 64-bit (macOS): LE magic 0xFEEDFACF, cputype at offset 4.
    if (buf[0] === 0xcf && buf[1] === 0xfa && buf[2] === 0xed && buf[3] === 0xfe) {
      const cpuType = buf.readUInt32LE(4);
      if (process.arch === 'arm64') return cpuType === 0x0100000c; // CPU_TYPE_ARM64
      if (process.arch === 'x64') return cpuType === 0x01000007; // CPU_TYPE_X86_64
      return true;
    }
    // ELF (Linux): magic 0x7F 'E' 'L' 'F', e_machine at offset 18 (LE).
    if (buf[0] === 0x7f && buf[1] === 0x45 && buf[2] === 0x4c && buf[3] === 0x46) {
      const machine = buf.readUInt16LE(18);
      if (process.arch === 'x64') return machine === 0x3e; // EM_X86_64
      if (process.arch === 'arm64') return machine === 0xb7; // EM_AARCH64
      return true;
    }
    // PE (Windows) / unknown format — don't block.
    return true;
  } finally {
    closeSync(fd);
  }
}

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

  // 2. Try postinstall-downloaded binary (fallback), but only if its arch
  //    matches this process — a wrong-arch leftover would fail with ENOEXEC.
  const __dirname = dirname(fileURLToPath(import.meta.url));
  const localBin = join(__dirname, '..', 'bin', binName);
  if (existsSync(localBin) && binaryMatchesPlatform(localBin)) return localBin;

  // 3. Nothing found
  const supported = Object.keys(PLATFORM_PACKAGES).join(', ');
  throw new Error(
    `[truffle] Sidecar binary not found for platform "${key}".\n` +
      `Supported platforms: ${supported}\n` +
      `Try reinstalling: npm install @vibecook/truffle\n` +
      `Or build from source: cd packages/sidecar-slim && go build -o bin/sidecar-slim`,
  );
}
