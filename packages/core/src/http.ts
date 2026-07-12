// node:http interop for the mesh (RFC 021 Phase 2; server half RFC 023 §6.1).
//
// `mesh.http` lets existing node:http / Express code talk to peers over the
// mesh with a one-line change. Two directions:
//
//   • Outbound: point a request at a peer through a MeshAgent
//       mesh.http.get('http://100.64.0.7:8080/api/status', (res) => …)
//       await mesh.http.fetchText('kitchen-pi', 8080, '/api/status')
//   • Inbound: a drop-in http.createServer whose listen() binds the mesh
//       const server = mesh.http.createServer(app);       // e.g. Express
//       server.listen(8080);
//
// Nothing here is mesh-specific beyond two seams: `http.Agent#
// createConnection` is documented to accept any `stream.Duplex` (outbound),
// and an `http.Server` parses any duplex fed to it as a `'connection'` event
// (inbound) — TruffleSocket (see ./net.ts) is one, so node:http drives it
// exactly like a real net.Socket. The compat shims that make that true live
// in ./net.ts.

import http from 'node:http';
import type { Duplex } from 'node:stream';
import type { TruffleNet, TruffleSocket } from './net.js';

/**
 * An `http.Agent` that dials its connections over the mesh instead of the
 * host network. Pass it as `{ agent }` to any `node:http` request and the
 * request travels to the peer named by the URL hostname (or `options.host`).
 *
 * Connection reuse behaves exactly like the stock agent. The default
 * `keepAlive: false` opens a fresh mesh connection per request and closes it
 * on response end; `new MeshAgent(net, { keepAlive: true })` pools and reuses
 * idle sockets like any http.Agent (but see the idle-reaping caveat on
 * `TruffleSocket.setTimeout` — quiet pooled sockets aren't reaped on a timer).
 */
export class MeshAgent extends http.Agent {
  readonly #net: TruffleNet;

  constructor(net: TruffleNet, opts?: http.AgentOptions) {
    super(opts);
    this.#net = net;
  }

  /**
   * Called by node:http to obtain a socket for a request. Returns a mesh
   * TruffleSocket (a `stream.Duplex`) synchronously — it connects in the
   * background and emits `'connect'` later, which is exactly the net.Socket
   * contract http expects. The callback form is also honoured for robustness
   * (node uses whichever settles first, once).
   */
  override createConnection(
    options: http.ClientRequestArgs,
    callback?: (err: Error | null, stream: Duplex) => void,
  ): Duplex {
    const socket = this.#net.connect({ host: hostOf(options), port: portOf(options) });

    if (callback) {
      const onConnect = () => {
        socket.removeListener('error', onError);
        callback(null, socket);
      };
      const onError = (err: Error) => {
        socket.removeListener('connect', onConnect);
        callback(err, socket);
      };
      socket.once('connect', onConnect);
      socket.once('error', onError);
    }
    return socket;
  }
}

/** The peer reference for a request: URL hostname, or `options.host`. */
function hostOf(options: http.ClientRequestArgs): string {
  // ClientRequest normalises `options.host = hostname || host || 'localhost'`
  // before the agent sees it; prefer `hostname` explicitly all the same.
  const host = options.hostname ?? options.host;
  if (!host) throw new TypeError('mesh.http: request is missing a host (peer reference)');
  return host;
}

function portOf(options: http.ClientRequestArgs): number {
  const port = typeof options.port === 'string' ? Number(options.port) : options.port;
  return port && Number.isFinite(port) ? port : 80;
}

/** Options for {@link TruffleHttp.createServer} (RFC 023 §6.1). */
export interface MeshHttpServerOptions extends http.ServerOptions {
  /**
   * Terminate TLS in the sidecar with automatic MagicDNS certificates.
   * Requires MagicDNS + HTTPS certificates enabled on the tailnet; the
   * first handshake per name does ACME issuance (the mesh pre-warms it at
   * listen time). Inbound sockets carry `encrypted: true`, so Express-style
   * `req.secure` / `req.protocol === 'https'` are correct. Default false:
   * WireGuard already encrypts peer-to-peer traffic — TLS is for browser
   * consumers (padlock, secure-context APIs, wss:).
   */
  tls?: boolean;
}

/**
 * A real `node:http.Server` whose `listen()`/`close()`/`address()` drive a
 * mesh listener instead of a host socket. `instanceof http.Server` holds, so
 * Express, Koa, socket.io and `ws` attach unmodified; `req.socket` is a
 * {@link TruffleSocket} (with `remotePeer`/`remotePeerId` identity).
 */
export type MeshHttpServer = http.Server & {
  /** Bound mesh port; set once `'listening'` fires (resolved when 0 was requested). */
  port?: number;
};

