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

export interface NetConnectOptions {
  /**
   * Peer to connect to: device id (or unique ≥4-char prefix), device
   * name, Tailscale hostname, or Tailscale IP.
   */
  host: string;
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

  /** Logical remote address (`host:port`); set once connected. */
  remoteAddress?: string;
  /** Stable peer id (resolved device id, or WhoIs node id inbound) when known. */
  remotePeerId?: string;
  /** Human-readable peer name from the WhoIs identity (inbound sockets). */
  remotePeerName?: string;

  constructor(native: NapiTcpSocket | Promise<NapiTcpSocket>) {
    super({ allowHalfOpen: true });
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

  /** Bound port; set once `'listening'` fires (resolved when 0 was requested). */
  port?: number;

  constructor(node: NapiNode, connectionListener?: ConnectionListener) {
    super();
    this.#node = node;
    if (connectionListener) this.on('connection', connectionListener);
  }

  /**
   * Start listening on `port` (0 = ephemeral; read `server.port` after
   * `'listening'`). Returns `this`, like `net.Server#listen`.
   */
  listen(port: number, listeningListener?: () => void): this {
    if (this.#listener) throw new Error('listen() already called');
    if (listeningListener) this.once('listening', listeningListener);
    this.#node
      .listenTcp(port)
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
        this.emit('connection', new TruffleSocket(native));
      }
    } catch (err) {
      if (!this.#closed) this.emit('error', err);
    }
    this.emit('close');
  }

  /** `net.Server#address` compat (port only — mesh listeners have no local IP). */
  address(): { port: number } | null {
    return this.port === undefined ? null : { port: this.port };
  }

  /** Stop accepting connections and release the port. */
  close(callback?: () => void): this {
    if (callback) this.once('close', callback);
    if (this.#closed) return this;
    this.#closed = true;
    const listener = this.#listener;
    this.#listener = null;
    if (listener) {
      // accept() resolves null → the loop exits and emits 'close'.
      void listener.close();
    } else {
      this.emit('close');
    }
    return this;
  }
}

/** node:net-shaped namespace bound to a mesh node. */
export interface TruffleNet {
  /** Open a connection to a peer. Returns the socket immediately; it emits `'connect'`. */
  connect(options: NetConnectOptions): TruffleSocket;
  connect(port: number, host: string): TruffleSocket;
  /** Alias of `connect`, mirroring `net.createConnection`. */
  createConnection(options: NetConnectOptions): TruffleSocket;
  createConnection(port: number, host: string): TruffleSocket;
  /** Create a server; call `.listen(port)` to bind, like `net.createServer`. */
  createServer(connectionListener?: ConnectionListener): TruffleServer;
}

export function createNetNamespace(node: NapiNode): TruffleNet {
  function connect(options: NetConnectOptions): TruffleSocket;
  function connect(port: number, host: string): TruffleSocket;
  function connect(optionsOrPort: NetConnectOptions | number, maybeHost?: string): TruffleSocket {
    const { host, port } =
      typeof optionsOrPort === 'number'
        ? { host: maybeHost ?? '', port: optionsOrPort }
        : optionsOrPort;
    if (!host) throw new TypeError('connect: host (peer) is required');
    return new TruffleSocket(node.openTcp(host, port));
  }

  return {
    connect,
    createConnection: connect,
    createServer: (connectionListener?: ConnectionListener) =>
      new TruffleServer(node, connectionListener),
  };
}
