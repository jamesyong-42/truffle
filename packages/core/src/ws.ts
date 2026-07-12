// WebSocket over the mesh via the `ws` package (RFC 021 Phase 4, D3).
//
// mesh.ws ships zero new Rust: it runs the battle-tested `ws` npm package over
// mesh TCP. The client drives `ws`'s HTTP upgrade through a MeshAgent (whose
// `createConnection` returns a mesh `TruffleSocket`); the server feeds mesh
// connections into a stock `http.Server` and lets a `noServer` WebSocketServer
// complete the upgrade — exactly the seams `ws` documents for custom
// transports. `websocket.rs` stays the internal envelope pipe on 9417; this is
// a separate, user-facing raw-WebSocket surface.
//
// `ws` is an OPTIONAL peer dependency: apps that never touch mesh.ws don't need
// it installed. Nothing here imports `ws` at module load — the namespace holds
// lazy async functions that `await import('ws')` on first use and throw a
// clear, actionable error when it's missing.

import http from 'node:http';
import type { Duplex } from 'node:stream';
import { MeshAgent, createMeshHttpServer } from './http.js';
import type { TruffleNet } from './net.js';
import { peerLikeToQuery, type PeerLike } from './peer.js';
// Type-only: erased at compile time, so it never forces `ws` to be installed
// (safe with an optional dependency as long as @types/ws is a devDependency).
import type WebSocket from 'ws';
import type {
  WebSocketServer as WsServerInstance,
  ServerOptions as WsServerOptions,
  ClientOptions as WsClientOptions,
} from 'ws';

/**
 * Hostname placed in the `ws://` URL when the caller's peer reference can't be
 * a URL hostname (device names with spaces, case-sensitive device ids, …).
 * It only ever sets the HTTP `Host` header — correctness never depends on it,
 * because the agent dials the real peer reference verbatim (see {@link PIN_KEY}).
 * `.invalid` is reserved by RFC 2606 and never resolves, so it reads as the
 * placeholder it is.
 */
const PLACEHOLDER_HOST = 'truffle.invalid';

/**
 * Request-option key carrying the true peer reference past `ws`'s URL
 * rewriting. `ws` overwrites `options.host` with the URL's hostname before the
 * agent sees it, so a non-URL-safe peer reference can't ride `options.host`.
 * It CAN ride an unknown key: `ws` spreads user options into the request and
 * `http` forwards unknown keys to the agent's `createConnection` untouched
 * (verified empirically). {@link MeshWsAgent} reads it back.
 */
const PIN_KEY = 'truffleWsPeer';

/** The subset of the `ws` module mesh.ws uses; see {@link WsLoader}. */
interface WsExports {
  /** The `ws` client constructor. */
  WebSocket: new (address: string, options?: WsClientOptions) => WebSocket;
  /** The `ws` server constructor. */
  WebSocketServer: new (options?: WsServerOptions) => WsServerInstance;
}

/**
 * Loads the `ws` module. Defaults to a real `import('ws')`; the seam exists so
 * tests can inject a loader (including one that rejects, to exercise the
 * missing-dependency path) without touching the filesystem.
 */
export type WsLoader = () => Promise<WsExports>;

const importWs: WsLoader = () => import('ws') as Promise<WsExports>;

/**
 * A {@link MeshAgent} that honours a pinned peer reference ({@link PIN_KEY}).
 *
 * For a URL-safe peer reference the pin equals the URL hostname and this is a
 * no-op; for a non-URL-safe one the URL carries a placeholder and the real
 * reference arrives only via the pin, so we rewrite `host`/`hostname` before
 * delegating to MeshAgent — which then dials the exact peer over mesh TCP.
 */
class MeshWsAgent extends MeshAgent {
  override createConnection(
    options: http.ClientRequestArgs,
    callback?: (err: Error | null, stream: Duplex) => void,
  ): Duplex {
    const pinned = (options as Record<string, unknown>)[PIN_KEY];
    if (typeof pinned === 'string' && pinned.length > 0) {
      return super.createConnection({ ...options, host: pinned, hostname: pinned }, callback);
    }
    return super.createConnection(options, callback);
  }
}

/**
 * A `ws` {@link WebSocket.Server} bound to a mesh listener.
 *
 * Everything about it is the ordinary `ws` server API — most importantly the
 * `'connection'` event still fires with `(ws, request)` — with two additions:
 * `port` (resolved, so an ephemeral `0` request surfaces the real port) and a
 * `close()` that also tears down the underlying mesh listener.
 */
export interface TruffleWsServer extends WsServerInstance {
  /** The mesh port this server is bound to (resolved when `0` was requested). */
  readonly port: number;
}

/** Options for {@link TruffleWs.createServer}. */
export interface TruffleWsServerOptions {
  /**
   * Mesh port to listen on. `0` binds an ephemeral port (read it back from
   * `server.port`). The session WebSocket port (default 9417) is reserved
   * and rejected.
   */
  port: number;
  /**
   * Optional path filter, like `ws`'s own `path` option: only upgrade requests
   * whose URL path matches are accepted; others get a `400`-style socket close.
   */
  path?: string;
  /**
   * Terminate TLS in the sidecar with automatic MagicDNS certificates so
   * browsers can connect via `wss://name.tailnet.ts.net` (RFC 023 §7.1).
   * Same prerequisites as `mesh.http.createServer({ tls: true })`.
   */
  tls?: boolean;
}

