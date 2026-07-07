import { execFile } from 'node:child_process';
import { NapiNode, type NapiNodeConfig, type NapiPeerEvent } from '@vibecook/truffle-native';
import { createNetNamespace, type TruffleNet } from './net.js';
import { createHttpNamespace, type TruffleHttp } from './http.js';
import { createQuicNamespace, type TruffleQuic } from './quic.js';
import { createDgramNamespace, type TruffleDgram } from './dgram.js';
import { createWsNamespace, type TruffleWs } from './ws.js';
import { resolveSidecarPath } from './sidecar.js';

/**
 * A started mesh node: the full native `NapiNode` API plus protocol
 * namespaces (RFC 021). Namespaces are attached to the same underlying
 * instance, so every `NapiNode` method is available directly and new
 * native methods surface automatically.
 */
export type MeshNode = NapiNode & {
  /** node:net-shaped raw TCP API over the mesh (RFC 021). */
  net: TruffleNet;
  /**
   * node:http interop over the mesh (RFC 021): a MeshAgent for outbound
   * requests to peers plus `request`/`get`/`fetchText` sugar. Inbound HTTP
   * is served by feeding `mesh.net` connections into an `http.Server` via
   * `httpServer.emit('connection', socket)`.
   */
  http: TruffleHttp;
  /**
   * QUIC over the mesh (RFC 021): multiplexed bidirectional byte streams
   * with no head-of-line blocking, async-iterable connections/streams.
   */
  quic: TruffleQuic;
  /**
   * node:dgram-shaped raw UDP API over the mesh (RFC 021): datagram sockets
   * with `'message'` events and preserved datagram boundaries.
   */
  dgram: TruffleDgram;
  /**
   * WebSocket over the mesh (RFC 021): thin wrappers over the `ws` package —
   * `ws.connect(peer, port)` for a client, `ws.createServer({ port })` for a
   * server. Requires the optional `ws` peer dependency to be installed.
   */
  ws: TruffleWs;
  /** The underlying native handle (escape hatch; the same object). */
  native: NapiNode;
};

/**
 * Options for {@link createMeshNode}. RFC 017 shape — `appId` is required,
 * `deviceName` / `deviceId` are optional overrides that the underlying
 * `NodeBuilder` defaults when absent (OS hostname / auto-generated ULID).
 */
export interface CreateMeshNodeOptions {
  /**
   * Required. Application namespace identifier. Two apps with different
   * `appId`s cannot see each other as peers.
   *
   * Format: `^[a-z][a-z0-9-]{1,31}$` (lowercase letters, digits, hyphens,
   * 2–32 characters, must start with a letter).
   */
  appId: string;
  /**
   * Optional human-readable device name. Defaults to the OS hostname.
   * May contain any Unicode characters; truffle derives a Tailscale-safe
   * hostname from this automatically.
   */
  deviceName?: string;
  /**
   * Optional stable per-device ULID. If omitted, one is generated on
   * first run and persisted inside `stateDir`. Provide your own if you
   * want to federate identity with an existing user system.
   */
  deviceId?: string;
  /** Override sidecar path. If omitted, auto-resolved. */
  sidecarPath?: string;
  /**
   * State directory for Tailscale. Defaults to
   * `{userDataDir}/truffle/{appId}/{slug(deviceName)}`.
   */
  stateDir?: string;
  /** Tailscale auth key (for headless/CI setups). */
  authKey?: string;
  /** Whether this node is ephemeral (removed from tailnet on stop). */
  ephemeral?: boolean;
  /** WebSocket listener port. Defaults to 9417 when omitted. */
  wsPort?: number;
  /**
   * Auto-open the Tailscale auth URL in the default browser.
   * Defaults to true. Set to false to handle auth manually.
   */
  autoAuth?: boolean;
  /**
   * Custom URL opener. Overrides the default browser opener.
   * Useful in Electron where you'd use `shell.openExternal()`.
   */
  openUrl?: (url: string) => void;
  /** Called when Tailscale auth is required. Receives the auth URL. */
  onAuthRequired?: (url: string) => void;
  /** Called for peer change events (join, leave, update, ws_connected, etc.). */
  onPeerChange?: (event: NapiPeerEvent) => void;
}

/**
 * Open a URL in the system's default browser.
 */
