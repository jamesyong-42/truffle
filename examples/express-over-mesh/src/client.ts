// Client for the Express-over-mesh example (RFC 021 Phase 2).
//
// Talks to another device's Express server (src/server.ts) over the mesh,
// two ways: the high-level `mesh.http.fetchText` helper, and a raw node:http
// request routed through the mesh agent (`mesh.http.get`).
//
//   tsx src/client.ts <peer> [port]
//
// <peer> is any peer reference: a device name, a device id (or ≥4-char
// prefix), a Tailscale hostname, or a Tailscale IP (100.x).

import { createMeshNode } from '@vibecook/truffle';

const peer = process.argv[2];
if (!peer) {
  console.error('Usage: tsx src/client.ts <peer-name-or-id-or-ip> [port]');
  process.exit(1);
}
const port = Number(process.argv[3] ?? '8080');

const mesh = await createMeshNode({
  appId: 'express-over-mesh',
  authKey: process.env.TS_AUTHKEY,
});

try {
  // (a) High-level: fetchText buffers the body for you and takes ANY peer
  //     reference — including a device name that contains spaces.
  const status = await mesh.http.fetchText(peer, port, '/api/status');
  console.log(`[fetchText] GET /api/status -> ${status.status}`);
  console.log(status.body);

  // (b) Raw node:http through the mesh agent. Here the peer goes in the URL
  //     hostname, so it needs to be a valid URL host: a Tailscale IP or a
  //     name without spaces. (Use fetchText / the options form otherwise.)
  await new Promise<void>((resolve, reject) => {
    const req = mesh.http.get(`http://${peer}:${port}/api/status`, (res) => {
      const chunks: Buffer[] = [];
      res.on('data', (chunk: Buffer) => chunks.push(chunk));
      res.on('end', () => {
        console.log(`\n[raw http.get] GET /api/status -> ${res.statusCode}`);
        console.log(Buffer.concat(chunks).toString('utf8'));
        resolve();
      });
      res.on('error', reject);
    });
    req.on('error', reject);
  });

  // (c) Ask the server who it thinks WE are. On the other side, createServer
  //     attaches our verified Tailscale identity to req.socket — no auth
  //     token, nothing we sent; it comes from the WireGuard tunnel.
  const whoami = await mesh.http.fetchText(peer, port, '/api/whoami');
  console.log(`\n[fetchText] GET /api/whoami -> ${whoami.status}`);
  console.log(whoami.body);

  // (d) And the peer list the server sees, for good measure.
  const peers = await mesh.http.fetchText(peer, port, '/api/peers');
  console.log(`\n[fetchText] GET /api/peers -> ${peers.status}`);
  console.log(peers.body);
} catch (err) {
  console.error('Request failed:', err instanceof Error ? err.message : err);
  process.exitCode = 1;
} finally {
  await mesh.stop();
}
