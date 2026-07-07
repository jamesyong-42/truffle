// Unit tests for the WebSocket-over-mesh wrapper (RFC 021 Phase 4).
//
// mesh.ws is exercised end-to-end IN PROCESS: a fake TruffleNet backed by a
// real 127.0.0.1 `net` loopback server stands in for the tailnet, so the whole
// dance runs — `ws` server upgrade, the MeshAgent client, the non-URL-safe
// peer-ref pin — with zero Tailscale and zero native addon. Run:
//   pnpm --filter @vibecook/truffle test

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const net = require('node:net');
const { EventEmitter, once } = require('node:events');

async function loadWs() {
  return import('../dist/ws.js');
}

/**
 * A fake TruffleNet backed by real loopback TCP. `createServer` binds a real
 * `net.Server` on 127.0.0.1 (so ephemeral port 0 resolves to a real port);
 * `connect` dials that server on the given port, ignoring `host` (the tailnet
 * peer reference has no meaning on loopback). This is exactly the seam ws.ts
 * uses: the client MeshAgent calls `connect`, the server calls `createServer`.
 */
function fakeTruffleNet() {
  return {
    createServer(connectionListener) {
      const real = net.createServer();
      const api = new EventEmitter();
      if (connectionListener) api.on('connection', connectionListener);
      api.port = undefined;
      real.on('connection', (sock) => api.emit('connection', sock));
      real.on('error', (err) => api.emit('error', err));
      api.listen = (port, cb) => {
        real.listen(port, '127.0.0.1', () => {
          api.port = real.address().port;
          api.emit('listening');
          if (cb) cb();
        });
        return api;
      };
      api.close = (cb) => {
        real.close(cb);
        api.emit('close');
        return api;
      };
      api.address = () => (api.port == null ? null : { port: api.port });
      return api;
    },
    connect({ port }) {
      return net.connect(port, '127.0.0.1');
    },
  };
}

/** Stand up an echo server that prefixes replies with `echo:`. */
function attachEcho(server) {
  server.on('connection', (client) => {
    client.on('message', (data) => client.send('echo:' + data.toString()));
  });
}

test('mesh.ws round-trips a message over a URL-safe host and closes cleanly', async () => {
  const { createWsNamespace } = await loadWs();
  const ws = createWsNamespace(fakeTruffleNet());

  const server = await ws.createServer({ port: 0 });
  assert.ok(server.port > 0, 'ephemeral port resolved');
  attachEcho(server);

  const client = await ws.connect('127.0.0.1', server.port, '/chat');
  await once(client, 'open');
  const reply = new Promise((resolve) => client.once('message', (d) => resolve(d.toString())));
  client.send('hi');
  assert.equal(await reply, 'echo:hi');

  client.close();
  await once(client, 'close');
  await new Promise((resolve) => server.close(resolve));
});

test('mesh.ws routes a peer ref that is not a URL hostname (spaces) via the pin', async () => {
  const { createWsNamespace } = await loadWs();
  const net = fakeTruffleNet();
  // Record what the agent was asked to dial, to prove the real peer ref — not
  // a placeholder — reaches connect() even though it can't be a URL hostname.
  const dialed = [];
  const origConnect = net.connect.bind(net);
  net.connect = (opts) => {
    dialed.push(opts.host);
    return origConnect(opts);
  };
  const ws = createWsNamespace(net);

  const server = await ws.createServer({ port: 0 });
  attachEcho(server);

  const client = await ws.connect('living room pi', server.port);
  await once(client, 'open');
  const reply = new Promise((resolve) => client.once('message', (d) => resolve(d.toString())));
  client.send('ping');
  assert.equal(await reply, 'echo:ping');
  assert.deepEqual(
    dialed,
    ['living room pi'],
    'agent dialed the exact peer ref, not a placeholder',
  );

  client.close();
  await once(client, 'close');
  await new Promise((resolve) => server.close(resolve));
});

test('mesh.ws createServer honours the path filter', async () => {
  const { createWsNamespace } = await loadWs();
  const ws = createWsNamespace(fakeTruffleNet());

  const server = await ws.createServer({ port: 0, path: '/chat' });
  attachEcho(server);

  // Right path connects; wrong path is rejected before 'open'.
  const good = await ws.connect('127.0.0.1', server.port, '/chat');
  await once(good, 'open');

  const bad = await ws.connect('127.0.0.1', server.port, '/nope');
  const outcome = await new Promise((resolve) => {
    bad.once('open', () => resolve('open'));
    bad.once('error', () => resolve('error'));
  });
  assert.equal(outcome, 'error', 'mismatched path did not upgrade');

  good.close();
  await once(good, 'close');
  await new Promise((resolve) => server.close(resolve));
});

test('mesh.ws throws a clear, actionable error when the ws package is missing', async () => {
  const { createWsNamespace } = await loadWs();
  // Inject a loader that fails like a missing optional dependency would.
  const failing = () => Promise.reject(new Error("Cannot find package 'ws'"));
  const ws = createWsNamespace(fakeTruffleNet(), failing);

  await assert.rejects(() => ws.connect('127.0.0.1', 9000), /mesh\.ws requires the 'ws' package/);
  await assert.rejects(() => ws.createServer({ port: 0 }), /install it with `npm install ws`/);
});