function defaultOpenUrl(url: string): void {
  // Only ever open http(s) URLs, and pass the URL as an argv element (never
  // through a shell) so it cannot be interpreted as a command. The previous
  // `execSync(`open "${url}"`)` form let shell metacharacters ($(), backticks)
  // in the URL execute arbitrary commands.
  if (!/^https?:\/\//i.test(url)) return;
  const [cmd, args]: [string, string[]] =
    process.platform === 'darwin'
      ? ['open', [url]]
      : process.platform === 'win32'
        ? ['explorer', [url]]
        : ['xdg-open', [url]];
  execFile(cmd, args, () => {
    // Best-effort; ignore failures (no browser, headless environment, etc.).
  });
}

/**
 * Create and start a Truffle node with sensible defaults.
 *
 * - Auto-resolves the sidecar binary path
 * - Auto-opens the Tailscale auth URL in the browser (configurable)
 * - Provides auth and peer lifecycle callbacks
 *
 * @example
 * ```ts
 * const mesh = await createMeshNode({
 *   appId: 'playground',
 *   deviceName: 'alice-mbp',
 *   onPeerChange: (event) => console.log('Peer event:', event),
 * });
 *
 * const peers = await mesh.getPeers();
 * await mesh.send(peers[0].deviceId, 'chat', Buffer.from(JSON.stringify({ text: 'hello' })));
 *
 * // Raw TCP over the mesh, node:net-style (RFC 021):
 * const server = mesh.net.createServer((socket) => socket.pipe(socket));
 * server.listen(8080);
 * const client = mesh.net.connect({ host: 'other-machine', port: 8080 });
 * ```
 */
export async function createMeshNode(options: CreateMeshNodeOptions): Promise<MeshNode> {
  const {
    appId,
    deviceName,
    deviceId,
    autoAuth = true,
    openUrl: customOpenUrl,
    onAuthRequired,
    onPeerChange,
    sidecarPath,
    stateDir,
    authKey,
    ephemeral,
    wsPort,
  } = options;

  // Validate appId synchronously (mirrors the Rust-side rule) so a bad value
  // fails here with a clear error instead of asynchronously across the FFI.
  if (!/^[a-z][a-z0-9-]{1,31}$/.test(appId)) {
    throw new Error(
      `[truffle] Invalid appId ${JSON.stringify(appId)}: must match ^[a-z][a-z0-9-]{1,31}$ ` +
        `(2–32 chars; lowercase letters, digits, hyphens; must start with a letter).`,
    );
  }

  const resolvedSidecarPath = sidecarPath ?? resolveSidecarPath();

  const node = new NapiNode();

  // IMPORTANT: install the auth-required callback BEFORE calling `start()`.
  // `start()` blocks waiting for Tailscale authentication to complete, so
  // an `onPeerChange` subscription installed afterwards misses the
  // `auth_required` event entirely and `start()` hangs until its internal
  // 5-minute timeout with "timed out waiting for authentication".
  //
  // `onAuthRequired` is a distinct, pre-start hook on NapiNode that bridges
  // to `NodeBuilder::build_with_auth_handler` under the hood.
  node.onAuthRequired((url: string) => {
    if (autoAuth) {
      const opener = customOpenUrl ?? defaultOpenUrl;
      opener(url);
    }
    onAuthRequired?.(url);
  });

  const config: NapiNodeConfig = {
    appId,
    deviceName,
    deviceId,
    sidecarPath: resolvedSidecarPath,
    stateDir,
    authKey,
    ephemeral,
    wsPort,
  };

  try {
    await node.start(config);
  } catch (err) {
    // Best-effort cleanup so a failed start doesn't leak the spawned sidecar
    // process. `stop()` is safe to call even if start() failed early.
    try {
      await node.stop();
    } catch {
      // ignore — nothing to clean up, or already torn down
    }
    throw err;
  }

  // Post-start: forward ongoing peer lifecycle events to the user callback.
  if (onPeerChange) {
    node.onPeerChange(onPeerChange);
  }

  // Attach the protocol namespaces (RFC 021) to the native instance.
  const mesh = node as MeshNode;
  mesh.net = createNetNamespace(node);
  mesh.http = createHttpNamespace(mesh.net);
  mesh.quic = createQuicNamespace(node);
  mesh.dgram = createDgramNamespace(node);
  mesh.ws = createWsNamespace(mesh.net);
  mesh.native = node;

  return mesh;
}
