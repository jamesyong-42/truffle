// node:net-shaped API over truffle's raw TCP transport (RFC 021 Phase 2).
//
// TruffleSocket is a real stream.Duplex, so everything that accepts a
// Node socket-ish duplex works over the mesh: piping,
// `httpServer.emit('connection', socket)`, `http.Agent#createConnection`,
// the `ws` package via a custom agent, undici's `connect`, etc.
//
// Backpressure comes from the pull-model native handle: `_read` awaits one
// native `read()` at a time, `_write` resolves when the transport accepted
// the bytes.

import { Duplex } from 'node:stream';
import { EventEmitter } from 'node:events';
import type { NapiNode, NapiTcpListener, NapiTcpSocket } from '@vibecook/truffle-native';
import { peerLikeToQuery, type Peer, type PeerLike } from './peer.js';

/**
 * Hooks threaded from `createMeshNode` into the namespaces (RFC 023 §6.1).
 * Namespaces stay constructible without them (tests, low-level use); the
 * Peer-handle surfaces just return null then.
 */
export interface NetHooks {
  /**
   * Resolve a Tailscale node id to the interned RFC 022 `Peer` handle.
   * Wired to the mesh node's peer registry by `createMeshNode`.
   */
  resolvePeer?: (tailscaleId: string) => Peer | null;
}

/** Options for {@link TruffleNet.createServer}. */
export interface NetServerOptions {
  /**
   * Terminate TLS in the sidecar with automatic MagicDNS certificates
   * (RFC 023 §6.1). Requires MagicDNS + HTTPS enabled on the tailnet.
   * Inbound sockets carry `encrypted: true` so http frameworks report
   * `req.secure` correctly. Default false — WireGuard already encrypts
   * peer-to-peer traffic; TLS is for browser consumers.
   */
  tls?: boolean;
}

export interface NetConnectOptions {
  /**
   * Peer to connect to: {@link PeerLike} handle, or a query string
   * (device id / prefix, name, hostname, or Tailscale IP).
   * Prefer a Peer handle from `getPeers()` / events when available.
   */
  host?: PeerLike;
  /**
   * Alias of `host` for RFC 022 handle-first call sites:
   * `mesh.net.connect({ peer, port })`.
   */
  peer?: PeerLike;
  /** Port on the peer. */
  port: number;
}

export type ConnectionListener = (socket: TruffleSocket) => void;

/**
 * A TCP connection over the mesh, as a standard `stream.Duplex`.
 *
 * Mirrors `net.Socket` semantics where it matters: `'connect'`/`'ready'`
 * on establishment, `end()` half-closes (FIN) while reads continue,
 * `'end'` on peer EOF, `destroy()` tears down both directions.
 */
export class TruffleSocket extends Duplex {
  #native: NapiTcpSocket | null = null;
  #ready: Promise<NapiTcpSocket>;
  #reading = false;
  #resolvePeer?: NetHooks['resolvePeer'];

  /** Logical remote address (`host:port`); set once connected. */
  remoteAddress?: string;
  /** Stable peer id (resolved device id, or WhoIs node id inbound) when known. */
  remotePeerId?: string;
  /** Human-readable peer name from the WhoIs identity (inbound sockets). */
  remotePeerName?: string;
  /**
   * True on sockets accepted by a `tls: true` mesh listener (TLS terminated
   * in the sidecar). This is the undocumented-but-universal property http
   * frameworks sniff for `req.secure` / `req.protocol === 'https'`.
   */
  encrypted?: boolean;

