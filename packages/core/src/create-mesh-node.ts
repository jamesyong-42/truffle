import { execFile } from 'node:child_process';
import {
  NapiNode,
  type NapiNodeConfig,
  type NapiNamespacedMessage,
  type NapiPeerEvent,
  type NapiFileTransfer,
  type NapiPingResult,
  type NapiQuicConnection,
  type NapiTcpSocket,
  type NapiTransferResult,
} from '@vibecook/truffle-native';
import { createNetNamespace, type TruffleNet } from './net.js';
import { createHttpNamespace, type TruffleHttp } from './http.js';
import { createQuicNamespace, type TruffleQuic } from './quic.js';
import { createDgramNamespace, type TruffleDgram } from './dgram.js';
import { createWsNamespace, type TruffleWs } from './ws.js';
import { resolveSidecarPath } from './sidecar.js';
import { Peer, PeerRegistry, peerLikeToQuery, type PeerLike, type PeerRef } from './peer.js';

export type { PeerLike, PeerRef };
export { Peer };

/**
 * A started mesh node: native `NapiNode` methods plus RFC 021 namespaces
 * and RFC 022 Peer-handle APIs.
 */
export type MeshNode = Omit<
  NapiNode,
  | 'getPeers'
  | 'peer'
  | 'send'
  | 'ping'
  | 'onPeerChange'
  | 'onMessage'
  | 'openTcp'
  | 'connectQuic'
  | 'fileTransfer'
> & {
  net: TruffleNet;
  http: TruffleHttp;
  quic: TruffleQuic;
  dgram: TruffleDgram;
  ws: TruffleWs;
  native: NapiNode;

  /** Interned Peer handles (`===` stable per peerRef). */
  getPeers(): Promise<Peer[]>;

  /**
   * Resolve a query to an interned Peer.
   * `waitMs` blocks until resolvable or timeout → null.
   */
  peer(query: string, opts?: { waitMs?: number }): Promise<Peer | null>;

  /** Handle-first send (`Peer` or query string). */
  send(to: PeerLike, namespace: string, data: Buffer | Uint8Array): Promise<void>;

  /** Handle-first ping. */
  ping(to: PeerLike): Promise<NapiPingResult>;

  /** Handle-first raw TCP dial (RFC 021, PeerLike per RFC 022 §6.3). */
  openTcp(to: PeerLike, port: number): Promise<NapiTcpSocket>;

  /** Handle-first QUIC dial. */
  connectQuic(to: PeerLike, port: number): Promise<NapiQuicConnection>;

  /** File transfer handle whose peer-taking methods accept PeerLike (RFC 014). */
  fileTransfer(): MeshFileTransfer;

  /**
   * Peer-change subscription with interned `event.peer` when present.
   */
  onPeerChange(callback: (event: MeshPeerEvent) => void): void;

  /**
   * Namespace messages with `from` as an interned Peer when known.
   */
  onMessage(namespace: string, callback: (msg: MeshNamespacedMessage) => void): void;
};

/** Native file-transfer handle with PeerLike parameters (RFC 022 §6.3). */
export type MeshFileTransfer = Omit<NapiFileTransfer, 'sendFile' | 'pullFile'> & {
  sendFile(to: PeerLike, localPath: string, remotePath: string): Promise<NapiTransferResult>;
  pullFile(to: PeerLike, remotePath: string, localPath: string): Promise<NapiTransferResult>;
};

/** Peer event with interned handle (RFC 022). */
export type MeshPeerEvent = {
  type: string;
  /** Tailscale routing key when peer-related; empty for auth. */
  peerId: string;
  /**
   * Interned handle — present for every peer-scoped event, including
   * `left`, where it carries the final offline view (RFC 022 §16.4).
   */
  peer?: Peer;
  authUrl?: string;
};

/** Inbound message; `from` is Peer when the sender is interned. */
export type MeshNamespacedMessage = {
  from: Peer | string;
  namespace: string;
  msgType: string;
  payload: unknown;
  timestamp?: number;
};

