/**
 * Truffle Playground
 * ==================
 *
 * An interactive kitchen-sink REPL that exercises every public feature of
 * `@vibecook/truffle`:
 *
 *   - Peer discovery via Tailscale (join/leave/update events)
 *   - Namespaced messaging (chat-style) with `send()` + `broadcast()`
 *   - Shared state sync via SyncedStore (a distributed key/value map)
 *   - File transfer (send, receive with auto-accept, progress events)
 *   - Health checks and latency pings
 *
 * Run on two or more devices on the same Tailscale network:
 *
 *     pnpm --filter @vibecook/example-playground start
 *     # or, from this directory:
 *     npm start
 *
 * Then use the REPL. Type `help` for a list of commands.
 *
 * Override the node name with the NAME env var:
 *
 *     NAME=alice npm start   # device 1
 *     NAME=bob   npm start   # device 2
 *
 * Using this example outside the monorepo? Replace `"workspace:*"` in
 * package.json with the latest version from npm, e.g. `"^0.3.23"`.
 *
 * Prerequisites:
 *   - Tailscale installed and signed in on each device
 *   - Node.js 18+ on each device
 */

import { mkdirSync } from 'node:fs';
import { basename, resolve } from 'node:path';
import { createInterface, type Interface as ReadlineInterface } from 'node:readline';

import {
  createMeshNode,
  type NapiFileOffer,
  type NapiFileTransferEvent,
  type NapiNamespacedMessage,
  type NapiNode,
  type NapiPeer,
  type NapiPeerEvent,
  type NapiStoreEvent,
  type NapiSyncedStore,
  type NapiTransferProgress,
} from '@vibecook/truffle';

// ─── Configuration ──────────────────────────────────────────────────────

/** The device name advertised to the tailnet. Override with NAME env var. */
const NODE_NAME = process.env.NAME ?? `playground-${process.pid}`;

/** Namespace used for chat-style messages. */
const CHAT_NS = 'chat';

/** SyncedStore identifier — peers must use the same ID to sync state. */
const STORE_ID = 'playground-state';

/** Where incoming files are auto-saved. */
const DOWNLOADS_DIR = resolve(process.cwd(), 'downloads');

// ─── Types ──────────────────────────────────────────────────────────────

interface ChatMessage {
  from: string;
  text: string;
  ts: number;
}

interface PlaygroundState {
  /** Arbitrary key/value map this node advertises to peers. */
  kv: Record<string, string>;
  /** Last time this slice was updated (ms since epoch). */
  updatedAt: number;
}

// ─── Small logging helpers ──────────────────────────────────────────────

const dim = (s: string): string => `\x1b[2m${s}\x1b[0m`;
const bold = (s: string): string => `\x1b[1m${s}\x1b[0m`;
const cyan = (s: string): string => `\x1b[36m${s}\x1b[0m`;
const green = (s: string): string => `\x1b[32m${s}\x1b[0m`;
const yellow = (s: string): string => `\x1b[33m${s}\x1b[0m`;
const red = (s: string): string => `\x1b[31m${s}\x1b[0m`;

function log(prefix: string, ...parts: unknown[]): void {
  const ts = new Date().toLocaleTimeString();
  process.stdout.write('\r\x1b[K'); // clear current prompt line
  console.log(dim(`[${ts}]`), prefix, ...parts);
  rl?.prompt(true);
}

// ─── Formatting helpers ─────────────────────────────────────────────────

