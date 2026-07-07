// Unit tests for the node:dgram-shaped mesh UDP wrapper (RFC 021 Phase 3).
//
// TruffleDgramSocket is tested against mocked native handles — no tailnet,
// no native addon calls. Run: pnpm --filter @vibecook/truffle test

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { once } = require('node:events');

async function loadDgram() {
  return import('../dist/dgram.js');
}

/**
 * A mock NapiUdpSocket that serves `datagrams` in order, records sends, and
 * models the native recv contract: once the queue drains it either returns
 * `null` (`endOnExhaust` — a relay-closed socket) or parks until `close()`
 * resolves the pending recv with `null`.
 */
function mockUdpSocket({ datagrams = [], boundPort = 0, endOnExhaust = false } = {}) {
  const state = { sends: [], closed: false };
  let recvIndex = 0;
  let parked = [];
  const native = {
    async send(data, host, port) {
      state.sends.push({ data: Buffer.from(data), host, port });
      return data.length;
    },
    async recv() {
      if (recvIndex < datagrams.length) return datagrams[recvIndex++];
      if (endOnExhaust || state.closed) return null;
      return new Promise((resolve) => parked.push(resolve));
    },
    port() {
      return boundPort;
    },
    close() {
      state.closed = true;
      for (const resolve of parked) resolve(null);
      parked = [];
    },
  };
  return { native, state };
}

/** A mock NapiNode exposing just what the dgram wrapper touches. */
function mockNode({ udp, peers = [] } = {}) {
  const state = { getPeersCalls: 0, bindPort: undefined };
  const node = {
    async bindUdp(port) {
      state.bindPort = port;
      return udp;
    },
    async getPeers() {
      state.getPeersCalls += 1;
      return peers;
    },
  };
  return { node, state };
}

test('TruffleDgramSocket delivers datagrams with rinfo fields', async () => {
  const { createDgramNamespace } = await loadDgram();
  const { native } = mockUdpSocket({
    datagrams: [{ data: Buffer.from('hello'), address: '100.64.0.5', port: 9000 }],
    boundPort: 8081,
    endOnExhaust: true,
  });
  const { node } = mockNode({ udp: native });

  const sock = createDgramNamespace(node).createSocket();
  const messages = [];
  sock.on('message', (msg, rinfo) => messages.push({ msg, rinfo }));
  // Attach the 'close' waiter before bind: the pump can drain and emit
  // 'close' during bind's await, and a listener added after would miss it.
  const closed = once(sock, 'close');

  await sock.bind(8081);
  await closed; // endOnExhaust → pump drains then ends

  assert.equal(messages.length, 1);
  assert.equal(messages[0].msg.toString(), 'hello');
  assert.deepEqual(messages[0].rinfo, {
    address: '100.64.0.5',
    port: 9000,
    family: 'IPv4',
    size: 5,
    peerId: undefined,
    peerName: undefined,
  });
});

test('TruffleDgramSocket emits listening and honours the bind callback', async () => {
  const { createDgramNamespace } = await loadDgram();
  const { native } = mockUdpSocket({ boundPort: 5000 });
  const { node, state } = mockNode({ udp: native });

  const sock = createDgramNamespace(node).createSocket();
  let cbFired = false;
  const bindResult = sock.bind(5000, () => {
    cbFired = true;
  });

  await once(sock, 'listening');
  assert.equal(await bindResult, sock); // bind resolves with the socket
  assert.equal(cbFired, true);
  assert.equal(state.bindPort, 5000);
  assert.equal(sock.port, 5000);
  assert.deepEqual(sock.address(), { address: '', family: 'IPv4', port: 5000 });

  sock.close();
  await once(sock, 'close');
});

test('TruffleDgramSocket ends and emits close when recv returns null', async () => {
  const { createDgramNamespace } = await loadDgram();
  const { native, state } = mockUdpSocket({ datagrams: [], endOnExhaust: true });
  const { node } = mockNode({ udp: native });

  const sock = createDgramNamespace(node).createSocket();
  // The pump emits 'close' during bind's await here (recv resolves null with
  // no datagrams to process), so the waiter must be attached beforehand.
  const closed = once(sock, 'close');
  await sock.bind(0);
  await closed;
  // Pump ended on the relay-closed path, without us calling native.close().
  assert.equal(state.closed, false);
});