export interface CreateMeshNodeOptions {
  /**
   * Required. Application namespace. Format: `^[a-z][a-z0-9-]{1,31}$`
   */
  appId: string;
  deviceName?: string;
  deviceId?: string;
  sidecarPath?: string;
  stateDir?: string;
  authKey?: string;
  ephemeral?: boolean;
  /** WebSocket listener port. Defaults to 9417 when omitted. */
  wsPort?: number;
  /**
   * When true (default), proactively exchange hello with online peers so
   * durable `deviceId` is learned without application `send` (RFC 022 §8).
   */
  eagerIdentity?: boolean;
  autoAuth?: boolean;
  openUrl?: (url: string) => void;
  onAuthRequired?: (url: string) => void;
  onPeerChange?: (event: MeshPeerEvent) => void;
}

function defaultOpenUrl(url: string): void {
  if (!/^https?:\/\//i.test(url)) return;
  const [cmd, args]: [string, string[]] =
    process.platform === 'darwin'
      ? ['open', [url]]
      : process.platform === 'win32'
        ? ['explorer', [url]]
        : ['xdg-open', [url]];
  execFile(cmd, args, () => {});
}

/**
 * Convert a native peer event, maintaining the registry as the single side
 * effect: intern on snapshot-bearing events, retire on `left`.
 * @internal — exported for unit tests only.
 */
export function toMeshPeerEvent(reg: PeerRegistry, raw: NapiPeerEvent): MeshPeerEvent {
  const type = raw.eventType;
  const peerId = raw.peerId;
  let peer: Peer | undefined;
  if (raw.peer) {
    peer = reg.upsert(raw.peer);
  }
  if (type === 'left' && peerId) {
    // Core emits `left` with the entry's final (offline) snapshot; fall back
    // to the interned handle if it is ever absent so cleanup code always
    // receives a usable `ev.peer` (RFC 022 §16.4).
    peer ??= reg.getByTailscaleId(peerId);
    if (peer) {
      peer._markLeft();
      reg.remove(peer.ref);
    } else {
      reg.removeByTailscaleId(peerId);
    }
  }
  return { type, peerId, peer, authUrl: raw.authUrl };
}

/**
 * Create and start a Truffle node with sensible defaults.
 *
 * @example
 * ```ts
 * const mesh = await createMeshNode({ appId: 'chat', deviceName: 'laptop' });
 *
 * mesh.onMessage('chat', async (msg) => {
 *   if (typeof msg.from !== 'string') {
 *     await msg.from.send('chat', Buffer.from('ack'));
 *   }
 * });
 *
 * const peers = await mesh.getPeers();
 * await peers[0]?.send('chat', Buffer.from('hello'));
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
    eagerIdentity,
  } = options;

  if (!/^[a-z][a-z0-9-]{1,31}$/.test(appId)) {
    throw new Error(
      `[truffle] Invalid appId ${JSON.stringify(appId)}: must match ^[a-z][a-z0-9-]{1,31}$ ` +
        `(2–32 chars; lowercase letters, digits, hyphens; must start with a letter).`,
    );
  }

  const resolvedSidecarPath = sidecarPath ?? resolveSidecarPath();
  const node = new NapiNode();

  node.onAuthRequired((url: string) => {
    if (autoAuth) {
      (customOpenUrl ?? defaultOpenUrl)(url);
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
    eagerIdentity,
  };

  try {
    await node.start(config);
  } catch (err) {
    try {
      await node.stop();
    } catch {
      /* ignore */
    }
    throw err;
  }

  const registry = new PeerRegistry(node);
  const mesh = node as unknown as MeshNode;

  // `mesh` IS the native node object, so each wrapper assignment shadows the
  // NAPI prototype method — the native method must be bound BEFORE the
  // assignment or the wrapper calls itself (infinite recursion).
  const nativeGetPeers = node.getPeers.bind(node);
  mesh.getPeers = async () => {
    const snaps = await nativeGetPeers();
    return registry.upsertAll(snaps);
  };

  const nativePeer = node.peer.bind(node);
  mesh.peer = async (query: string, opts?: { waitMs?: number }) => {
    const snap = await nativePeer(query, opts?.waitMs);
    if (!snap) return null;
    return registry.upsert(snap);
  };

  const nativeSend = node.send.bind(node);
  mesh.send = async (to: PeerLike, namespace: string, data: Buffer | Uint8Array) => {
    const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
    return nativeSend(peerLikeToQuery(to), namespace, buf);
  };

  const nativePing = node.ping.bind(node);
  mesh.ping = async (to: PeerLike) => nativePing(peerLikeToQuery(to));

  const nativeOpenTcp = node.openTcp.bind(node);
  mesh.openTcp = (to: PeerLike, port: number) => nativeOpenTcp(peerLikeToQuery(to), port);

  const nativeConnectQuic = node.connectQuic.bind(node);
  mesh.connectQuic = (to: PeerLike, port: number) => nativeConnectQuic(peerLikeToQuery(to), port);

  const nativeFileTransfer = node.fileTransfer.bind(node);
  mesh.fileTransfer = () => {
    const ft = nativeFileTransfer();
    // Same shadowing rule as the node itself: bind the native method before
    // the own-property assignment or the wrapper calls itself.
    const nativeSendFile = ft.sendFile.bind(ft);
    const nativePullFile = ft.pullFile.bind(ft);
    const wrapped = ft as unknown as MeshFileTransfer;
    wrapped.sendFile = (to: PeerLike, localPath: string, remotePath: string) =>
      nativeSendFile(peerLikeToQuery(to), localPath, remotePath);
    wrapped.pullFile = (to: PeerLike, remotePath: string, localPath: string) =>
      nativePullFile(peerLikeToQuery(to), remotePath, localPath);
    return wrapped;
  };

  // One unconditional native subscription maintains the registry — intern on
  // every snapshot-bearing event, retire on `left` — so `msg.from` resolves
  // to a handle and `left` carries one even when the app never calls
  // getPeers()/onPeerChange(). User callbacks fan out from here: the
  // registry must mutate exactly once per event, not once per subscriber.
  const peerListeners: Array<(event: MeshPeerEvent) => void> = [];
  const nativeOnPeerChange = node.onPeerChange.bind(node);
  nativeOnPeerChange((raw: NapiPeerEvent) => {
    const ev = toMeshPeerEvent(registry, raw);
    for (const cb of peerListeners) {
      try {
        cb(ev);
      } catch (err) {
        // Surface the callback error without starving later listeners or
        // the registry maintenance.
        queueMicrotask(() => {
          throw err;
        });
      }
    }
  });
  mesh.onPeerChange = (callback: (event: MeshPeerEvent) => void) => {
    peerListeners.push(callback);
  };

  const nativeOnMessage = node.onMessage.bind(node);
  mesh.onMessage = (namespace: string, callback: (msg: MeshNamespacedMessage) => void) => {
    nativeOnMessage(namespace, (raw: NapiNamespacedMessage) => {
      // Attribution is Tailscale id; intern when we already know the peer.
      const from: Peer | string = registry.getByTailscaleId(raw.from) ?? raw.from;
      callback({
        from,
        namespace: raw.namespace,
        msgType: raw.msgType,
        payload: raw.payload,
        timestamp: raw.timestamp,
      });
    });
  };

  if (onPeerChange) {
    mesh.onPeerChange(onPeerChange);
  }

  mesh.net = createNetNamespace(node, {
    // Live registry lookup behind TruffleSocket.remotePeer (RFC 023 §6.1).
    resolvePeer: (tailscaleId) => registry.getByTailscaleId(tailscaleId) ?? null,
  });
  mesh.http = createHttpNamespace(mesh.net);
  mesh.quic = createQuicNamespace(node);
  mesh.dgram = createDgramNamespace(node);
  mesh.ws = createWsNamespace(mesh.net);
  mesh.native = node;

  return mesh;
}
