// node:http interop for the mesh (RFC 021 Phase 2).
//
// `mesh.http` lets existing node:http / Express code talk to peers over the
// mesh with a one-line change. Two directions:
//
//   • Outbound: point a request at a peer through a MeshAgent
//       mesh.http.get('http://100.64.0.7:8080/api/status', (res) => …)
//       await mesh.http.fetchText('kitchen-pi', 8080, '/api/status')
//   • Inbound: feed mesh TCP connections into a stock http.Server
//       const httpServer = http.createServer(app);        // e.g. Express
//       mesh.net
//         .createServer((socket) => httpServer.emit('connection', socket))
//         .listen(8080);
//
// Nothing here is mesh-specific beyond the Agent seam: `http.Agent#
// createConnection` is documented to accept any `stream.Duplex`, and
// TruffleSocket (see ./net.ts) is one, so node:http drives it exactly like a
// real net.Socket. The compat shims that make that true live in ./net.ts.

import http from 'node:http';
import type { Duplex } from 'node:stream';
import type { TruffleNet } from './net.js';

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

  return { agent, Agent: MeshAgent, request, get, fetchText };
}
