// Serve a static single-page app to the whole tailnet (RFC 023 §6.2).
//
// `mesh.serve({ dir, fallback })` publishes a directory — no handler, no
// Express, no byte-pumping in JS. The Go sidecar serves the files directly
// (index.html at the root, ETag / Range / correct mime types for free), and
// `fallback` rewrites deep-link misses to index.html so a single-page app's
// client-side routes survive a refresh. TLS is on by default, so a phone
// browser gets a real https:// URL with a padlock.
//
//   tsx src/main.ts
//
// Then open the printed URL on any device on your tailnet.

import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { createMeshNode } from '@vibecook/truffle';

const publicDir = join(dirname(fileURLToPath(import.meta.url)), '..', 'public');

const mesh = await createMeshNode({
  appId: 'serve-static-spa',
  // deviceName defaults to the OS hostname; override it for a prettier URL.
  deviceName: process.env.DEVICE_NAME,
  // Optional: a Tailscale auth key for headless/CI runs (skips interactive login).
  authKey: process.env.TS_AUTHKEY,
});

// Publish ./public to the tailnet on port 443 (the bare https:// URL, no port
// suffix). `fallback` makes every unknown path serve index.html — the SPA
// trick, so `…/about` resolves client-side even on a hard refresh.
const site = await mesh.serve({
  port: 443,
  dir: publicDir,
  fallback: '/index.html',
});

console.log('Static site is live on the tailnet:');
console.log(`  ${site.url}`);
console.log('Open it from any device on your tailnet — phone browsers included.');
console.log('Press Ctrl-C to stop serving.');

// Sidecar-side runtime errors (missing cert, etc.) surface here. 'serveError'
// is always safe to leave unlistened; we log it for visibility.
site.on('serveError', ({ code, message }) => {
  console.error(`serve error [${code}]: ${message}`);
});

process.on('SIGINT', async () => {
  console.log('\nStopping...');
  await site.close(); // stop serving, release the tailnet port
  await mesh.stop();
  process.exit(0);
});
