// Express, served over the mesh (RFC 023 §6.1 — mesh.http.createServer).
//
// This is a perfectly ordinary Express app. The only mesh-specific line is how
// it starts: instead of `app.listen(port)` on a host TCP port, we hand the app
// to `mesh.http.createServer` and call `.listen(port)` on that. It returns a
// real node:http.Server whose listener is the mesh, so every request arrives
// over the tailnet — WireGuard-encrypted, reachable by any Tailscale device
// (browsers included), and carrying the caller's verified identity on
// `req.socket` (see the /api/whoami route).
//
// Run this on one device, then run src/client.ts on another (see README.md).

import express from 'express';
import { createMeshNode, type TruffleSocket } from '@vibecook/truffle';

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

// Identity on the request: req.socket is a mesh TruffleSocket carrying the
// caller's verified Tailscale WhoIs identity (spoof-proof — it comes from the
// WireGuard tunnel, not a client header). Frameworks type it as a net.Socket,
// so cast to read the mesh fields.
app.get('/api/whoami', (req, res) => {
  const sock = req.socket as unknown as TruffleSocket;
  res.json({
    // remotePeer is the RFC 022 handle (null until the caller is interned);
    // remotePeerName is the WhoIs display name, always set for a tailnet caller.
    youAre: sock.remotePeer?.displayName ?? sock.remotePeerName ?? null,
    yourDeviceId: sock.remotePeer?.deviceId ?? null,
    seenBy: me.deviceName,
  });
});

app.post('/api/echo', (req, res) => {
  res.json({ echoedBy: me.deviceName, body: req.body });
});

// The one mesh-specific line: serve the Express app over the mesh. createServer
// returns a real http.Server whose listen() binds a mesh listener instead of a
// host TCP port. (The older, more manual form still works too —
// `mesh.net.createServer((s) => httpServer.emit('connection', s)).listen(PORT)`
// — but createServer is the front door and also gives you req.socket identity.)
const server = mesh.http.createServer(app);

server.listen(PORT, () => {
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