  constructor(native: NapiTcpSocket | Promise<NapiTcpSocket>, hooks?: NetHooks) {
    super({ allowHalfOpen: true });
    this.#resolvePeer = hooks?.resolvePeer;
    const adopt = (sock: NapiTcpSocket): NapiTcpSocket => {
      this.#native = sock;
      this.remoteAddress = sock.remoteAddress();
      this.remotePeerId = sock.remotePeerId() ?? undefined;
      this.remotePeerName = sock.remotePeerName() ?? undefined;
      return sock;
    };
    if (typeof (native as PromiseLike<NapiTcpSocket>).then === 'function') {
      // Outbound: the dial is in flight; metadata lands on 'connect'.
      this.#ready = Promise.resolve(native).then((sock) => {
        adopt(sock);
        this.emit('connect');
        this.emit('ready');
        return sock;
      });
      // A failed dial destroys the socket with the error, like net.Socket.
      this.#ready.catch((err) => this.destroy(err as Error));
    } else {
      // Inbound (accept-path) sockets are already connected: set the peer
      // metadata synchronously so a 'connection' handler can read
      // remotePeerId/remotePeerName immediately for accept-time gating.
      // The events still fire asynchronously — constructor-time emits
      // would have no listeners yet.
      const sock = adopt(native as NapiTcpSocket);
      this.#ready = Promise.resolve(sock);
      queueMicrotask(() => {
        if (!this.destroyed) {
          this.emit('connect');
          this.emit('ready');
        }
      });
    }
  }

  override _read(size: number): void {
    if (this.#reading) return;
    this.#reading = true;
    this.#ready
      .then((sock) => sock.read(size > 0 ? size : undefined))
      .then((chunk) => {
        this.#reading = false;
        this.push(chunk);
      })
      .catch((err) => {
        this.#reading = false;
        this.destroy(err as Error);
      });
  }

  override _write(
    chunk: Buffer,
    _encoding: BufferEncoding,
    callback: (error?: Error | null) => void,
  ): void {
    this.#ready.then((sock) => sock.write(chunk).then(() => callback(null), callback), callback);
  }

  override _final(callback: (error?: Error | null) => void): void {
    this.#ready.then((sock) => sock.end().then(() => callback(null), callback), callback);
  }

  override _destroy(error: Error | null, callback: (error: Error | null) => void): void {
    const finish = () => callback(error);
    if (this.#native) {
      this.#native.close().then(finish, finish);
    } else {
      // Dial still in flight — close the socket when (if) it lands.
      this.#ready.then((sock) => sock.close()).catch(() => {});
      finish();
    }
  }

  // ─── net.Socket compat shims (RFC 021) ────────────────────────────────
  // node:http's client and server hand their sockets a few net.Socket-only
  // methods (setTimeout/setNoDelay/setKeepAlive/ref/unref/address). A plain
  // Duplex doesn't have them, so without these `http.request(...)` and
  // `httpServer.emit('connection', socket)` throw on the missing method.
  // The mesh data path is a loopback hop with no Nagle, no SO_KEEPALIVE and
  // no local IP/port, so these are well-behaved no-ops that return `this`
  // (net.Socket's chainable contract) — enough to satisfy node:http.

  /** net.Socket#timeout, as recorded by {@link setTimeout}. */
  timeout = 0;

  /**
   * net.Socket#setTimeout. Records the value and, like net.Socket, wires an
   * optional one-shot `'timeout'` listener (and clears it when `msecs` is 0).
   *
   * Caveat: the mesh bridge doesn't surface socket idle activity to JS
   * without forcing the readable side into flowing mode (which would defeat
   * the pull-model backpressure), so no idle timer is armed and `'timeout'`
   * never fires on its own — enforce inactivity limits at the app layer if
   * you need them. http's default server timeout is 0 (disabled) and the
   * default agent is `keepAlive: false`, so this is inert on the common
   * paths and exists so calling code doesn't throw. Never auto-destroys the
   * socket, matching net.Socket.
   */
  setTimeout(msecs: number, callback?: () => void): this {
    this.timeout = msecs;
    if (callback) {
      if (msecs === 0) this.removeListener('timeout', callback);
      else this.once('timeout', callback);
    }
    return this;
  }

  /** net.Socket#setNoDelay — no-op: Nagle doesn't apply to the mesh bridge. */
  setNoDelay(_noDelay?: boolean): this {
    return this;
  }

  /** net.Socket#setKeepAlive — no-op: the bridge has no TCP keepalive to set. */
  setKeepAlive(_enable?: boolean, _initialDelay?: number): this {
    return this;
  }

  /** net.Socket#ref — no-op: no libuv handle backs a mesh socket. */
  ref(): this {
    return this;
  }

  /** net.Socket#unref — no-op (see {@link ref}). */
  unref(): this {
    return this;
  }

  /** net.Socket#address — mesh sockets have no local IP/port; returns `{}`. */
  address(): { address?: string; family?: string; port?: number } {
    return {};
  }

  /**
   * The interned RFC 022 `Peer` handle for the remote end, resolved live from
   * the mesh node's registry by this socket's `remotePeerId`. Null when the
   * id is not yet known (dial in flight), the peer isn't interned, or the
   * namespace was built without registry hooks (outside `createMeshNode`).
   * Inbound sockets have it from accept time — use it for gating:
   * `req.socket.remotePeer?.displayName`.
   */
  get remotePeer(): Peer | null {
    if (!this.remotePeerId || !this.#resolvePeer) return null;
    return this.#resolvePeer(this.remotePeerId) ?? null;
  }
}

/**
 * A TCP server on the mesh, mimicking `net.Server`.
 *
 * Events: `'listening'`, `'connection'` ([`TruffleSocket`]), `'close'`,
 * `'error'`. To serve HTTP over the mesh:
 *
 * ```ts
 * const httpServer = http.createServer(app);
 * mesh.net
 *   .createServer((socket) => httpServer.emit('connection', socket))
 *   .listen(8080);
 * ```
 */
export class TruffleServer extends EventEmitter {
  #node: NapiNode;
  #listener: NapiTcpListener | null = null;
  #closed = false;
  #closeEmitted = false;
  #hooks?: NetHooks;
  #tls: boolean;

  /** Bound port; set once `'listening'` fires (resolved when 0 was requested). */
  port?: number;

  constructor(
    node: NapiNode,
    connectionListener?: ConnectionListener,
    options?: NetServerOptions & { hooks?: NetHooks },
  ) {
    super();
    this.#node = node;
    this.#hooks = options?.hooks;
    this.#tls = options?.tls ?? false;
    if (connectionListener) this.on('connection', connectionListener);
  }

  /**
   * Start listening on `port` (0 = ephemeral; read `server.port` after
   * `'listening'`). Returns `this`, like `net.Server#listen`.
   */
  listen(port: number, listeningListener?: () => void): this {
    if (this.#listener) throw new Error('listen() already called');
    if (listeningListener) this.once('listening', listeningListener);
    // The tls parameter is the RFC 023 P2 native signature; pre-P2 natives
    // (and test mocks written against the one-arg form) ignore extra args.
    const listenTcp = this.#node.listenTcp.bind(this.#node) as (
      port: number,
      tls?: boolean,
    ) => Promise<NapiTcpListener>;
    listenTcp(port, this.#tls ? true : undefined)
      .then((listener) => {
        if (this.#closed) {
          void listener.close();
          return;
        }
        this.#listener = listener;
        this.port = listener.port();
        this.emit('listening');
        void this.#acceptLoop(listener);
      })
      .catch((err) => this.emit('error', err));
    return this;
  }

  async #acceptLoop(listener: NapiTcpListener): Promise<void> {
    try {
      for (;;) {
        const native = await listener.accept();
        if (native === null) break;
        const socket = new TruffleSocket(native, this.#hooks);
        // TLS terminated in the sidecar → mark the socket the way frameworks
        // sniff it (req.secure); see TruffleSocket.encrypted.
        if (this.#tls) socket.encrypted = true;
        this.emit('connection', socket);
      }
    } catch (err) {
      if (!this.#closed) this.emit('error', err);
    }
    this.#emitClose();
  }

  #emitClose(): void {
    if (this.#closeEmitted) return;
    this.#closeEmitted = true;
    this.emit('close');
  }

  /** `net.Server#address` compat (port only — mesh listeners have no local IP). */
  address(): { port: number } | null {
    return this.port === undefined ? null : { port: this.port };
  }

  /**
   * Stop accepting connections and release the port. `callback` fires on
   * `'close'` — including when the server already closed (e.g. the accept
   * loop ended on its own after a mesh teardown), like `net.Server#close`.
   */
  close(callback?: () => void): this {
    if (callback) {
      if (this.#closeEmitted) queueMicrotask(callback);
      else this.once('close', callback);
    }
    if (this.#closed) return this;
    this.#closed = true;
    const listener = this.#listener;
    this.#listener = null;
    if (listener) {
      // accept() resolves null → the loop exits and emits 'close' (unless
      // the loop already ended on its own — then 'close' already fired and
      // this just releases the native listener).
      void listener.close();
    } else {
      this.#emitClose();
    }
    return this;
  }
}

/** node:net-shaped namespace bound to a mesh node. */
export interface TruffleNet {
  /** Open a connection to a peer. Returns the socket immediately; it emits `'connect'`. */
  connect(options: NetConnectOptions): TruffleSocket;
  connect(port: number, host: PeerLike): TruffleSocket;
  /** Alias of `connect`, mirroring `net.createConnection`. */
  createConnection(options: NetConnectOptions): TruffleSocket;
  createConnection(port: number, host: PeerLike): TruffleSocket;
  /** Create a server; call `.listen(port)` to bind, like `net.createServer`. */
  createServer(connectionListener?: ConnectionListener): TruffleServer;
  createServer(options: NetServerOptions, connectionListener?: ConnectionListener): TruffleServer;
}

function resolveConnectTarget(options: NetConnectOptions): string {
  const target = options.peer ?? options.host;
  if (target === undefined || target === '') {
    throw new TypeError('connect: peer/host is required');
  }
  return peerLikeToQuery(target);
}

export function createNetNamespace(node: NapiNode, hooks?: NetHooks): TruffleNet {
  function connect(options: NetConnectOptions): TruffleSocket;
  function connect(port: number, host: PeerLike): TruffleSocket;
  function connect(optionsOrPort: NetConnectOptions | number, maybeHost?: PeerLike): TruffleSocket {
    const opts: NetConnectOptions =
      typeof optionsOrPort === 'number' ? { host: maybeHost, port: optionsOrPort } : optionsOrPort;
    const host = resolveConnectTarget(opts);
    return new TruffleSocket(node.openTcp(host, opts.port), hooks);
  }

  function createServer(connectionListener?: ConnectionListener): TruffleServer;
  function createServer(
    options: NetServerOptions,
    connectionListener?: ConnectionListener,
  ): TruffleServer;
  function createServer(
    arg1?: ConnectionListener | NetServerOptions,
    arg2?: ConnectionListener,
  ): TruffleServer {
    const options = typeof arg1 === 'object' && arg1 !== null ? arg1 : {};
    const listener = typeof arg1 === 'function' ? arg1 : arg2;
    return new TruffleServer(node, listener, { ...options, hooks });
  }

  return {
    connect,
    createConnection: connect,
    createServer,
  };
}
