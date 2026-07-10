/**
 * Netcat Example - netcat-over-the-mesh
 *
 * Raw TCP over the tailnet with zero servers: pipes stdin/stdout through a
 * `mesh.net` socket (RFC 021 §8), the same trick BSD/GNU `nc` plays with a
 * plain TCP socket. Demonstrates `mesh.net.createServer` / `mesh.net.connect`
 * end to end, including half-close semantics for one-shot transfers.
 *
 * Prerequisites:
 *   - Tailscale installed and running on both devices
 *   - npm install @vibecook/truffle
 *   - Both devices run with the SAME appId ('netcat-demo', below) — appId
 *     scopes peer visibility, so mismatched apps can't see each other.
 *
 * Usage:
 *   npx tsx src/main.ts listen <port>
 *   npx tsx src/main.ts connect <peer> <port>
 *   npx tsx src/main.ts peers
 *
 * See README.md for the two-device walkthrough.
 */

import { createMeshNode, type MeshNode } from '@vibecook/truffle';

const APP_ID = 'netcat-demo';

async function startMesh(): Promise<MeshNode> {
  return createMeshNode({
    appId: APP_ID,
    onAuthRequired: (url) => {
      console.error(`Tailscale auth required — visit: ${url}`);
    },
  });
}

/**
 * One shared exit path per invocation: stop the mesh (best-effort) and
 * exit with `code`. Every event that can end the run — socket 'end'/'close',
 * a socket/server 'error', or SIGINT — calls the *same* exiter, so whichever
 * fires first wins and later callbacks are no-ops (no double `mesh.stop()`,
 * no exit-code clobbering from a second caller).
 */
function makeExiter(mesh: MeshNode): (code: number) => void {
  let done = false;
  return (code: number) => {
    if (done) return;
    done = true;
    mesh
      .stop()
      .catch(() => {})
      .finally(() => process.exit(code));
  };
}

function parsePort(raw: string | undefined): number {
  const port = Number(raw);
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    console.error(`netcat: invalid port: ${raw === undefined ? '(missing)' : JSON.stringify(raw)}`);
    process.exit(1);
  }
  return port;
}

function usage(): void {
  console.error('Usage:');
  console.error('  netcat listen <port>');
  console.error('  netcat connect <peer> <port>');
  console.error('  netcat peers');
}

async function runListen(portArg: string | undefined): Promise<void> {
  const port = parsePort(portArg);
  const mesh = await startMesh();
  const exit = makeExiter(mesh);
  process.on('SIGINT', () => exit(0));

  const server = mesh.net.createServer((socket) => {
    console.error(`connected: ${socket.remotePeerName ?? socket.remoteAddress}`);

    // process.stdin.pipe() ends the socket's write side (FIN) on stdin EOF,
    // but the socket is `allowHalfOpen` — the read side (piped to stdout)
    // keeps delivering data until the peer's own FIN arrives. That's what
    // makes `netcat listen 9000 > out.png` wait for the whole transfer
    // instead of exiting the moment our own stdin (usually a TTY) is idle.
    socket.pipe(process.stdout);
    process.stdin.pipe(socket);

    socket.on('end', () => exit(0));
    socket.on('close', () => exit(0));
    socket.on('error', (err: Error) => {
      console.error(`netcat: connection error: ${err.message}`);
      exit(1);
    });
  });

  server.on('error', (err: Error) => {
    console.error(`netcat: listen on port ${port} failed: ${err.message}`);
    exit(1);
  });

  server.listen(port, () => {
    console.error(`listening on port ${server.port}, waiting for a connection...`);
  });
}

async function runConnect(peer: string | undefined, portArg: string | undefined): Promise<void> {
  if (!peer) {
    usage();
    process.exit(1);
  }
  const port = parsePort(portArg);
  const mesh = await startMesh();
  const exit = makeExiter(mesh);
  process.on('SIGINT', () => exit(0));

  const socket = mesh.net.connect({ host: peer, port });

  socket.on('connect', () => {
    console.error(`connected: ${peer}:${port} (${socket.remoteAddress})`);
  });
  socket.on('error', (err: Error) => {
    // Covers both peer-resolution failures ("peer not found") and dial
    // failures ("connection refused") — both surface as a rejected
    // `openTcp()`, which `TruffleSocket` turns into this 'error' event.
    console.error(`netcat: connect to ${peer}:${port} failed: ${err.message}`);
    exit(1);
  });

  socket.pipe(process.stdout);
  process.stdin.pipe(socket);

  socket.on('end', () => exit(0));
  socket.on('close', () => exit(0));
}

async function runPeers(): Promise<void> {
  const mesh = await startMesh();
  try {
    const peers = await mesh.getPeers();
    const row = (a: string, b: string, c: string, d: string): string =>
      `${a.padEnd(24)}${b.padEnd(28)}${c.padEnd(16)}${d}`;
    console.log(row('name', 'deviceId', 'ip', 'online'));
    for (const peer of peers) {
      // displayName is always present; deviceId is the durable ULID — null
      // until the peer's hello is seen (RFC 022).
      console.log(
        row(peer.displayName, peer.deviceId ?? '(pending)', peer.ip, peer.online ? 'yes' : 'no'),
      );
    }
  } finally {
    await mesh.stop().catch(() => {});
  }
  process.exit(0);
}

async function main(): Promise<void> {
  const [command, a, b] = process.argv.slice(2);
  switch (command) {
    case 'listen':
      await runListen(a);
      break;
    case 'connect':
      await runConnect(a, b);
      break;
    case 'peers':
      await runPeers();
      break;
    default:
      usage();
      process.exit(command ? 1 : 0);
  }
}

main().catch((err: unknown) => {
  console.error(`netcat: ${err instanceof Error ? err.message : String(err)}`);
  process.exit(1);
});
