// node:dgram-shaped API over truffle's raw UDP transport (RFC 021 Phase 3).
//
// TruffleDgramSocket mirrors `dgram.Socket` closely enough that existing
// node:dgram code ports by swapping the module: `createSocket()`, `bind()`,
// `send()`, `close()`, and the `'listening'` / `'message'` / `'error'` /
// `'close'` events all behave as a Node developer expects.
//
// Two deliberate deviations from node:dgram, both documented at their call
// sites: (1) `bind()` returns a Promise<this> (node:dgram returns the socket
// synchronously and signals readiness only via `'listening'`); the event
// still fires, so callback-style code keeps working. (2) `send()` before
// `bind()` throws instead of auto-binding — auto-bind would silently pick an
// ephemeral relay port, and explicitness beats a surprise here.
//
// Datagram semantics are preserved by the receive pump: it awaits one
// `recv()` at a time, so a slow `'message'` handler backpressures the read
// loop and the relay/OS buffer drops the overflow — correct UDP behaviour
// with bounded memory, never an unbounded in-process queue.

import { EventEmitter } from 'node:events';
import type { NapiDatagram, NapiNode, NapiUdpSocket } from '@vibecook/truffle-native';

/**
 * Minimum spacing between `getPeers()` calls made to enrich datagram
 * `rinfo` with peer identity. A cache miss from an unknown sender triggers
 * at most one refresh per window, so a burst of datagrams from unmapped IPs
 * can't turn into a `getPeers()` storm.
 */
const PEER_CACHE_TTL_MS = 5_000;

/**
 * Sender metadata delivered with each `'message'` event, mirroring
 * node:dgram's `RemoteInfo` and extending it with mesh peer identity.
 *
 * `address`/`port` are the datagram's WireGuard-authenticated tailnet
 * source (`100.x` IPv4). `peerId`/`peerName` are best-effort enrichments
 * resolved from the local netmap — they may be `undefined` for a sender
 * that isn't a known peer yet, and message delivery never blocks on them.
 */
export interface TruffleRemoteInfo {
  /** Sender's tailnet IP (`100.x.x.x`). */
  address: string;
  /** Sender's source port. */
  port: number;
  /** Always `'IPv4'`: datagram addressing is IPv4-only in v1 (RFC 021 §3). */
  family: 'IPv4';
  /** Datagram payload length in bytes. */
  size: number;
  /** Sender's stable device id, when the source IP maps to a known peer. */
  peerId?: string;
  /** Sender's human-readable device name, when known. */
  peerName?: string;
}

/**
 * A UDP socket over the mesh, mimicking `dgram.Socket`.
 *
 * Events:
 * - `'listening'` — after {@link bind} resolves; `this.port` is set.
 * - `'message'` — `(msg: Buffer, rinfo: TruffleRemoteInfo)` per datagram.
 * - `'error'` — a terminal receive error, or a {@link send} failure with no
 *   callback (node:dgram contract).
 * - `'close'` — the socket has shut down (via {@link close} or a closed
 *   relay); fires exactly once.
 *
 * ```ts
 * const sock = mesh.dgram.createSocket();
 * sock.on('message', (msg, rinfo) => {
 *   console.log(`${rinfo.peerName ?? rinfo.address}: ${msg}`);
 * });
 * await sock.bind(8081);
 * sock.send(Buffer.from('ping'), 8081, 'other-machine');
 * ```
 */
export class TruffleDgramSocket extends EventEmitter {
  #node: NapiNode;
  #native: NapiUdpSocket | null = null;
  #binding = false;
  #closed = false;
  #closeEmitted = false;

  // Lazy ip→peer map for rinfo enrichment, rebuilt wholesale on refresh so
  // departed peers don't linger. Refreshes are rate-limited (see below).
  #peerCache = new Map<string, { peerId: string; peerName: string }>();
  #peerCacheRefreshedAt = 0;

  /**
   * The mesh port this socket is bound to; `0` until {@link bind} resolves.
   * Note it reflects the *requested* tsnet port — an ephemeral client
   * socket bound with port `0` keeps `0` here (its real relay port isn't
   * surfaced), so read `port` only after `'listening'` and only when you
   * bound an explicit port.
   */
  port = 0;

  constructor(node: NapiNode) {
    super();
    this.#node = node;
  }

  /**
   * Bind the socket to `port` (default `0` = ephemeral relay port) and start
   * receiving. Resolves with the socket once bound; `'listening'` fires at
   * the same point, and `callback` (if given) is invoked as a one-shot
   * `'listening'` listener — so both `await sock.bind(port)` and
   * `sock.bind(port, () => …)` work.
   *
   * Deviates from node:dgram, which returns the socket synchronously and
   * signals readiness only via the event; here bind is genuinely async
   * (it round-trips to the sidecar), so the promise is the primary channel.
   * Failure semantics follow the calling style: promise style (`await
   * sock.bind(p)`) **rejects**; callback style (`sock.bind(p, cb)`,
   * typically fire-and-forget) emits **`'error'`** like node:dgram and
   * never leaves an unhandled rejection.
   */
  bind(port = 0, callback?: () => void): Promise<this> {
    const attempt = this.#bind(port);
    if (!callback) return attempt;

    this.once('listening', callback);
    return attempt.catch((err) => {
      this.removeListener('listening', callback);
      this.emit('error', err as Error);
      return this;
    });
  }

