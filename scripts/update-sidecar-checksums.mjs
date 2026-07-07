#!/usr/bin/env node
// Regenerate the committed sidecar SHA-256 checksum files for a release.
//
// Usage: node scripts/update-sidecar-checksums.mjs <version> <assets-dir>
//
// Computes the SHA-256 of each `tsnet-sidecar-*` asset found in <assets-dir>
// and writes them, keyed by <version> then by asset filename, into BOTH
// checksum trust anchors:
//   - packages/core/sidecar-checksums.json        (postinstall download-fallback verification)
//   - crates/truffle-sidecar/sidecar-checksums.json (build.rs download verification)
// The `_comment` field and any existing version entries are preserved, and the
// output is byte-stable (2-space indent, trailing newline) so re-running for an
// unchanged release produces no diff.
//
// Run by .github/workflows/release-sidecar.yml after the per-platform assets are
// built; also runnable by hand to backfill or verify a release's checksums.

import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

// Asset filenames uploaded to each GitHub Release (must match the `asset`
// values in .github/workflows/release-sidecar.yml, GITHUB_ASSETS in
// packages/core/scripts/postinstall.cjs, and the asset match in build.rs).
const ASSETS = [
  'tsnet-sidecar-darwin-arm64',
  'tsnet-sidecar-darwin-amd64',
  'tsnet-sidecar-linux-amd64',
  'tsnet-sidecar-linux-arm64',
  'tsnet-sidecar-windows-amd64.exe',
];

const [, , version, assetsDir] = process.argv;
if (!version || !assetsDir) {
  console.error('Usage: node scripts/update-sidecar-checksums.mjs <version> <assets-dir>');
  process.exit(2);
}

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..');
const TARGETS = [
  join(repoRoot, 'packages/core/sidecar-checksums.json'),
  join(repoRoot, 'crates/truffle-sidecar/sidecar-checksums.json'),
];

// Compute checksums — all five assets are required so a partial release can't
// silently ship a half-populated trust anchor.
const entry = {};
for (const asset of ASSETS) {
  const p = join(assetsDir, asset);
  if (!existsSync(p)) {
    console.error(`[checksums] missing asset: ${p}`);
    process.exit(1);
  }
  entry[asset] = createHash('sha256').update(readFileSync(p)).digest('hex');
}

for (const target of TARGETS) {
  const json = JSON.parse(readFileSync(target, 'utf8'));
  json[version] = entry;
  writeFileSync(target, `${JSON.stringify(json, null, 2)}\n`);
  console.log(`[checksums] wrote ${version} into ${target}`);
}
