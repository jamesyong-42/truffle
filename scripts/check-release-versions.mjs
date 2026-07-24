#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const read = (path) => readFileSync(join(root, path), 'utf8');
const readJson = (path) => JSON.parse(read(path));

const expected = readJson('packages/core/package.json').version;
const errors = [];
const allowMissingChecksums = process.argv.includes('--allow-missing-checksums');

const jsonPackages = [
  'packages/react/package.json',
  'crates/truffle-napi/package.json',
  'crates/truffle-napi/npm/darwin-arm64/package.json',
  'crates/truffle-napi/npm/darwin-x64/package.json',
  'crates/truffle-napi/npm/linux-arm64-gnu/package.json',
  'crates/truffle-napi/npm/linux-x64-gnu/package.json',
  'crates/truffle-napi/npm/win32-x64-msvc/package.json',
  'npm/truffle-sidecar-darwin-arm64/package.json',
  'npm/truffle-sidecar-darwin-x64/package.json',
  'npm/truffle-sidecar-linux-arm64/package.json',
  'npm/truffle-sidecar-linux-x64/package.json',
  'npm/truffle-sidecar-win32-x64/package.json',
];

for (const path of jsonPackages) {
  const actual = readJson(path).version;
  if (actual !== expected) errors.push(`${path}: expected ${expected}, found ${actual}`);
}

const napiLock = readJson('crates/truffle-napi/package-lock.json');
for (const [label, actual] of [
  ['top-level version', napiLock.version],
  ['root package version', napiLock.packages?.['']?.version],
]) {
  if (actual !== expected) {
    errors.push(
      `crates/truffle-napi/package-lock.json ${label}: expected ${expected}, found ${actual ?? 'none'}`,
    );
  }
}

const cargoPackages = [
  'crates/truffle/Cargo.toml',
  'crates/truffle-cli/Cargo.toml',
  'crates/truffle-core/Cargo.toml',
  'crates/truffle-napi/Cargo.toml',
  'crates/truffle-sidecar/Cargo.toml',
];

for (const path of cargoPackages) {
  const actual = read(path).match(/^version = "([^"]+)"/m)?.[1];
  if (actual !== expected) errors.push(`${path}: expected ${expected}, found ${actual ?? 'none'}`);
}

const lock = read('Cargo.lock');
for (const name of ['truffle', 'truffle-cli', 'truffle-core', 'truffle-napi', 'truffle-sidecar']) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const block = lock.match(
    new RegExp(`\\[\\[package\\]\\]\\nname = "${escaped}"\\nversion = "([^"]+)"`),
  );
  const actual = block?.[1];
  if (actual !== expected) {
    errors.push(`Cargo.lock ${name}: expected ${expected}, found ${actual ?? 'none'}`);
  }
}

const assets = [
  'tsnet-sidecar-darwin-arm64',
  'tsnet-sidecar-darwin-amd64',
  'tsnet-sidecar-linux-amd64',
  'tsnet-sidecar-linux-arm64',
  'tsnet-sidecar-windows-amd64.exe',
];
const checksumPaths = [
  'packages/core/sidecar-checksums.json',
  'crates/truffle-sidecar/sidecar-checksums.json',
];
const checksumMaps = checksumPaths.map(readJson);

for (let index = 0; index < checksumMaps.length; index += 1) {
  const entry = checksumMaps[index][expected];
  for (const asset of assets) {
    const digest = entry?.[asset];
    if (!allowMissingChecksums && !/^[0-9a-f]{64}$/.test(digest ?? '')) {
      errors.push(`${checksumPaths[index]}: missing valid ${expected}/${asset} checksum`);
    }
  }
}

for (const asset of assets) {
  const jsDigest = checksumMaps[0][expected]?.[asset];
  const rustDigest = checksumMaps[1][expected]?.[asset];
  if ((jsDigest || rustDigest) && jsDigest !== rustDigest) {
    errors.push(`sidecar checksum maps disagree for ${expected}/${asset}`);
  }
}

const ref = process.env.GITHUB_REF_NAME;
if (ref?.startsWith('truffle-v') && ref !== `truffle-v${expected}`) {
  errors.push(`release tag ${ref} does not match package version ${expected}`);
}

if (errors.length > 0) {
  console.error('[release:verify] version/checksum consistency failed:');
  for (const error of errors) console.error(`  - ${error}`);
  process.exit(1);
}

const checksumStatus = allowMissingChecksums ? 'checksum maps agree when present' : 'checksums pinned';
console.log(`[release:verify] all publishable artifacts are version ${expected}; ${checksumStatus}`);
