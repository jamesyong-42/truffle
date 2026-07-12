// Expose a local process to the whole tailnet (RFC 023 §6.2).
//
// `mesh.serve({ target })` publishes something already listening on localhost
// — a Vite dev server, Grafana, an internal API, a container — to the tailnet,
// with zero changes to that process. The Go sidecar reverse-proxies it; bytes
// never cross into JS. Targets are loopback-only by default (a LAN target
// would turn this node into a pivot into its network — opt in with
// `allowNonLoopback: true` if you really mean it).
//
// To stay self-contained, this example starts a tiny "dev server" on
// localhost:3000 and publishes THAT. Point `target` at your real one
// (e.g. http://localhost:5173 for Vite) to expose it instead.
//
//   tsx src/main.ts

import http from 'node:http';
import { createMeshNode } from '@vibecook/truffle';

const LOCAL_PORT = 3000;

// Stand-in for "your local dev server". Anything on localhost works — this is
// only here so the example runs on its own. Note it serves a FIXED body:
// reflecting request data (like req.url) into HTML would be an XSS the moment
// this port is published to the tailnet.
const local = http.createServer((_req, res) => {
  res.writeHead(200, { 'content-type': 'text/html' });
  res.end(`<h1>Hello from localhost:${LOCAL_PORT}</h1><p>Swap me for your real dev server.</p>`);
});
await new Promise<void>((resolve) => local.listen(LOCAL_PORT, '127.0.0.1', resolve));
console.log(`Local dev server listening on http://localhost:${LOCAL_PORT}`);

const mesh = await createMeshNode({
  appId: 'expose-dev-server',
  deviceName: process.env.DEVICE_NAME,
  authKey: process.env.TS_AUTHKEY,
});

// Publish the local server to the tailnet on port 443 (the bare https:// URL).
const published = await mesh.serve({
  port: 443,
  target: `http://localhost:${LOCAL_PORT}`,
  // Restrict access to specific tailnet users (Tailscale login-name globs).
  // Absent = the whole tailnet; a non-match gets a 403. Uncomment to gate:
  // allow: ['you@example.com', '*@your-org.com'],
});

console.log('Now reachable from anywhere on your tailnet:');
console.log(`  ${published.url}`);
console.log('Open it on your phone, a teammate’s laptop, or via curl.');
console.log('Press Ctrl-C to stop.');

// Sidecar-side runtime errors (backend refused, bad cert, …) surface here.
published.on('serveError', ({ code, message }) => {
  console.error(`serve error [${code}]: ${message}`);
});

process.on('SIGINT', async () => {
  console.log('\nStopping...');
  await published.close(); // stop serving, release the tailnet port
  await mesh.stop();
  local.close();
  process.exit(0);
});
