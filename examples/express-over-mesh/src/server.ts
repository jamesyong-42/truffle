// Express, served over the mesh (RFC 021 Phase 2).
//
// This is a perfectly ordinary Express app. The only mesh-specific line is
// how connections reach Node's http.Server: instead of binding a host TCP
// port with `httpServer.listen(port)`, we take connections from `mesh.net`
// and feed them in with `httpServer.emit('connection', socket)`. Every
// request then arrives over the tailnet — WireGuard-encrypted and reachable
// only by other devices running this app.
//
// Run this on one device, then run src/client.ts on another (see README.md).

import http from 'node:http';
import express from 'express';
import { createMeshNode } from '@vibecook/truffle';

const PORT = 8080;

const mesh = await createMeshNode({
  appId: 'express-over-mesh',
  // deviceName defaults to the OS hostname; override it for clarity if you like.
  deviceName: process.env.DEVICE_NAME,
  // Optional: a Tailscale auth key for headless/CI runs (skips interactive login).
  authKey: process.env.TS_AUTHKEY,
});

const me = mesh.getLocalInfo();

const app = express();
app.use(express.json());

// Plain JSON routes — none of this knows or cares that it's on a mesh.
app.get('/api/status', (_req, res) => {
  res.json({
    device: me.deviceName,
    deviceId: me.deviceId,
    ip: me.ip ?? null,
    appId: me.appId,
    uptimeSeconds: Math.round(process.uptime()),
    time: new Date().toISOString(),
  });
});

app.get('/api/peers', async (_req, res) => {
  // getPeers() returns live Peer handles (class instances with getters), so
  // project the fields we want to expose — a raw handle JSON-serialises to {}.
  const peers = await mesh.getPeers();
  res.json({
    count: peers.length,
    peers: peers.map((p) => ({
      displayName: p.displayName,
      deviceId: p.deviceId, // durable ULID, or null until the peer's hello is seen
      ip: p.ip,
      os: p.os,
      online: p.online,
    })),
  });
});

app.post('/api/echo', (req, res) => {
  res.json({ echoedBy: me.deviceName, body: req.body });
});

const httpServer = http.createServer(app);

// The one mesh-specific line: serve the Express app over the mesh. Each mesh
// TCP connection becomes an http.Server 'connection' — exactly what a real
// listening socket would emit.
mesh.net
  .createServer((socket) => httpServer.emit('connection', socket))
  .listen(PORT, () => {
    console.log(`Express is live over the mesh on port ${PORT}`);
    console.log(`  device: "${me.deviceName}"   ip: ${me.ip ?? '(pending Tailscale)'}`);
    console.log('From another device running this app, in the repo root:');
    console.log(
      `  pnpm --filter @vibecook/example-express-over-mesh exec tsx src/client.ts "${me.deviceName}"`,
    );
  });

process.on('SIGINT', async () => {
  console.log('\nStopping...');
  await mesh.stop();
  process.exit(0);
});
