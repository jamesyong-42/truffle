/**
 * WebSocket chat over the mesh (RFC 021 §8, decision D3).
 *
 * `mesh.ws` wraps the battle-tested `ws` package over mesh TCP — no new
 * protocol code. The server broadcasts every message it receives to all
 * connected clients; each client sends its stdin lines and prints whatever the
 * server relays. It's a group chat between tailnet devices with no public
 * server and no port forwarding.
 *
 * Prerequisites:
 *   - Tailscale installed and running on every device
 *   - npm install @vibecook/truffle ws   (`ws` is an optional peer dependency)
 *   - Every device runs with the SAME appId ('ws-chat-demo', below) — appId
 *     scopes peer visibility, so mismatched apps can't see each other.
 *
 * Usage:
 *   npx tsx src/main.ts serve <port>
 *   npx tsx src/main.ts connect <peer> <port>
 *   npx tsx src/main.ts peers
 *
 * See README.md for the multi-device walkthrough.
 */

import { createInterface } from 'node:readline';
import type { IncomingMessage } from 'node:http';
import { createMeshNode, type MeshNode } from '@vibecook/truffle';

const APP_ID = 'ws-chat-demo';

async function startMesh(): Promise<MeshNode> {
  return createMeshNode({
    appId: APP_ID,
    onAuthRequired: (url) => {
      console.error(`Tailscale auth required — visit: ${url}`);
    },
  });
}

/**
 * One shared exit path per invocation: stop the mesh (best-effort) and exit
 * with `code`. Whichever event ends the run — a socket close/error or SIGINT —
 * calls the same exiter, so later callbacks are no-ops (no double stop()).
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
    console.error(`chat: invalid port: ${raw === undefined ? '(missing)' : JSON.stringify(raw)}`);
    process.exit(1);
  }
  return port;
}

function usage(): void {
  console.error('Usage:');
  console.error('  chat serve <port>');
  console.error('  chat connect <peer> <port>');
  console.error('  chat peers');
}

/** The best label we have for an inbound client: its mesh peer name, else IP. */
function clientLabel(req: IncomingMessage): string {
  // The upgraded socket is a mesh TruffleSocket, which carries WhoIs identity
  // on inbound connections (RFC 021 §6.4). Fall back to the tailnet address.
  const sock = req.socket as { remotePeerName?: string; remoteAddress?: string };
  return sock.remotePeerName ?? sock.remoteAddress ?? 'someone';
}

async function runServe(portArg: string | undefined): Promise<void> {
  const port = parsePort(portArg);
  const mesh = await startMesh();
  const exit = makeExiter(mesh);
  process.on('SIGINT', () => exit(0));

  const server = await mesh.ws.createServer({ port });

  /** Relay `text` to every open client. */
  const broadcast = (text: string): void => {
    for (const client of server.clients) {
      if (client.readyState === client.OPEN) client.send(text);
    }
  };

  server.on('connection', (client, req) => {
    const who = clientLabel(req);
    console.error(`joined: ${who}`);
    broadcast(`* ${who} joined`);

    client.on('message', (data: Buffer) => broadcast(`${who}: ${data.toString()}`));
    client.on('close', () => {
      console.error(`left: ${who}`);
      broadcast(`* ${who} left`);
    });
    // A single client's transport error shouldn't take down the server.
    client.on('error', (err: Error) => console.error(`chat: client ${who} error: ${err.message}`));
  });

  server.on('error', (err: Error) => {
    console.error(`chat: server error: ${err.message}`);
    exit(1);
  });

  console.error(`chat server live on mesh port ${server.port} — waiting for clients...`);
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

  const socket = await mesh.ws.connect(peer, port);

  socket.on('open', () =>
    console.error(`connected to ${peer}:${port} — type to chat, Ctrl-D to quit`),
  );
  socket.on('message', (data: Buffer) => console.log(data.toString()));
  socket.on('close', () => exit(0));
  socket.on('error', (err: Error) => {
    console.error(`chat: connect to ${peer}:${port} failed: ${err.message}`);
    exit(1);
  });

  const rl = createInterface({ input: process.stdin });
  rl.on('line', (line) => {
    if (socket.readyState === socket.OPEN) socket.send(line);
  });
  rl.on('close', () => socket.close()); // stdin EOF → leave the chat
}

async function runPeers(): Promise<void> {
  const mesh = await startMesh();
  try {
    const peers = await mesh.getPeers();
    const row = (a: string, b: string, c: string, d: string): string =>
      `${a.padEnd(24)}${b.padEnd(28)}${c.padEnd(16)}${d}`;
    console.log(row('deviceName', 'deviceId', 'ip', 'online'));
    for (const peer of peers) {
      console.log(row(peer.deviceName, peer.deviceId, peer.ip, peer.online ? 'yes' : 'no'));
    }
  } finally {
    await mesh.stop().catch(() => {});
  }
  process.exit(0);
}

async function main(): Promise<void> {
  const [command, a, b] = process.argv.slice(2);
  switch (command) {
    case 'serve':
      await runServe(a);
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
  console.error(`chat: ${err instanceof Error ? err.message : String(err)}`);
  process.exit(1);
});
