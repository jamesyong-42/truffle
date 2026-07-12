// Serve a single-page app AND its API on one tailnet port (RFC 023 §6.2).
//
// `mesh.serve({ routes })` is the mixed-route shape: several path prefixes on
// one listener, matched by longest prefix. Here `/` is a static directory (the
// SPA) and `/api` is reverse-proxied to a local backend — the classic
// SPA+API deployment, published to the whole tailnet with no handler code and
// no CORS (it's all one origin). The Go sidecar serves the files and proxies
// the API; bytes never cross into JS. TLS is on by default, so a phone browser
// gets a real https:// URL with a padlock.
//
//   tsx src/main.ts
//
// Then open the printed URL on any device on your tailnet.

import http from 'node:http';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { createMeshNode } from '@vibecook/truffle';

const publicDir = join(dirname(fileURLToPath(import.meta.url)), '..', 'public');
const API_PORT = 8000;

// A tiny local API for the SPA to call. Any localhost backend works — a real
// app would point `/api` at its own server; this keeps the example runnable.
const api = http.createServer((req, res) => {
  res.writeHead(200, { 'content-type': 'application/json' });
  res.end(JSON.stringify({ message: 'hello from the /api backend', path: req.url }));
});
await new Promise<void>((resolve) => api.listen(API_PORT, '127.0.0.1', resolve));

const mesh = await createMeshNode({
  appId: 'serve-static-spa',
  // deviceName defaults to the OS hostname; override it for a prettier URL.
  deviceName: process.env.DEVICE_NAME,
  // Optional: a Tailscale auth key for headless/CI runs (skips interactive login).
  authKey: process.env.TS_AUTHKEY,
});

// One port, two routes (longest prefix wins, so `/api/*` beats `/`):
//   /api  → the local backend (reverse-proxied)
//   /     → the static SPA, with `fallback` so deep links survive a refresh
const site = await mesh.serve({
  port: 443,
  routes: {
    '/api': `http://localhost:${API_PORT}`,
    '/': { dir: publicDir, fallback: '/index.html' },
  },
});

console.log('SPA + API are live on the tailnet:');
console.log(`  ${site.url}`);
console.log('Open it from any device on your tailnet — phone browsers included.');
console.log('Press Ctrl-C to stop serving.');

// Sidecar-side runtime errors (missing cert, backend refused, …) surface here.
// 'serveError' is always safe to leave unlistened; we log it for visibility.
site.on('serveError', ({ code, message }) => {
  console.error(`serve error [${code}]: ${message}`);
});

process.on('SIGINT', async () => {
  console.log('\nStopping...');
  await site.close(); // stop serving, release the tailnet port
  await mesh.stop();
  api.close();
  process.exit(0);
});