/** WebSocket-over-mesh namespace bound to a mesh node's `net` namespace. */
export interface TruffleWs {
  /**
   * Open a WebSocket to a peer, returning a connecting `ws` `WebSocket` (await
   * its `'open'` event as usual). `host` accepts any peer reference — device
   * id (or ≥4-char prefix), device name, Tailscale hostname, or `100.x` IP —
   * including names with spaces and case-sensitive ids that aren't valid URL
   * hostnames (they're pinned past the URL, see the module docs). `path`
   * defaults to `/`; `options` are forwarded to the `ws` `WebSocket`
   * constructor (a caller-supplied `agent` is overridden — the mesh agent is
   * how it reaches the peer).
   */
  connect(
    host: PeerLike,
    port: number,
    path?: string,
    options?: WsClientOptions,
  ): Promise<WebSocket>;
  /**
   * Start a WebSocket server on the mesh. Resolves once it's listening; the
   * returned server's `'connection'` event delivers `(ws, request)` like any
   * `ws` server. Close it (and release the mesh port) with `server.close()`.
   */
  createServer(options: TruffleWsServerOptions): Promise<TruffleWsServer>;
}

/**
 * Build the `mesh.ws` namespace over a node's `net` namespace.
 *
 * @param net   The node's {@link TruffleNet} (mesh TCP), used for the client
 *              agent and the server's listener.
 * @param load  How to obtain the `ws` module; defaults to `import('ws')`.
 *              Injectable for tests.
 */
export function createWsNamespace(net: TruffleNet, load: WsLoader = importWs): TruffleWs {
  const agent = new MeshWsAgent(net);

  async function loadWs(): Promise<WsExports> {
    try {
      return await load();
    } catch (err) {
      throw new Error(
        "mesh.ws requires the 'ws' package, which is an optional peer dependency — " +
          'install it with `npm install ws` (or `pnpm add ws`).',
        { cause: err },
      );
    }
  }

  async function connect(
    host: PeerLike,
    port: number,
    path = '/',
    options: WsClientOptions = {},
  ): Promise<WebSocket> {
    const hostQuery = peerLikeToQuery(host);
    const { WebSocket: WebSocketImpl } = await loadWs();
    const requestPath = path.startsWith('/') ? path : `/${path}`;

    // The URL hostname only sets the HTTP Host header; the agent always dials
    // the pinned peer. Embed `hostQuery` when it round-trips as a URL hostname
    // unchanged (a plain IP or lowercase device name), so the Host header is
    // natural; otherwise a placeholder (a name with spaces, or a device id
    // whose case a URL would fold — either would dial the wrong peer).
    const urlHost = urlHostnameFor(hostQuery, port) ?? PLACEHOLDER_HOST;
    const url = `ws://${urlHost}:${port}${requestPath}`;

    // `agent` last: the mesh agent is non-negotiable. PIN_KEY is an unknown key
    // ws/http pass through to the agent (see PIN_KEY); cast past the typed
    // ClientOptions to attach it.
    const wsOptions: Record<string, unknown> = {
      ...options,
      agent,
      [PIN_KEY]: hostQuery,
    };
    return new WebSocketImpl(url, wsOptions as WsClientOptions);
  }

  async function createServer(options: TruffleWsServerOptions): Promise<TruffleWsServer> {
    const { WebSocketServer } = await loadWs();

    // A mesh-listening http.Server (the shared RFC 023 engine) does the
    // HTTP/WebSocket upgrade parsing; a noServer WebSocketServer completes
    // the handshake — the seams `ws` documents for custom transports.
    const httpServer = options.tls
      ? createMeshHttpServer(net, { tls: true })
      : createMeshHttpServer(net);
    const wss = new WebSocketServer({ noServer: true, path: options.path });

    httpServer.on('upgrade', (req, socket, head) => {
      // `ws`'s own path check: true when no `path` was set, else on a match.
      Promise.resolve(wss.shouldHandle(req)).then((ok) => {
        if (!ok) {
          socket.destroy();
          return;
        }
        wss.handleUpgrade(req, socket, head, (client: WebSocket, request: http.IncomingMessage) => {
          wss.emit('connection', client, request);
        });
      });
    });

    await new Promise<void>((resolve, reject) => {
      const onError = (err: Error): void => reject(err);
      httpServer.once('error', onError);
      httpServer.listen(options.port, () => {
        httpServer.removeListener('error', onError);
        resolve();
      });
    });

    const server = wss as TruffleWsServer;
    Object.defineProperty(server, 'port', {
      value: httpServer.port ?? options.port,
      enumerable: true,
    });

    // close() also tears down the mesh listener and any live client sockets.
    // Keep ws's void-returning signature so the type stays a ws Server.
    const closeWs = wss.close.bind(wss);
    server.close = (callback?: (err?: Error) => void): void => {
      for (const client of wss.clients) client.close();
      httpServer.close();
      closeWs(callback);
    };

    return server;
  }

  return { connect, createServer };
}

/**
 * Returns `host` when it is a URL hostname that round-trips unchanged (so it's
 * safe to embed in a `ws://` URL and use for the Host header), else `null`.
 * The equality check rejects anything a WHATWG URL would normalise — notably
 * uppercase device ids, which URL parsing lowercases.
 */
function urlHostnameFor(host: string, port: number): string | null {
  try {
    return new URL(`ws://${host}:${port}`).hostname === host ? host : null;
  } catch {
    return null;
  }
}