test('TruffleDgramSocket send reaches native with (data, host, port)', async () => {
  const { createDgramNamespace } = await loadDgram();
  const { native, state } = mockUdpSocket({ boundPort: 7000 });
  const { node } = mockNode({ udp: native });

  const sock = createDgramNamespace(node).createSocket();
  await sock.bind(7000);

  // Callback resolves on success (node:dgram contract), Buffer payload.
  await new Promise((resolve, reject) => {
    sock.send(Buffer.from('ping'), 8081, 'other-machine', (err) => (err ? reject(err) : resolve()));
  });
  // String payloads are coerced to UTF-8 Buffers.
  sock.send('pong', 9090, 'kitchen-pi');

  assert.equal(state.sends.length, 2);
  assert.equal(state.sends[0].data.toString(), 'ping');
  assert.equal(state.sends[0].host, 'other-machine');
  assert.equal(state.sends[0].port, 8081);
  assert.equal(state.sends[1].data.toString(), 'pong');
  assert.equal(state.sends[1].host, 'kitchen-pi');
  assert.equal(state.sends[1].port, 9090);

  sock.close();
  await once(sock, 'close');
});

test('TruffleDgramSocket send before bind throws', async () => {
  const { createDgramNamespace } = await loadDgram();
  const { native } = mockUdpSocket();
  const { node } = mockNode({ udp: native });

  const sock = createDgramNamespace(node).createSocket();
  assert.throws(() => sock.send(Buffer.from('x'), 1, 'peer'), /bind\(\) before send/);
});

test('TruffleDgramSocket close() is idempotent and fires the callback even post-close', async () => {
  const { createDgramNamespace } = await loadDgram();
  const { native, state } = mockUdpSocket({ datagrams: [] }); // recv parks until close
  const { node } = mockNode({ udp: native });

  const sock = createDgramNamespace(node).createSocket();
  await sock.bind(0);

  let closeCount = 0;
  sock.on('close', () => {
    closeCount += 1;
  });

  await new Promise((resolve) => {
    sock.close(resolve); // callback runs on 'close'
    sock.close(); // idempotent: no throw, no second teardown
  });
  assert.equal(closeCount, 1);
  assert.equal(state.closed, true);

  // close() after the socket already closed still invokes the callback...
  await new Promise((resolve) => sock.close(resolve));
  assert.equal(closeCount, 1); // ...but does not re-emit 'close'
});

test('TruffleDgramSocket enriches rinfo from peers and rate-limits getPeers', async () => {
  const { createDgramNamespace } = await loadDgram();
  const { native } = mockUdpSocket({
    datagrams: [
      { data: Buffer.from('a'), address: '100.64.0.5', port: 1111 },
      { data: Buffer.from('b'), address: '100.64.0.99', port: 2222 },
    ],
    endOnExhaust: true,
  });
  const { node, state } = mockNode({
    udp: native,
    peers: [{ deviceId: 'DEV-A', deviceName: 'Machine A', ip: '100.64.0.5' }],
  });

  const sock = createDgramNamespace(node).createSocket();
  const rinfos = [];
  sock.on('message', (_msg, rinfo) => rinfos.push(rinfo));
  const closed = once(sock, 'close');
  await sock.bind(0);
  await closed;

  assert.equal(rinfos.length, 2);
  // Known source IP → enriched from the peer list (cold cache → 1 getPeers).
  assert.equal(rinfos[0].peerId, 'DEV-A');
  assert.equal(rinfos[0].peerName, 'Machine A');
  // Unknown source IP → undefined, and no extra getPeers within the window.
  assert.equal(rinfos[1].peerId, undefined);
  assert.equal(rinfos[1].peerName, undefined);
  assert.equal(state.getPeersCalls, 1);
});

test('TruffleDgramSocket surfaces a recv error then closes', async () => {
  const { createDgramNamespace } = await loadDgram();
  const native = {
    async recv() {
      throw new Error('relay gone');
    },
    async send() {
      return 0;
    },
    port: () => 0,
    close() {},
  };
  const node = {
    async bindUdp() {
      return native;
    },
    async getPeers() {
      return [];
    },
  };

  const sock = createDgramNamespace(node).createSocket();
  // 'close' follows 'error' within the same tick; register a plain 'close'
  // listener before yielding, since events.once() rejects on 'error' by
  // design (the footgun documented in net.test.cjs).
  const errorEvent = once(sock, 'error');
  const closeEvent = new Promise((resolve) => sock.once('close', resolve));
  await sock.bind(0);

  const [err] = await errorEvent;
  assert.match(err.message, /relay gone/);
  await closeEvent;
});

test('TruffleDgramSocket bind(port, cb) failure emits error, not an unhandled rejection', async () => {
  const { createDgramNamespace } = await loadDgram();
  const node = {
    async bindUdp() {
      throw new Error('relay bind failed');
    },
    async getPeers() {
      return [];
    },
  };

  const sock = createDgramNamespace(node).createSocket();
  const errorEvent = once(sock, 'error');
  let listeningFired = false;
  // Fire-and-forget callback style (the node:dgram idiom) — failure must
  // surface as 'error', and the returned promise must not reject unhandled.
  sock.bind(4444, () => {
    listeningFired = true;
  });

  const [err] = await errorEvent;
  assert.match(err.message, /relay bind failed/);
  assert.equal(listeningFired, false);
});