/**
 * Build a mesh-listening `http.Server` (the shared engine behind
 * `mesh.http.createServer` and `mesh.ws.createServer`).
 *
 * Contract differences from a host-bound server, all inherent to the mesh:
 * `listen()` accepts only a port (no host/backlog/ipc path — the mesh has
 * exactly one address); idle keep-alive sockets are not reaped on a timer
 * (see TruffleSocket.setTimeout) — `close()` sweeps them via
 * `closeIdleConnections()`, and `closeAllConnections()` works because the
 * server tracks every socket fed to it; `'close'` fires when the mesh
 * listener is released (in-flight responses run to completion, as with
 * `closeIdleConnections` semantics).
 */
export function createMeshHttpServer(
  net: TruffleNet,
  options: MeshHttpServerOptions = {},
  requestListener?: http.RequestListener,
): MeshHttpServer {
  const { tls, ...serverOptions } = options;
  const server = (
    requestListener
      ? http.createServer(serverOptions, requestListener)
      : http.createServer(serverOptions)
  ) as MeshHttpServer;

  const onConnection = (socket: TruffleSocket) => server.emit('connection', socket);
  // Plain single-arg form when tls is off, so the namespace stays usable
  // against pre-RFC-023 net implementations (and their test mocks).
  const meshServer = tls ? net.createServer({ tls }, onConnection) : net.createServer(onConnection);

  meshServer.on('error', (err) => server.emit('error', err));

  let meshListening = false;
  meshServer.on('listening', () => {
    meshListening = true;
    server.port = meshServer.port;
    server.emit('listening');
  });
  meshServer.on('close', () => {
    meshListening = false;
  });

  // net.Server#listening is a prototype getter over the libuv handle; give
  // this instance one that reflects the mesh listener instead.
  Object.defineProperty(server, 'listening', {
    configurable: true,
    get: () => meshListening,
  });

  // Mesh listeners have no local IP/family — address() is `{ port } | null`
  // (same contract as TruffleServer.address), narrower than AddressInfo.
  server.address = (() => meshServer.address()) as MeshHttpServer['address'];

  server.listen = ((...args: unknown[]) => {
    let port: number | undefined;
    let cb: (() => void) | undefined;
    for (const arg of args) {
      if (typeof arg === 'function') cb = arg as () => void;
      else if (typeof arg === 'number' || typeof arg === 'string') port = Number(arg);
      else if (arg && typeof arg === 'object') {
        const p = (arg as { port?: number | string }).port;
        if (p !== undefined) port = Number(p);
        const host = (arg as { host?: string }).host;
        if (host !== undefined) {
          throw new TypeError(
            'mesh http server: listen() takes no host — the mesh has exactly one address (see mesh.dnsName)',
          );
        }
      }
    }
    port ??= 0;
    if (!Number.isInteger(port) || port < 0 || port > 65535) {
      throw new TypeError(`mesh http server: invalid port ${String(port)}`);
    }
    if (cb) server.once('listening', cb);
    meshServer.listen(port);
    return server;
  }) as MeshHttpServer['listen'];

  let closeEmitted = false;
  server.close = ((callback?: (err?: Error) => void) => {
    // Reap idle keep-alive sockets (no timer does it on the mesh); in-flight
    // responses finish naturally — closeAllConnections() is the hard stop.
    server.closeIdleConnections();
    meshServer.close(() => {
      if (!closeEmitted) {
        closeEmitted = true;
        server.emit('close');
      }
      callback?.();
    });
    return server;
  }) as MeshHttpServer['close'];

  return server;
}