function formatPeer(peer: NapiPeer): string {
  const status = peer.wsConnected ? green('●') : peer.online ? yellow('○') : red('○');
  const conn = peer.wsConnected ? dim(`(${peer.connectionType})`) : dim('(offline)');
  return `${status} ${bold(peer.name)} ${dim(peer.id.slice(0, 8))} ${peer.ip} ${conn}`;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MiB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

// ─── State we hold across the REPL ──────────────────────────────────────

let node: NapiNode | undefined;
let store: NapiSyncedStore | undefined;
let rl: ReadlineInterface | undefined;

const localState: PlaygroundState = {
  kv: {},
  updatedAt: Date.now(),
};

// ─── Peer event handler ─────────────────────────────────────────────────

function handlePeerEvent(event: NapiPeerEvent): void {
  switch (event.eventType) {
    case 'auth_required':
      log(cyan('[auth]'), `Tailscale login required → ${event.authUrl}`);
      log(cyan('[auth]'), 'Your browser should open automatically.');
      break;
    case 'joined':
      if (event.peer) log(green('[peer]'), `joined: ${formatPeer(event.peer)}`);
      break;
    case 'left':
      log(yellow('[peer]'), `left: ${dim(event.peerId.slice(0, 8))}`);
      break;
    case 'ws_connected':
      if (event.peer) log(green('[peer]'), `ws connected: ${formatPeer(event.peer)}`);
      break;
    case 'ws_disconnected':
      log(yellow('[peer]'), `ws disconnected: ${dim(event.peerId.slice(0, 8))}`);
      break;
    case 'updated':
      // Too noisy to log every update — uncomment to debug.
      // if (event.peer) log(dim('[peer]'), `updated: ${formatPeer(event.peer)}`);
      break;
  }
}

// ─── Chat messaging ─────────────────────────────────────────────────────

function handleChatMessage(msg: NapiNamespacedMessage): void {
  try {
    // Payload is JSON-decoded by NAPI, so `msg.payload` is already an object.
    const chat = msg.payload as ChatMessage;
    log(cyan(`[${chat.from}]`), chat.text);
  } catch (err) {
    log(red('[chat]'), `malformed message from ${msg.from}:`, err);
  }
}

async function sendChat(n: NapiNode, peerName: string, text: string): Promise<void> {
  const peerId = await n.resolvePeerId(peerName);
  const payload: ChatMessage = { from: NODE_NAME, text, ts: Date.now() };
  await n.send(peerId, CHAT_NS, Buffer.from(JSON.stringify(payload)));
}

async function broadcastChat(n: NapiNode, text: string): Promise<void> {
  const payload: ChatMessage = { from: NODE_NAME, text, ts: Date.now() };
  await n.broadcast(CHAT_NS, Buffer.from(JSON.stringify(payload)));
}

// ─── Shared state via SyncedStore ───────────────────────────────────────

function handleStoreEvent(event: NapiStoreEvent): void {
  switch (event.eventType) {
    case 'local_changed':
      // This fires after we call `store.set(...)` ourselves — no-op.
      break;
    case 'peer_updated':
      log(
        cyan('[store]'),
        `peer ${dim((event.deviceId ?? '').slice(0, 8))} updated (v${event.version})`,
      );
      break;
    case 'peer_removed':
      log(yellow('[store]'), `peer ${dim((event.deviceId ?? '').slice(0, 8))} removed`);
      break;
  }
}

async function commitLocalState(s: NapiSyncedStore): Promise<void> {
  localState.updatedAt = Date.now();
  await s.set(localState);
}

// ─── File transfer ──────────────────────────────────────────────────────

function attachFileTransferHandlers(n: NapiNode): void {
  const ft = n.fileTransfer();

  mkdirSync(DOWNLOADS_DIR, { recursive: true });

  // Auto-accept incoming files into ./downloads. For manual control, use
  // `ft.onOffer((offer, responder) => responder.accept('/custom/path'))`.
  void ft.autoAccept(DOWNLOADS_DIR).then(() => {
    log(dim('[ft]'), `auto-accepting incoming files into ${DOWNLOADS_DIR}`);
  });

  ft.onOffer((offer: NapiFileOffer) => {
    log(
      cyan('[ft]'),
      `offer from ${offer.fromName}: ${bold(offer.fileName)} (${formatBytes(offer.size)})`,
    );
  });

  ft.onEvent((event: NapiFileTransferEvent) => {
    switch (event.eventType) {
      case 'progress': {
        const p = event.progress as NapiTransferProgress;
        const pct = ((p.bytesTransferred / p.totalBytes) * 100).toFixed(0);
        const speed = formatBytes(p.speedBps) + '/s';
        // Write progress in-place without spamming new lines.
        process.stdout.write(
          `\r${dim('[ft]')} ${p.direction} ${p.fileName}: ${pct}% (${speed})      `,
        );
        break;
      }
      case 'completed':
        process.stdout.write('\r\x1b[K');
        log(
          green('[ft]'),
          `${event.direction} completed: ${event.fileName} ` +
            `(${formatBytes(event.bytesTransferred ?? 0)} in ${(event.elapsedSecs ?? 0).toFixed(1)}s)`,
        );
        break;
      case 'failed':
        process.stdout.write('\r\x1b[K');
        log(red('[ft]'), `${event.fileName ?? '?'} failed: ${event.reason ?? 'unknown'}`);
        break;
      case 'rejected':
        log(yellow('[ft]'), `${event.fileName ?? '?'} rejected: ${event.reason ?? ''}`);
        break;
    }
  });
}

async function sendFileTo(n: NapiNode, peerName: string, localPath: string): Promise<void> {
  const ft = n.fileTransfer();
  const peerId = await n.resolvePeerId(peerName);
  const remotePath = basename(localPath);
  log(dim('[ft]'), `sending ${localPath} → ${peerName} (${remotePath})`);
  const result = await ft.sendFile(peerId, resolve(localPath), remotePath);
  log(
    green('[ft]'),
    `sent ${remotePath}: ${formatBytes(result.bytesTransferred)} in ${result.elapsedSecs.toFixed(1)}s`,
  );
}

// ─── REPL ───────────────────────────────────────────────────────────────

const HELP_TEXT = `
Commands:
  ${bold('peers')}                  list all known peers
  ${bold('ping')} <name>            ping a peer and show round-trip time
  ${bold('health')}                 show network health info
  ${bold('send')} <name> <text...>  send a chat message to one peer
  ${bold('broadcast')} <text...>    broadcast a chat message to all peers
  ${bold('set')} <key> <value>      update the shared SyncedStore
  ${bold('unset')} <key>            remove a key from the shared SyncedStore
  ${bold('state')}                  print this node's shared state
  ${bold('all')}                    print shared state from all peers
  ${bold('send-file')} <name> <path>  send a file to a peer
  ${bold('help')}                   show this message
  ${bold('quit')} | ${bold('exit')}         shutdown and exit
`;

async function handleCommand(n: NapiNode, s: NapiSyncedStore, line: string): Promise<void> {
  const parts = line.trim().split(/\s+/);
  const cmd = parts[0]?.toLowerCase() ?? '';
  const rest = parts.slice(1);

  switch (cmd) {
    case '':
      return;

    case 'help':
    case '?':
      console.log(HELP_TEXT);
      return;

    case 'peers': {
      const peers = await n.getPeers();
      if (peers.length === 0) {
        console.log(dim('  (no peers yet — waiting for Tailscale discovery)'));
      } else {
        peers.forEach((p) => console.log('  ' + formatPeer(p)));
      }
      return;
    }

    case 'ping': {
      const name = rest[0];
      if (!name) return void console.log(red('usage: ping <name>'));
      try {
        const peerId = await n.resolvePeerId(name);
        const result = await n.ping(peerId);
        console.log(
          `  ${green('✓')} ${name}: ${result.latencyMs.toFixed(1)}ms (${result.connection})`,
        );
      } catch (err) {
        console.log(red(`  ✗ ping failed: ${(err as Error).message}`));
      }
      return;
    }

    case 'health': {
      const health = await n.health();
      const icon = health.healthy ? green('✓') : red('✗');
      console.log(`  ${icon} state: ${health.state}`);
      if (health.keyExpiry) console.log(`    key expires: ${health.keyExpiry}`);
      if (health.warnings.length > 0) {
        console.log(`    warnings: ${yellow(health.warnings.join(', '))}`);
      }
      return;
    }

    case 'send': {
      const [name, ...words] = rest;
      if (!name || words.length === 0) {
        return void console.log(red('usage: send <name> <text...>'));
      }
      try {
        await sendChat(n, name, words.join(' '));
      } catch (err) {
        console.log(red(`  ✗ send failed: ${(err as Error).message}`));
      }
      return;
    }

    case 'broadcast':
    case 'bcast': {
      if (rest.length === 0) return void console.log(red('usage: broadcast <text...>'));
      try {
        await broadcastChat(n, rest.join(' '));
      } catch (err) {
        console.log(red(`  ✗ broadcast failed: ${(err as Error).message}`));
      }
      return;
    }

    case 'set': {
      const [key, ...valueParts] = rest;
      if (!key || valueParts.length === 0) {
        return void console.log(red('usage: set <key> <value>'));
      }
      localState.kv[key] = valueParts.join(' ');
      await commitLocalState(s);
      console.log(green(`  ✓ ${key} = ${localState.kv[key]}`));
      return;
    }

    case 'unset': {
      const key = rest[0];
      if (!key) return void console.log(red('usage: unset <key>'));
      if (!(key in localState.kv)) {
        return void console.log(dim(`  (${key} not set)`));
      }
      delete localState.kv[key];
      await commitLocalState(s);
      console.log(green(`  ✓ unset ${key}`));
      return;
    }

    case 'state': {
      const local = (await s.local()) as PlaygroundState | null;
      if (!local || Object.keys(local.kv).length === 0) {
        console.log(dim('  (empty — use `set <key> <value>` to populate)'));
      } else {
        Object.entries(local.kv).forEach(([k, v]) => console.log(`  ${cyan(k)}: ${v}`));
      }
      return;
    }

    case 'all': {
      const slices = await s.all();
      if (slices.length === 0) {
        console.log(dim('  (no slices — no one has set() anything yet)'));
        return;
      }
      for (const slice of slices) {
        const data = slice.data as PlaygroundState;
        const isLocal = slice.deviceId === n.getLocalInfo().id;
        const marker = isLocal ? green(' (you)') : '';
        console.log(
          `  ${bold(slice.deviceId.slice(0, 8))}${marker} v${slice.version} ` +
            dim(`(${Object.keys(data.kv).length} keys)`),
        );
        Object.entries(data.kv).forEach(([k, v]) => console.log(`    ${cyan(k)}: ${v}`));
      }
      return;
    }

    case 'send-file':
    case 'sendfile': {
      const [name, ...pathParts] = rest;
      if (!name || pathParts.length === 0) {
        return void console.log(red('usage: send-file <name> <path>'));
      }
      try {
        await sendFileTo(n, name, pathParts.join(' '));
      } catch (err) {
        console.log(red(`  ✗ send-file failed: ${(err as Error).message}`));
      }
      return;
    }

    case 'quit':
    case 'exit':
      rl?.close();
      return;

    default:
      console.log(red(`unknown command: ${cmd}`) + dim(' — type `help` for a list'));
  }
}

// ─── Lifecycle ──────────────────────────────────────────────────────────

async function main(): Promise<void> {
  console.log(bold(`\n  Truffle Playground`));
  console.log(dim(`  starting node: ${NODE_NAME}\n`));

  node = await createMeshNode({
    name: NODE_NAME,
    // State dir is per-node so multiple playgrounds on one host don't clash.
    stateDir: `./.truffle-state/${NODE_NAME}`,
    // Clean up on quit so we don't leave stale tailnet entries around.
    ephemeral: true,
    onPeerChange: handlePeerEvent,
  });

  const info = node.getLocalInfo();
  log(green('[node]'), `ready as ${bold(info.name)} ${dim(info.id)}${info.ip ? ` @ ${info.ip}` : ''}`);

  // Register chat-namespace handler.
  node.onMessage(CHAT_NS, handleChatMessage);

  // Set up file transfer (auto-accepts into ./downloads).
  attachFileTransferHandlers(node);

  // Create the shared-state store and subscribe to remote updates.
  store = node.syncedStore(STORE_ID);
  store.onChange(handleStoreEvent);
  await commitLocalState(store); // publish empty initial state

  // Start the REPL.
  rl = createInterface({ input: process.stdin, output: process.stdout });
  rl.setPrompt(bold(`${NODE_NAME}> `));

  console.log(dim('  type `help` to list commands'));
  console.log();
  rl.prompt();

  rl.on('line', async (line) => {
    try {
      await handleCommand(node!, store!, line);
    } catch (err) {
      console.log(red(`error: ${(err as Error).message}`));
    }
    rl!.prompt();
  });

  await new Promise<void>((resolvePromise) => {
    rl!.once('close', () => resolvePromise());
  });
}

async function shutdown(): Promise<void> {
  log(dim('[node]'), 'shutting down...');
  try {
    await store?.stop();
    await node?.stop();
  } catch (err) {
    console.error(red(`shutdown error: ${(err as Error).message}`));
  }
}

// Graceful shutdown on SIGINT / SIGTERM.
process.on('SIGINT', () => {
  rl?.close();
});
process.on('SIGTERM', () => {
  rl?.close();
});

main()
  .then(shutdown)
  .then(() => process.exit(0))
  .catch((err) => {
    console.error(red(`fatal: ${(err as Error).message}`));
    console.error(err);
    void shutdown().finally(() => process.exit(1));
  });
