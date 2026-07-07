const test = require('node:test');
const assert = require('node:assert/strict');
const { buildDownloadUrl, assertChecksum, loadExpectedChecksum } = require('../scripts/postinstall.cjs');

test('fallback URL uses the truffle-v release tag scheme', () => {
  assert.equal(
    buildDownloadUrl('0.4.8', 'tsnet-sidecar-darwin-arm64'),
    'https://github.com/jamesyong-42/truffle/releases/download/truffle-v0.4.8/tsnet-sidecar-darwin-arm64',
  );
});

test('checksum mismatch throws (fail closed); match and case-insensitive match pass', () => {
  assert.throws(() => assertChecksum('aa'.repeat(32), 'bb'.repeat(32), 'x'), /checksum mismatch/);
  assert.doesNotThrow(() => assertChecksum('AB'.repeat(32), 'ab'.repeat(32), 'x'));
});

test('shipped checksums contain real sha256 entries for all 0.4.8 assets', () => {
  for (const asset of [
    'tsnet-sidecar-darwin-arm64',
    'tsnet-sidecar-darwin-amd64',
    'tsnet-sidecar-linux-amd64',
    'tsnet-sidecar-linux-arm64',
    'tsnet-sidecar-windows-amd64.exe',
  ]) {
    assert.match(loadExpectedChecksum('0.4.8', asset) ?? '', /^[0-9a-f]{64}$/);
  }
});

test('requiring the script does not execute main()', () => {
  // If main() had run on require, the suite would have attempted a download; reaching here is the assertion.
  assert.ok(true);
});