  async #bind(port: number): Promise<this> {
    if (this.#closed) throw new Error('dgram: socket is closed');
    if (this.#native || this.#binding) throw new Error('dgram: bind() already called');

    this.#binding = true;
    let native: NapiUdpSocket;
    try {
      native = await this.#node.bindUdp(port);
    } finally {
      this.#binding = false;
    }

    // close() may have raced in while bindUdp was in flight; honour it and
    // don't start the pump or announce 'listening'.
    if (this.#closed) {
      native.close();
      return this;
    }

    this.#native = native;
    this.port = native.port();
    this.emit('listening');
    void this.#recvPump(native);
    return this;
  }

  /**
   * Send a datagram to `address:port`. `address` accepts any peer reference
   * (device id or ≥4-char prefix, device name, Tailscale hostname, or `100.x`
   * IP), matching node:dgram's `(msg, port, address, callback?)` arg order.
   *
   * Must be called after {@link bind}: unlike node:dgram this does **not**
   * auto-bind (that would silently pick an ephemeral port), so sending before
   * bind throws synchronously. Async send failures go to `callback` if given,
   * otherwise the `'error'` event (node:dgram contract).
   */
  send(
    msg: Buffer | string | Uint8Array,
    port: number,
    address: string,
    callback?: (error: Error | null) => void,
  ): void {
    const native = this.#native;
    if (!native) {
      throw new Error('dgram: call bind() before send() (auto-bind is not supported)');
    }
    const data =
      typeof msg === 'string' ? Buffer.from(msg) : Buffer.isBuffer(msg) ? msg : Buffer.from(msg);
    native.send(data, address, port).then(
      () => callback?.(null),
      (err) => this.#failSend(err as Error, callback),
    );
  }

  /**
   * Close the socket. In-flight `recv()` resolves `null`, ending the receive
   * pump and emitting `'close'` once. Idempotent; `callback` (if given) runs
   * on `'close'`, even if the socket has already closed.
   */
  close(callback?: () => void): void {
    if (callback) {
      if (this.#closeEmitted) queueMicrotask(callback);
      else this.once('close', callback);
    }
    if (this.#closed) return;
    this.#closed = true;

    const native = this.#native;
    if (native) {
      // recv() resolves null → the pump exits and emits 'close'.
      native.close();
    } else {
      // Never bound (or bind still in flight): no pump to unwind.
      this.#emitClose();
    }
  }

  /**
   * node:dgram `address()` compat. Mesh sockets have no local IP, so
   * `address` is empty; `port` is the bound port (see the {@link port} note).
   */
  address(): { address: string; family: 'IPv4'; port: number } {
    return { address: '', family: 'IPv4', port: this.port };
  }

  // ─── internals ─────────────────────────────────────────────────────────

  async #recvPump(native: NapiUdpSocket): Promise<void> {
    try {
      for (;;) {
        const datagram = await native.recv();
        if (datagram === null) break; // socket closed
        // Enrichment may await one getPeers() on a cold-cache miss (rate-
        // limited); it's the only place the pump awaits beyond recv itself.
        const rinfo = await this.#rinfoFor(datagram);
        this.emit('message', datagram.data, rinfo);
      }
    } catch (err) {
      // A recv error on this socket is terminal — surface it and stop (unless
      // we're already tearing down, where the null-on-close path handles it).
      if (!this.#closed) this.emit('error', err as Error);
    }
    this.#emitClose();
  }

  async #rinfoFor(datagram: NapiDatagram): Promise<TruffleRemoteInfo> {
    let peer = this.#peerCache.get(datagram.address);
    if (!peer) {
      await this.#maybeRefreshPeerCache();
      peer = this.#peerCache.get(datagram.address);
    }
    return {
      address: datagram.address,
      port: datagram.port,
      family: 'IPv4',
      size: datagram.data.length,
      peerId: peer?.peerId,
      peerName: peer?.peerName,
    };
  }

  /**
   * Rebuild the ip→peer cache from `getPeers()`, at most once per
   * {@link PEER_CACHE_TTL_MS}. Best-effort: a failed lookup leaves the prior
   * cache in place and never propagates, so enrichment never blocks or breaks
   * message delivery. The pump is sequential, so no in-flight guard is needed.
   */
  async #maybeRefreshPeerCache(): Promise<void> {
    const now = Date.now();
    if (now - this.#peerCacheRefreshedAt < PEER_CACHE_TTL_MS) return;
    this.#peerCacheRefreshedAt = now; // rate-limit even if the call fails
    try {
      const peers = await this.#node.getPeers();
      const next = new Map<string, { peerId: string; peerName: string }>();
      for (const peer of peers) {
        if (peer.ip) next.set(peer.ip, { peerId: peer.deviceId, peerName: peer.deviceName });
      }
      this.#peerCache = next;
    } catch {
      // Enrichment is best-effort; keep the prior cache.
    }
  }

  #failSend(err: Error, callback?: (error: Error | null) => void): void {
    if (callback) callback(err);
    else this.emit('error', err);
  }

  #emitClose(): void {
    if (this.#closeEmitted) return;
    this.#closeEmitted = true;
    this.emit('close');
  }
}

/** node:dgram-shaped namespace bound to a mesh node. */
export interface TruffleDgram {
  /** Create an unbound UDP socket, like `dgram.createSocket('udp4')`. */
  createSocket(): TruffleDgramSocket;
}

export function createDgramNamespace(node: NapiNode): TruffleDgram {
  return {
    createSocket: () => new TruffleDgramSocket(node),
  };
}