/** node:http-shaped namespace bound to a mesh node's `net` namespace. */
export interface TruffleHttp {
  /**
   * Shared MeshAgent instance. Pass as `{ agent: mesh.http.agent }` to a raw
   * `node:http` / `node:https`-less request to route it over the mesh.
   */
  agent: MeshAgent;
  /** The MeshAgent class, for constructing an agent with custom options. */
  Agent: typeof MeshAgent;
  /**
   * `http.request` with the mesh agent injected. URL form:
   * `http://<peer>:<port>/path` (Tailscale IPs work as hostnames). Device
   * names containing spaces aren't valid URL hostnames — use the options-
   * object form, which takes any peer reference via `host`:
   * `mesh.http.request({ host: 'living room pi', port: 8080, path: '/x' })`.
   * A user-supplied `agent` (including `agent: false`) wins over the default.
   */
  request(
    options: http.RequestOptions | string | URL,
    callback?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest;
  request(
    url: string | URL,
    options: http.RequestOptions,
    callback?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest;
  /** As {@link request}, but calls `req.end()` for you (http.get shape). */
  get(
    options: http.RequestOptions | string | URL,
    callback?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest;
  get(
    url: string | URL,
    options: http.RequestOptions,
    callback?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest;
  /** GET a path on a peer and buffer the response body as UTF-8 text. */
  fetchText(
    peer: string,
    port: number,
    path?: string,
  ): Promise<{ status: number; headers: http.IncomingHttpHeaders; body: string }>;
  /**
   * Drop-in `http.createServer` that serves to the tailnet (RFC 023 §6.1):
   * a real `http.Server` whose `listen(port)` binds a mesh listener, so any
   * Tailscale device — browsers included — can reach it, and `req.socket`
   * carries WhoIs identity (`remotePeer`, `remotePeerId`, `remotePeerName`).
   *
   * ```ts
   * const server = mesh.http.createServer({ tls: true }, app); // app = Express
   * server.listen(443, () => console.log(`https://${mesh.dnsName}/`));
   * ```
   */
  createServer(requestListener?: http.RequestListener): MeshHttpServer;
  createServer(
    options: MeshHttpServerOptions,
    requestListener?: http.RequestListener,
  ): MeshHttpServer;
}

/** The two-signature call shape shared by `http.request` and `http.get`. */
type HttpDispatch = {
  (
    options: http.RequestOptions | string | URL,
    callback?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest;
  (
    url: string | URL,
    options: http.RequestOptions,
    callback?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest;
};

export function createHttpNamespace(net: TruffleNet): TruffleHttp {
  const agent = new MeshAgent(net);

  // Inject the mesh agent unless the caller supplied their own (or `false`).
  function withAgent(options?: http.RequestOptions): http.RequestOptions {
    return { ...options, agent: options?.agent ?? agent };
  }

  // Normalise node:http's overloads and route through `fn` (request or get).
  function dispatch(
    fn: HttpDispatch,
    arg1: http.RequestOptions | string | URL,
    arg2?: http.RequestOptions | ((res: http.IncomingMessage) => void),
    arg3?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest {
    if (typeof arg2 === 'function' || arg2 === undefined) {
      // (url|options, callback?) form.
      if (typeof arg1 === 'string' || arg1 instanceof URL) {
        return fn(arg1, withAgent(), arg2);
      }
      return fn(withAgent(arg1), arg2);
    }
    // (url, options, callback?) form.
    return fn(arg1 as string | URL, withAgent(arg2), arg3);
  }

  function request(
    options: http.RequestOptions | string | URL,
    callback?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest;
  function request(
    url: string | URL,
    options: http.RequestOptions,
    callback?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest;
  function request(
    arg1: http.RequestOptions | string | URL,
    arg2?: http.RequestOptions | ((res: http.IncomingMessage) => void),
    arg3?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest {
    return dispatch(http.request, arg1, arg2, arg3);
  }

  function get(
    options: http.RequestOptions | string | URL,
    callback?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest;
  function get(
    url: string | URL,
    options: http.RequestOptions,
    callback?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest;
  function get(
    arg1: http.RequestOptions | string | URL,
    arg2?: http.RequestOptions | ((res: http.IncomingMessage) => void),
    arg3?: (res: http.IncomingMessage) => void,
  ): http.ClientRequest {
    return dispatch(http.get, arg1, arg2, arg3);
  }

  function fetchText(
    peer: string,
    port: number,
    path = '/',
  ): Promise<{ status: number; headers: http.IncomingHttpHeaders; body: string }> {
    return new Promise((resolve, reject) => {
      const req = http.request({ host: peer, port, path, method: 'GET', agent }, (res) => {
        const chunks: Buffer[] = [];
        res.on('data', (chunk: Buffer) => chunks.push(chunk));
        res.on('end', () =>
          resolve({
            status: res.statusCode ?? 0,
            headers: res.headers,
            body: Buffer.concat(chunks).toString('utf8'),
          }),
        );
        res.on('error', reject);
      });
      req.on('error', reject);
      req.end();
    });
  }

  function createServer(requestListener?: http.RequestListener): MeshHttpServer;
  function createServer(
    options: MeshHttpServerOptions,
    requestListener?: http.RequestListener,
  ): MeshHttpServer;
  function createServer(
    arg1?: MeshHttpServerOptions | http.RequestListener,
    arg2?: http.RequestListener,
  ): MeshHttpServer {
    const options = typeof arg1 === 'object' && arg1 !== null ? arg1 : {};
    const listener = typeof arg1 === 'function' ? arg1 : arg2;
    return createMeshHttpServer(net, options, listener);
  }

  return { agent, Agent: MeshAgent, request, get, fetchText, createServer };
}
