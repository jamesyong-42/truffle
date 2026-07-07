// Unit tests for the node:net-shaped mesh TCP wrappers (RFC 021 Phase 2).
//
// TruffleSocket/TruffleServer are tested against mocked native handles —
// no tailnet, no native addon calls. Run: pnpm --filter @vibecook/truffle test

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { once } = require('node:events');

async function loadNet() {
  return import('../dist/net.js');
}

/** A mock NapiTcpSocket serving `chunks` then EOF, recording interactions. */
function mockNativeSocket(chunks = []) {
  const state = {
    writes: [],
    ended: false,
    closed: false,
    concurrentReads: 0,
    maxConcurrentReads: 0,
  };
  let readIndex = 0;
  const native = {
    async read(_maxBytes) {
      state.concurrentReads += 1;
      state.maxConcurrentReads = Math.max(state.maxConcurrentReads, state.concurrentReads);
      await new Promise((resolve) => setTimeout(resolve, 1));
      state.concurrentReads -= 1;
      if (readIndex >= chunks.length) return null;
      const chunk = chunks[readIndex];
      readIndex += 1;
      return chunk;
    },
    async write(data) {
      state.writes.push(Buffer.from(data));
    },
    async end() {
      state.ended = true;
    },
    async close() {
      state.closed = true;
    },
    remoteAddress: () => 'other-machine:9000',
    remotePeerId: () => 'PEER-ID-01',
    remotePeerName: () => 'Other Machine',
  };
  return { native, state };
}

test('TruffleSocket delivers data then end, with metadata on connect', async () => {
  const { TruffleSocket } = await loadNet();
  const { native } = mockNativeSocket([Buffer.from('hello '), Buffer.from('mesh')]);

  const socket = new TruffleSocket(native);
  // Inbound (non-promise) sockets expose peer metadata synchronously so a
  // 'connection' handler can gate/log at accept time (RFC 021 §8/§9).
  assert.equal(socket.remoteAddress, 'other-machine:9000');
  assert.equal(socket.remotePeerId, 'PEER-ID-01');
  assert.equal(socket.remotePeerName, 'Other Machine');
  await once(socket, 'connect');

  const received = [];
  socket.on('data', (chunk) => received.push(chunk));
  await once(socket, 'end');
  assert.equal(Buffer.concat(received).toString(), 'hello mesh');
});

test('TruffleSocket keeps reads single-inflight (pull-model backpressure)', async () => {
  const { TruffleSocket } = await loadNet();
  const chunks = Array.from({ length: 8 }, (_, i) => Buffer.from(`chunk-${i}`));
  const { native, state } = mockNativeSocket(chunks);

  const socket = new TruffleSocket(native);
  socket.resume();
  await once(socket, 'end');
  assert.equal(state.maxConcurrentReads, 1);
});

test('TruffleSocket write path reaches the native handle; end() half-closes', async () => {
  const { TruffleSocket } = await loadNet();
  const { native, state } = mockNativeSocket([]);

  const socket = new TruffleSocket(native);
  socket.resume(); // drain reads so 'end' can fire
  socket.write('ping ');
  socket.write('pong');
  socket.end();
  await once(socket, 'finish');

  assert.equal(Buffer.concat(state.writes).toString(), 'ping pong');
  assert.equal(state.ended, true);
  assert.equal(state.closed, false); // half-close, not teardown
});

test('TruffleSocket stays writable after peer EOF (allowHalfOpen)', async () => {
  const { TruffleSocket } = await loadNet();
  const { native, state } = mockNativeSocket([Buffer.from('bye')]);

  const socket = new TruffleSocket(native);
  socket.resume();
  await once(socket, 'end'); // peer finished writing
  socket.write('still here');
  socket.end();
  await once(socket, 'finish');
  assert.equal(Buffer.concat(state.writes).toString(), 'still here');
});

test('TruffleSocket destroy() closes the native socket', async () => {
  const { TruffleSocket } = await loadNet();
  const { native, state } = mockNativeSocket([]);

  const socket = new TruffleSocket(native);
  await once(socket, 'connect');
  socket.destroy();
  await once(socket, 'close');
  assert.equal(state.closed, true);
});

test('TruffleSocket surfaces a failed dial as error + close', async () => {
  const { TruffleSocket } = await loadNet();
  const socket = new TruffleSocket(Promise.reject(new Error('peer not found: nope')));

  // Attach both waiters before yielding — 'close' follows 'error' within
  // the same tick, so a late listener would miss it. The close waiter is a
  // plain listener because events.once() rejects on 'error' by design.
  const errorEvent = once(socket, 'error');
  const closeEvent = new Promise((resolve) => socket.once('close', resolve));
  const [err] = await errorEvent;
  assert.match(err.message, /peer not found/);
  await closeEvent;
});

test('TruffleServer accepts connections and closes cleanly', async () => {
  const { TruffleServer } = await loadNet();
  const { native } = mockNativeSocket([]);

  let acceptCalls = 0;
  let listenerClosed = false;
  const mockListener = {
    async accept() {
      acceptCalls += 1;
      if (acceptCalls === 1) return native;
      // Block until closed, then signal listener shutdown.
      await new Promise((resolve) => {
        const timer = setInterval(() => {
          if (listenerClosed) {
            clearInterval(timer);
            resolve();
          }
        }, 1);
      });
      return null;
    },
    port: () => 4242,
    async close() {
      listenerClosed = true;
    },
  };
  const mockNode = {
    async listenTcp(port) {
      assert.equal(port, 0);
      return mockListener;
    },
  };

  const server = new TruffleServer(mockNode);
  const connections = [];
  server.on('connection', (socket) => connections.push(socket));
  server.listen(0);

  await once(server, 'listening');
  assert.equal(server.port, 4242);
  assert.deepEqual(server.address(), { port: 4242 });

  // Wait for the accepted connection to surface.
  while (connections.length === 0) {
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  assert.equal(connections.length, 1);

  server.close();
  await once(server, 'close');
});

test('createNetNamespace connect() validates host and supports (port, host)', async () => {
  const { createNetNamespace } = await loadNet();
  const { native } = mockNativeSocket([]);
  const calls = [];
  const mockNode = {
    async openTcp(host, port) {
      calls.push([host, port]);
      return native;
    },
  };

  const net = createNetNamespace(mockNode);
  assert.throws(() => net.connect({ host: '', port: 1 }), /host .* required/);

  const socket = net.connect(9000, 'other-machine');
  await once(socket, 'connect');
  assert.deepEqual(calls, [['other-machine', 9000]]);
  socket.destroy();
  await once(socket, 'close');
});
