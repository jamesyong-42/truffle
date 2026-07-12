// Unit tests for mesh.http.createServer (RFC 023 §6.1, Phase 1).
//
// A real node:http.Server is driven end-to-end through mocked native mesh
// sockets — no tailnet, no native addon. The request bytes go in through the
// mock's read() and the parsed HTTP response comes back out through write(),
// so these tests exercise node's actual HTTP parser over TruffleSocket.
// Run: pnpm --filter @vibecook/truffle test

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { once } = require('node:events');

async function loadModules() {
  const [net, http] = await Promise.all([import('../dist/net.js'), import('../dist/http.js')]);
  return { ...net, ...http };
}

/** A mock NapiTcpSocket that serves `chunks` then blocks until end/close. */
function mockNativeSocket(chunks = []) {
  const state = { writes: [], ended: false, closed: false };
  let readIndex = 0;
  let releaseBlockedRead = null;
  const native = {
    async read(_maxBytes) {
      await new Promise((resolve) => setTimeout(resolve, 1));
      if (readIndex < chunks.length) {
        const chunk = chunks[readIndex];
        readIndex += 1;
        return chunk;
      }
      // No more scripted data: hold the read open like a quiet connection
      // (HTTP keep-alive shape) until the server ends/closes the socket.
      if (state.ended || state.closed) return null;
      await new Promise((resolve) => {
        releaseBlockedRead = resolve;
      });
      return null;
    },
    async write(data) {
      state.writes.push(Buffer.from(data));
    },
    async end() {
      state.ended = true;
      if (releaseBlockedRead) releaseBlockedRead();
    },
    async close() {
      state.closed = true;
      if (releaseBlockedRead) releaseBlockedRead();
    },
    remoteAddress: () => '100.64.0.9:51000',
    remotePeerId: () => 'nTAILSCALE01',
    remotePeerName: () => 'Phone',
  };
  return { native, state };
}

/** A mock node whose listener yields the given native sockets, then blocks. */
function mockNode(sockets) {
  const state = { listenCalls: [], listenerClosed: false };
  const queue = [...sockets];
  const listener = {
    async accept() {
      if (queue.length > 0) return queue.shift();
      await new Promise((resolve) => {
        const timer = setInterval(() => {
          if (state.listenerClosed) {
            clearInterval(timer);
            resolve();
          }
        }, 1);
      });
      return null;
    },
    port: () => 4243,
    async close() {
      state.listenerClosed = true;
    },
  };
  const node = {
    async listenTcp(port, tls) {
      state.listenCalls.push([port, tls]);
      return listener;
    },
  };
  return { node, state };
}

const GET = (path) =>
  Buffer.from(`GET ${path} HTTP/1.1\r\nHost: mesh\r\nConnection: close\r\n\r\n`);

async function until(cond) {
  while (!cond()) await new Promise((resolve) => setTimeout(resolve, 2));
}

test('createServer serves a real HTTP request over a mesh socket', async () => {
  const { createNetNamespace, createHttpNamespace } = await loadModules();
  const { native, state } = mockNativeSocket([GET('/hello')]);
  const { node } = mockNode([native]);
  const http = createHttpNamespace(createNetNamespace(node));

  const seen = [];
  const server = http.createServer((req, res) => {
    seen.push({
      url: req.url,
      peerId: req.socket.remotePeerId,
      peerName: req.socket.remotePeerName,
    });
    res.setHeader('content-type', 'text/plain');
    res.end('hi from the mesh');
  });
  server.listen(0);
  await once(server, 'listening');
  assert.equal(server.port, 4243);
  assert.deepEqual(server.address(), { port: 4243 });
  assert.equal(server.listening, true);

  await until(() => state.ended || state.closed);
  const response = Buffer.concat(state.writes).toString();
  assert.match(response, /^HTTP\/1\.1 200 OK\r\n/);
  assert.match(response, /hi from the mesh$/);
  // Identity is available to the handler at request time (RFC 023 §6.1).
  assert.deepEqual(seen, [{ url: '/hello', peerId: 'nTAILSCALE01', peerName: 'Phone' }]);

  server.close();
  await once(server, 'close');
  assert.equal(server.listening, false);
});

test('createServer(options, handler) form and listen(port, cb) callback', async () => {
  const { createNetNamespace, createHttpNamespace } = await loadModules();
  const { node, state } = mockNode([]);
  const http = createHttpNamespace(createNetNamespace(node));

  const server = http.createServer({}, (_req, res) => res.end());
  await new Promise((resolve) => server.listen(8080, resolve));
  assert.deepEqual(state.listenCalls, [[8080, undefined]]);
  await new Promise((resolve) => server.close(resolve));
});

test('tls: true requests a TLS mesh listener and marks sockets encrypted', async () => {
  const { createNetNamespace, createHttpNamespace } = await loadModules();
  const { native, state } = mockNativeSocket([GET('/')]);
  const { node, state: nodeState } = mockNode([native]);
  const http = createHttpNamespace(createNetNamespace(node));

  let sawEncrypted = null;
  const server = http.createServer({ tls: true }, (req, res) => {
    sawEncrypted = req.socket.encrypted;
    res.end('secure');
  });
  server.listen(443);
  await once(server, 'listening');
  assert.deepEqual(nodeState.listenCalls, [[443, true]]);

  await until(() => state.ended || state.closed);
  assert.equal(sawEncrypted, true);
  server.close();
  await once(server, 'close');
});

test('listen rejections surface as server error events', async () => {
  const { createNetNamespace, createHttpNamespace } = await loadModules();
  const node = {
    async listenTcp() {
      throw new Error('port 9417 is reserved');
    },
  };
  const http = createHttpNamespace(createNetNamespace(node));
  const server = http.createServer(() => {});
  const errEvent = once(server, 'error');
  server.listen(9417);
  const [err] = await errEvent;
  assert.match(err.message, /reserved/);
});

test('listen() rejects host options — the mesh has one address', async () => {
  const { createNetNamespace, createHttpNamespace } = await loadModules();
  const { node } = mockNode([]);
  const http = createHttpNamespace(createNetNamespace(node));
  const server = http.createServer(() => {});
  assert.throws(() => server.listen({ port: 80, host: '0.0.0.0' }), /takes no host/);
});

test('close(cb) after close still calls back and emits close once', async () => {
  const { createNetNamespace, createHttpNamespace } = await loadModules();
  const { node } = mockNode([]);
  const http = createHttpNamespace(createNetNamespace(node));
  const server = http.createServer(() => {});
  server.listen(0);
  await once(server, 'listening');

  let closeEvents = 0;
  server.on('close', () => {
    closeEvents += 1;
  });
  await new Promise((resolve) => server.close(resolve));
  await new Promise((resolve) => server.close(resolve));
  assert.equal(closeEvents, 1);
});
