/**
 * QUIC Streams Example - multiplexed streams over the mesh
 *
 * Demonstrates `mesh.quic` (RFC 021 §8): one QUIC connection carrying many
 * concurrent, independent bidirectional byte streams with no head-of-line
 * blocking between them — the property that sets QUIC apart from opening N
 * separate TCP connections. `serve` echoes every stream it receives;
 * `bench` opens several streams at once over a single connection, writes a
 * distinct payload on each, and times them individually to show they
 * complete together rather than one at a time.
 *
 * Prerequisites:
 *   - Tailscale installed and running on both devices
 *   - npm install @vibecook/truffle
 *   - Both devices run with the SAME appId ('quic-demo', below) — appId
 *     scopes peer visibility, so mismatched apps can't see each other.
 *
 * Usage:
 *   npx tsx src/main.ts serve <port>
 *   npx tsx src/main.ts bench <peer> <port> [streams=4]
 *   npx tsx src/main.ts peers
 *
 * See README.md for the two-device walkthrough.
 */

import { randomBytes } from 'node:crypto';
import {
  createMeshNode,
  type MeshNode,
  type TruffleQuicConnection,
  type TruffleQuicServer,
} from '@vibecook/truffle';

const APP_ID = 'quic-demo';
const DEFAULT_STREAM_COUNT = 4;
const PAYLOAD_BYTES = 32 * 1024; // 32 KiB per stream

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
 * exit with `code`. Every event that can end the run — the serve loop's
 * SIGINT, a bench failure, or a clean finish — calls the *same* exiter, so
 * whichever fires first wins (no double `mesh.stop()`, no exit-code
 * clobbering from a second caller).
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
    console.error(
      `quic-streams: invalid port: ${raw === undefined ? '(missing)' : JSON.stringify(raw)}`,
    );
    process.exit(1);
  }
  return port;
}

function usage(): void {
  console.error('Usage:');
  console.error('  quic-streams serve <port>');
  console.error('  quic-streams bench <peer> <port> [streams=4]');
  console.error('  quic-streams peers');
}

async function runServe(portArg: string | undefined): Promise<void> {
  const port = parsePort(portArg);
  const mesh = await startMesh();
  const exit = makeExiter(mesh);
  process.on('SIGINT', () => exit(0));

  let server: TruffleQuicServer;
  try {
    server = await mesh.quic.listen(port);
  } catch (err) {
    console.error(
      `quic-streams: listen on port ${port} failed: ${err instanceof Error ? err.message : String(err)}`,
    );
    exit(1);
    return;
  }
  console.error(`listening on port ${server.port}, waiting for connections...`);

  for await (const conn of server) {
    const peerLabel = conn.remotePeerId ?? conn.remoteAddress;
    console.error(`connection from ${peerLabel}`);
    (async () => {
      for await (const stream of conn.streams()) {
        stream.pipe(stream); // echo every stream independently — no HoL blocking between siblings
      }
    })().catch((err: unknown) => {
      console.error(
        `quic-streams: connection ${peerLabel} error: ${err instanceof Error ? err.message : String(err)}`,
      );
    });
  }
}

function makePayload(index: number): Buffer {
  const prefix = Buffer.from(`stream-${index}: `, 'utf8');
  const filler = randomBytes(PAYLOAD_BYTES - prefix.length);
  return Buffer.concat([prefix, filler], PAYLOAD_BYTES);
}

interface StreamTiming {
  index: number;
  ms: number;
}

/** Open one stream, write its payload, read the echo to completion, and verify it matches. */
async function runOneStream(conn: TruffleQuicConnection, index: number): Promise<StreamTiming> {
  const payload = makePayload(index);
  const start = process.hrtime.bigint();
  const stream = await conn.openStream();

  // Attach both consumers before calling end() so neither can miss an event
  // that fires immediately (e.g. a same-tick 'error').
  const chunks: Buffer[] = [];
  const readLoop = (async () => {
    for await (const chunk of stream) {
      chunks.push(chunk);
    }
  })();
  const writeDone = new Promise<void>((resolve, reject) => {
    stream.once('finish', resolve);
    stream.once('error', reject);
  });

  stream.end(payload); // write the payload, then half-close (peer sees EOF)
  await Promise.all([readLoop, writeDone]);

  const echoed = Buffer.concat(chunks);
  const ms = Number(process.hrtime.bigint() - start) / 1_000_000;

  if (!echoed.equals(payload)) {
    throw new Error(
      `stream ${index}: echo mismatch (sent ${payload.length} bytes, received ${echoed.length} bytes)`,
    );
  }
  return { index, ms };
}

async function runBench(
  peer: string | undefined,
  portArg: string | undefined,
  countArg: string | undefined,
): Promise<void> {
  if (!peer) {
    usage();
    process.exit(1);
  }
  const port = parsePort(portArg);
  const streamCount = countArg === undefined ? DEFAULT_STREAM_COUNT : Number(countArg);
  if (!Number.isInteger(streamCount) || streamCount < 1) {
    console.error(`quic-streams: invalid stream count: ${JSON.stringify(countArg)}`);
    process.exit(1);
  }

  const mesh = await startMesh();
  const exit = makeExiter(mesh);
  process.on('SIGINT', () => exit(0));

  let conn: TruffleQuicConnection;
  try {
    conn = await mesh.quic.connect(peer, port);
  } catch (err) {
    console.error(
      `quic-streams: connect to ${peer}:${port} failed: ${err instanceof Error ? err.message : String(err)}`,
    );
    exit(1);
    return;
  }
  console.error(`connected: ${peer}:${port} (${conn.remotePeerId ?? conn.remoteAddress})`);
  console.error(
    `opening ${streamCount} concurrent streams, ${PAYLOAD_BYTES} bytes each, over one connection...`,
  );

  const wallStart = process.hrtime.bigint();
  try {
    const timings = await Promise.all(
      Array.from({ length: streamCount }, (_, index) => runOneStream(conn, index)),
    );
    const wallMs = Number(process.hrtime.bigint() - wallStart) / 1_000_000;
    const sumMs = timings.reduce((total, t) => total + t.ms, 0);

    for (const t of timings.sort((a, b) => a.index - b.index)) {
      console.log(
        `stream ${t.index}: ${t.ms.toFixed(2)} ms (${PAYLOAD_BYTES} bytes, echo verified)`,
      );
    }
    console.log(
      `total: ${wallMs.toFixed(2)} ms wall time for ${streamCount} streams ` +
        `(sum of individual times: ${sumMs.toFixed(2)} ms) — wall time tracks the slowest ` +
        `stream, not the sum, because they ran concurrently over one connection.`,
    );
  } catch (err) {
    console.error(`quic-streams: bench failed: ${err instanceof Error ? err.message : String(err)}`);
    conn.close();
    exit(1);
    return;
  }

  conn.close();
  exit(0);
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
  const [command, a, b, c] = process.argv.slice(2);
  switch (command) {
    case 'serve':
      await runServe(a);
      break;
    case 'bench':
      await runBench(a, b, c);
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
  console.error(`quic-streams: ${err instanceof Error ? err.message : String(err)}`);
  process.exit(1);
});
