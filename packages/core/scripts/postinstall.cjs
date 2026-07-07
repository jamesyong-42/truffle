#!/usr/bin/env node

/**
 * Postinstall script for @vibecook/truffle.
 *
 * Primary path: the platform-specific sidecar arrives via optionalDependencies
 * (npm integrity-protected). Fallback (e.g. `--no-optional`): download the
 * binary from GitHub Releases over HTTPS, then verify its SHA-256 against the
 * checksums shipped *inside this package* (`sidecar-checksums.json`). When a
 * checksum is present we fail closed on mismatch; when absent we warn loudly
 * that integrity could not be verified.
 *
 * This replaces the previous unverified `curl | chmod +x` fallback.
 */

/* eslint-disable @typescript-eslint/no-require-imports */

const {
  existsSync,
  mkdirSync,
  chmodSync,
  createWriteStream,
  createReadStream,
  readFileSync,
  unlinkSync,
} = require('fs');
const { join, dirname } = require('path');
const https = require('https');
const crypto = require('crypto');

const GITHUB_REPO = 'jamesyong-42/truffle';
const DOWNLOAD_TIMEOUT_MS = 60_000;
const MAX_ATTEMPTS = 3;

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

/** Download `url` to `dest` over HTTPS, following redirects, with a hard timeout. */
function httpsDownload(url, dest, timeoutMs) {
  return new Promise((resolve, reject) => {
    const req = https.get(url, { headers: { 'User-Agent': 'truffle-postinstall' } }, (res) => {
      // GitHub release downloads redirect to a CDN — follow 3xx.
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        httpsDownload(res.headers.location, dest, timeoutMs).then(resolve, reject);
        return;
      }
      if (res.statusCode !== 200) {
        res.resume();
        reject(new Error(`HTTP ${res.statusCode}`));
        return;
      }
      const file = createWriteStream(dest);
      res.pipe(file);
      file.on('finish', () => file.close((err) => (err ? reject(err) : resolve())));
      file.on('error', (err) => {
        try {
          unlinkSync(dest);
        } catch {
          /* ignore */
        }
        reject(err);
      });
    });
    req.setTimeout(timeoutMs, () => req.destroy(new Error(`timed out after ${timeoutMs}ms`)));
    req.on('error', reject);
  });
}

/** Compute the hex SHA-256 of a file. */
function sha256File(path) {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash('sha256');
    const s = createReadStream(path);
    s.on('data', (d) => hash.update(d));
    s.on('end', () => resolve(hash.digest('hex')));
    s.on('error', reject);
  });
}

/** Look up the expected SHA-256 for a given version/asset from the shipped checksum map. */
function loadExpectedChecksum(version, asset) {
  try {
    const map = JSON.parse(readFileSync(join(__dirname, '..', 'sidecar-checksums.json'), 'utf8'));
    const byVersion = map[version] || map[`v${version}`];
    return byVersion ? byVersion[asset] : undefined;
  } catch {
    return undefined;
  }
}

/** Release tags are `truffle-v{version}` (release-please monorepo tag scheme), NOT `v{version}`. */
function buildDownloadUrl(version, asset) {
  return `https://github.com/${GITHUB_REPO}/releases/download/truffle-v${version}/${asset}`;
}

/** Throw if `actual` does not match `expected` (hex, case-insensitive). */
function assertChecksum(actual, expected, label) {
  if (actual.toLowerCase() !== expected.toLowerCase()) {
    throw new Error(
      `SECURITY: sidecar checksum mismatch for ${label}. Expected ${expected}, got ${actual}.`,
    );
  }
}

async function main() {
  const key = `${process.platform}-${process.arch}`;
  const pkg = PLATFORM_PACKAGES[key];
  const ext = process.platform === 'win32' ? '.exe' : '';
  const binName = `sidecar-slim${ext}`;

  if (!pkg) {
    console.warn(
      `[truffle] No prebuilt sidecar for ${key}. Build from source: cd packages/sidecar-slim && go build`,
    );
    return;
  }

  // Primary: was the platform optionalDependency installed?
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

  // Fallback: download from GitHub Releases (verified).
  const asset = GITHUB_ASSETS[key];
  if (!asset) return;

  const version = require('../package.json').version;
  const url = buildDownloadUrl(version, asset);
  const expected = loadExpectedChecksum(version, asset);

  console.log(`[truffle] Sidecar not found via npm. Downloading from GitHub Releases...`);

  const binDir = join(__dirname, '..', 'bin');
  const dest = join(binDir, binName);

  // Retry only the network download; a checksum mismatch is deterministic and
  // must NOT be retried (it would just re-download the same bad bytes).
  let downloaded = false;
  for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
    try {
      mkdirSync(binDir, { recursive: true });
      await httpsDownload(url, dest, DOWNLOAD_TIMEOUT_MS);
      downloaded = true;
      break;
    } catch (err) {
      console.warn(`[truffle] Download attempt ${attempt}/${MAX_ATTEMPTS} failed: ${err.message}`);
    }
  }

  if (!downloaded) {
    console.warn(`[truffle] Could not download sidecar binary.`);
    console.warn(`[truffle] Download manually: ${url}`);
    console.warn(
      `[truffle] Or build from source: cd packages/sidecar-slim && go build -o bin/sidecar-slim`,
    );
    return;
  }

  // Integrity check.
  if (expected) {
    const actual = await sha256File(dest);
    try {
      assertChecksum(actual, expected, `truffle-v${version}/${asset}`);
    } catch (err) {
      try {
        unlinkSync(dest);
      } catch {
        /* ignore */
      }
      console.error(`[truffle] ${err.message} Deleted the download and refusing to install it.`);
      process.exitCode = 1;
      return;
    }
  } else {
    console.warn(
      `[truffle] WARNING: no pinned checksum for truffle-v${version}/${asset}; integrity NOT verified. ` +
        `Prefer installing with optionalDependencies enabled.`,
    );
  }

  if (process.platform !== 'win32') {
    chmodSync(dest, 0o755);
  }

  console.log(`[truffle] Sidecar downloaded${expected ? ' and verified' : ''}.`);
}

module.exports = { buildDownloadUrl, assertChecksum, loadExpectedChecksum };

if (require.main === module) {
  main().catch((err) => {
    console.error(`[truffle] postinstall failed: ${err.message}`);
    process.exitCode = 1;
  });
}
