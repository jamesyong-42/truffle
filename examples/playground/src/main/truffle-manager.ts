/**
 * `TruffleManager` — owns the `NapiNode` lifecycle and every truffle
 * subsystem used by the playground (chat, SyncedStore, file transfer,
 * health polling).
 *
 * It extends `EventEmitter` so `ipc-handlers.ts` can subscribe to
 * high-level events without coupling to the raw NAPI callbacks.
 */

import { EventEmitter } from 'node:events';
import { mkdirSync } from 'node:fs';
import { basename, join } from 'node:path';
import { app, shell } from 'electron';
import {
  createMeshNode,
  type NapiNode,
  type NapiNodeIdentity,
  type NapiSyncedStore,
  type NapiFileTransfer,
  type NapiPeer,
  type NapiPeerEvent,
  type NapiNamespacedMessage,
  type NapiFileOffer,
  type NapiOfferResponder,
  type NapiFileTransferEvent,
  type NapiStoreEvent,
  type NapiSlice,
  type NapiTransferResult,
} from '@vibecook/truffle';

import type {
  StartConfig,
  NodeIdentity,
  NodeState,
  NodeStateEvent,
  Peer,
  PeerEvent,
  PeerEventType,
  PingResult,
  HealthInfo,
  ChatMessage,
  PlaygroundStoreData,
  StoreSlice,
  StoreEvent,
  StoreEventType,
  FileOffer,
  TransferProgress,
  TransferCompleted,
  TransferFailed,
  TransferResult,
} from '@shared/ipc';

import { resolveSidecarPath } from './sidecar-path.js';

// ─── Wire-format for chat messages ──────────────────────────────────────
/** Payload JSON-encoded into the `chat` namespace. */
interface ChatWirePayload {
  text: string;
  ts: number;
  broadcast?: boolean;
}

// ─── Event map ──────────────────────────────────────────────────────────
/**
 * Strongly-typed event map for the manager's EventEmitter surface.
 * `ipc-handlers.ts` subscribes to each of these and forwards to the
 * renderer via `webContents.send()`.
 */
export interface TruffleManagerEvents {
  nodeState: (event: NodeStateEvent) => void;
  peerEvent: (event: PeerEvent) => void;
  message: (msg: ChatMessage) => void;
  storeEvent: (event: StoreEvent) => void;
  fileOffer: (offer: FileOffer) => void;
  fileProgress: (progress: TransferProgress) => void;
  fileCompleted: (result: TransferCompleted) => void;
  fileFailed: (failure: TransferFailed) => void;
  authRequired: (url: string) => void;
  health: (info: HealthInfo) => void;
}

export declare interface TruffleManager {
  on<K extends keyof TruffleManagerEvents>(
    event: K,
    listener: TruffleManagerEvents[K],
  ): this;
  off<K extends keyof TruffleManagerEvents>(
    event: K,
    listener: TruffleManagerEvents[K],
  ): this;
  emit<K extends keyof TruffleManagerEvents>(
    event: K,
    ...args: Parameters<TruffleManagerEvents[K]>
  ): boolean;
}

const CHAT_NAMESPACE = 'chat';
const STORE_ID = 'playground';
const HEALTH_POLL_INTERVAL_MS = 30_000;

export class TruffleManager extends EventEmitter {
  /** The underlying truffle node. Undefined until `start()` succeeds. */
  private node: NapiNode | undefined;

  /** The playground SyncedStore. Undefined until `start()` succeeds. */
  private store: NapiSyncedStore | undefined;

  /** The file transfer handle. Undefined until `start()` succeeds. */
  private fileTransfer: NapiFileTransfer | undefined;

  /** Lifecycle state, mirrored on every transition to the renderer. */
  private state: NodeState = 'idle';

  /** Cached local identity, populated after `start()`. */
  private identity: NodeIdentity | undefined;

  /** Pending file offers indexed by transfer token. */
  private offers: Map<string, NapiOfferResponder> = new Map();

  /** Authoritative local slice of the playground SyncedStore. */
  private localKv: Record<string, string> = {};

  /** Latest known peers, used to resolve `fromName` on incoming messages. */
  private peerCache: Map<string, NapiPeer> = new Map();

  /** Interval handle for the periodic health poll. */
  private healthPollHandle: NodeJS.Timeout | undefined;

  // ─── Public lifecycle API ─────────────────────────────────────────────

  /**
   * Start the node: spins up the sidecar, connects to the tailnet, creates
   * the SyncedStore, wires up every listener.
   *
   * Emits `nodeState` at every transition (`starting`, `running`, `error`).
   */
  async start(config: StartConfig): Promise<NodeIdentity> {
    if (this.state === 'running' || this.state === 'starting') {
      throw new Error(`Cannot start: node is already ${this.state}`);
    }

    this.setState('starting');

    try {
      const sidecarPath = resolveSidecarPath();

      // RFC 017 §6.3 — pin the Tailscale state directory to Electron's
      // user-data dir so node identity persists across restarts. The core
      // library would otherwise default to `dirs::data_dir()/truffle/...`,
      // which is semantically equivalent but lives alongside other apps;
      // keeping truffle state under the playground's own userData makes
      // "reset everything" a single-directory operation for the user.
      const deviceName = config.deviceName ?? 'default';
      const stateDir =
        config.stateDir ??
        join(app.getPath('userData'), 'truffle-state', config.appId, deviceName);
      // Make sure the directory exists before the Rust side tries to read
      // `device-id.txt` / write `tailscaled.state` into it.
      mkdirSync(stateDir, { recursive: true });

      // Install the auth handler BEFORE `createMeshNode` calls start() — the
      // 0.3.x `createMeshNode` passes our `onAuthRequired` to its post-start
      // `onPeerChange` subscription, so the callback fires as soon as the
      // tailnet reports `auth_required`.
      const node = await createMeshNode({
        appId: config.appId,
        deviceName,
        ...(config.deviceId !== undefined ? { deviceId: config.deviceId } : {}),
        stateDir,
        sidecarPath,
        ...(config.ephemeral !== undefined ? { ephemeral: config.ephemeral } : {}),
        ...(config.wsPort !== undefined ? { wsPort: config.wsPort } : {}),
        autoAuth: false,
        openUrl: (url) => {
          void shell.openExternal(url);
        },
        onAuthRequired: (url) => {
          this.emit('authRequired', url);
        },
        onPeerChange: (event) => {
          this.handlePeerChange(event);
        },
      });

      this.node = node;

      // Seed the local identity.
      const localInfo = node.getLocalInfo();
      this.identity = toNodeIdentity(localInfo);

      // Seed peer cache so `fromName` resolution works immediately.
      // Key by RFC 017 `deviceId` (ULID), not the Tailscale stable ID.
      const initialPeers = await node.getPeers();
      for (const peer of initialPeers) {
        this.peerCache.set(peer.deviceId, peer);
      }

      // Subscribe to chat messages.
      node.onMessage(CHAT_NAMESPACE, (msg) => {
        this.handleChatMessage(msg);
      });

      // Create the SyncedStore and wire it up.
      const store = node.syncedStore(STORE_ID);
      this.store = store;
      store.onChange((event) => {
        this.handleStoreEvent(event);
      });
      // Seed the local slice with an empty kv so peers can see we exist.
      this.localKv = {};
      const seed: PlaygroundStoreData = { kv: {}, updatedAt: Date.now() };
      await store.set(seed);

      // Wire up file transfer. We deliberately do NOT call autoAccept() —
      // the renderer decides accept/reject per offer via the responder map.
      const ft = node.fileTransfer();
      this.fileTransfer = ft;
      ft.onOffer((offer, responder) => {
        this.handleFileOffer(offer, responder);
      });
      ft.onEvent((event) => {
        this.handleFileTransferEvent(event);
      });

      // Kick off the health poll.
      this.startHealthPoll();

      this.setState('running', this.identity);
      return this.identity;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.setState('error', undefined, message);
      // Best-effort cleanup — `start()` failed, so any partial state must go.
      await this.teardown().catch(() => {});
      throw err;
    }
  }

  /**
   * Stop the node and tear down every subsystem. Safe to call multiple
   * times or on an already-stopped manager.
   */
  async stop(): Promise<void> {
    if (this.state === 'idle' || this.state === 'stopping') {
      return;
    }
    this.setState('stopping');
    await this.teardown();
    this.setState('idle');
  }

  /** Returns the cached local identity, or `null` if the node is not running. */
  getLocalInfo(): NodeIdentity | null {
    return this.identity ?? null;
  }

  /** Returns the current lifecycle state as a `NodeStateEvent`. */
  getNodeState(): NodeStateEvent {
    const event: NodeStateEvent = { state: this.state };
    if (this.identity) {
      event.identity = this.identity;
    }
    return event;
  }

  // ─── Peers ────────────────────────────────────────────────────────────

  async getPeers(): Promise<Peer[]> {
    const node = this.requireNode();
    const peers = await node.getPeers();
    // Refresh the cache so the latest info is available for fromName lookups.
    // Cache key is `deviceId` (RFC 017 ULID) — the same key used by
    // `NapiNamespacedMessage.from` and incoming peer events.
    this.peerCache.clear();
    for (const peer of peers) {
      this.peerCache.set(peer.deviceId, peer);
    }
    return peers.map(toPeer);
  }

  async ping(peerId: string): Promise<PingResult> {
    const node = this.requireNode();
    const result = await node.ping(peerId);
    const out: PingResult = {
      latencyMs: result.latencyMs,
      connection: result.connection,
    };
    if (result.peerAddr !== undefined) {
      out.peerAddr = result.peerAddr;
    }
    return out;
  }

  async health(): Promise<HealthInfo> {
    const node = this.requireNode();
    const info = await node.health();
    return toHealthInfo(info);
  }

  async resolvePeerId(nameOrId: string): Promise<string> {
    const node = this.requireNode();
    return node.resolvePeerId(nameOrId);
  }

  // ─── Chat ─────────────────────────────────────────────────────────────

  async sendMessage(peerId: string, text: string): Promise<void> {
    const node = this.requireNode();
    const payload: ChatWirePayload = { text, ts: Date.now() };
    await node.send(
      peerId,
      CHAT_NAMESPACE,
      Buffer.from(JSON.stringify(payload)),
    );
  }

  async broadcast(text: string): Promise<void> {
    const node = this.requireNode();
    const payload: ChatWirePayload = { text, ts: Date.now(), broadcast: true };
    await node.broadcast(CHAT_NAMESPACE, Buffer.from(JSON.stringify(payload)));
  }

  // ─── SyncedStore ──────────────────────────────────────────────────────

  async storeSet(key: string, value: string): Promise<void> {
    const store = this.requireStore();
    this.localKv[key] = value;
    const data: PlaygroundStoreData = {
      kv: { ...this.localKv },
      updatedAt: Date.now(),
    };
    await store.set(data);
  }

  async storeUnset(key: string): Promise<void> {
    const store = this.requireStore();
    delete this.localKv[key];
    const data: PlaygroundStoreData = {
      kv: { ...this.localKv },
      updatedAt: Date.now(),
    };
    await store.set(data);
  }

  async storeGet(): Promise<StoreSlice | null> {
    const store = this.requireStore();
    const localId = this.requireIdentity().deviceId;
    const slice = await store.get(localId);
    if (!slice) return null;
    return toStoreSlice(slice, true);
  }

  async storeAll(): Promise<StoreSlice[]> {
    const store = this.requireStore();
    const localId = this.identity?.deviceId;
    const slices = await store.all();
    return slices.map((slice) => toStoreSlice(slice, slice.deviceId === localId));
  }

  // ─── File Transfer ────────────────────────────────────────────────────

  async sendFile(peerId: string, localPath: string): Promise<TransferResult> {
    const ft = this.requireFileTransfer();
    const result = await ft.sendFile(peerId, localPath, basename(localPath));
    return toTransferResult(result);
  }

  async acceptOffer(token: string, savePath: string): Promise<void> {
    const responder = this.offers.get(token);
    if (!responder) {
      throw new Error(`No pending offer for token ${token}`);
    }
    this.offers.delete(token);
    await responder.accept(savePath);
  }

  async rejectOffer(token: string, reason: string): Promise<void> {
    const responder = this.offers.get(token);
    if (!responder) {
      throw new Error(`No pending offer for token ${token}`);
    }
    this.offers.delete(token);
    await responder.reject(reason);
  }

  // ─── Internal event handlers ──────────────────────────────────────────

  private handlePeerChange(event: NapiPeerEvent): void {
    // `auth_required` is delivered separately via `onAuthRequired`. Skip it.
    if (event.eventType === 'auth_required') {
      return;
    }

    // Maintain the peer cache so subsequent chat messages can resolve
    // fromName without a round-trip to getPeers().
    if (event.peer) {
      this.peerCache.set(event.peerId, event.peer);
    } else if (event.eventType === 'left') {
      this.peerCache.delete(event.peerId);
    }

    const mapped = toPeerEvent(event);
    if (!mapped) return;
    this.emit('peerEvent', mapped);
  }

  private handleChatMessage(msg: NapiNamespacedMessage): void {
    // Payload was JSON-encoded by the sender in `sendMessage()`/`broadcast()`.
    const payload = msg.payload as ChatWirePayload | null | undefined;
    if (!payload || typeof payload.text !== 'string') {
      return;
    }
    // `msg.from` is the sender's `deviceId` (ULID). Resolve it to the
    // human-readable `deviceName` via the peer cache; fall back to the
    // raw ULID if we don't know the peer yet (first-ever hello race).
    const fromName = this.peerCache.get(msg.from)?.deviceName ?? msg.from;
    const chat: ChatMessage = {
      from: msg.from,
      fromName,
      text: payload.text,
      ts: typeof payload.ts === 'number' ? payload.ts : (msg.timestamp ?? Date.now()),
    };
    if (payload.broadcast === true) {
      chat.broadcast = true;
    }
    this.emit('message', chat);
  }

  private handleStoreEvent(event: NapiStoreEvent): void {
    const eventType = event.eventType as StoreEventType;
    const mapped: StoreEvent = { eventType };
    if (event.deviceId !== undefined) mapped.deviceId = event.deviceId;
    if (event.version !== undefined) mapped.version = event.version;
    this.emit('storeEvent', mapped);
  }

  private handleFileOffer(
    offer: NapiFileOffer,
    responder: NapiOfferResponder,
  ): void {
    this.offers.set(offer.token, responder);
    const mapped: FileOffer = {
      token: offer.token,
      fromPeer: offer.fromPeer,
      fromName: offer.fromName,
      fileName: offer.fileName,
      size: offer.size,
      sha256: offer.sha256,
      suggestedPath: offer.suggestedPath,
    };
    this.emit('fileOffer', mapped);
  }

  private handleFileTransferEvent(event: NapiFileTransferEvent): void {
    switch (event.eventType) {
      case 'progress': {
        if (!event.progress) return;
        const p = event.progress;
        const mapped: TransferProgress = {
          token: p.token,
          direction: p.direction === 'send' ? 'send' : 'receive',
          fileName: p.fileName,
          bytesTransferred: p.bytesTransferred,
          totalBytes: p.totalBytes,
          speedBps: p.speedBps,
        };
        this.emit('fileProgress', mapped);
        return;
      }
      case 'completed': {
        const direction: 'send' | 'receive' =
          event.direction === 'send' ? 'send' : 'receive';
        const mapped: TransferCompleted = {
          token: event.token ?? '',
          direction,
          fileName: event.fileName ?? '',
          bytesTransferred: event.bytesTransferred ?? 0,
          sha256: event.sha256 ?? '',
          elapsedSecs: event.elapsedSecs ?? 0,
        };
        this.emit('fileCompleted', mapped);
        return;
      }
      case 'failed':
      case 'rejected': {
        const direction: 'send' | 'receive' | undefined =
          event.direction === 'send' || event.direction === 'receive'
            ? event.direction
            : undefined;
        const mapped: TransferFailed = {
          token: event.token ?? '',
          reason: event.reason ?? event.eventType,
          eventType: event.eventType,
        };
        if (event.fileName !== undefined) mapped.fileName = event.fileName;
        if (direction !== undefined) mapped.direction = direction;
        // Clean up the responder if this was an inbound offer that failed
        // before accept/reject removed it.
        if (event.token) {
          this.offers.delete(event.token);
        }
        this.emit('fileFailed', mapped);
        return;
      }
      default:
        // `offer_received`, `hashing`, `waiting_for_accept` — no renderer
        // channel for these in the v1 playground. Ignore.
        return;
    }
  }

  // ─── Internal helpers ─────────────────────────────────────────────────

  private setState(
    state: NodeState,
    identity?: NodeIdentity,
    error?: string,
  ): void {
    this.state = state;
    const event: NodeStateEvent = { state };
    if (identity) event.identity = identity;
    if (error !== undefined) event.error = error;
    this.emit('nodeState', event);
  }

  private startHealthPoll(): void {
    this.stopHealthPoll();
    this.healthPollHandle = setInterval(() => {
      const node = this.node;
      if (!node) return;
      node
        .health()
        .then((info) => {
          this.emit('health', toHealthInfo(info));
        })
        .catch(() => {
          // Non-fatal — swallow polling errors.
        });
    }, HEALTH_POLL_INTERVAL_MS);
    // Don't block node shutdown on the timer.
    if (typeof this.healthPollHandle.unref === 'function') {
      this.healthPollHandle.unref();
    }
  }

  private stopHealthPoll(): void {
    if (this.healthPollHandle) {
      clearInterval(this.healthPollHandle);
      this.healthPollHandle = undefined;
    }
  }

  private async teardown(): Promise<void> {
    this.stopHealthPoll();
    const store = this.store;
    const node = this.node;
    this.store = undefined;
    this.fileTransfer = undefined;
    this.node = undefined;
    this.identity = undefined;
    this.offers.clear();
    this.peerCache.clear();
    this.localKv = {};

    if (store) {
      await store.stop().catch(() => {});
    }
    if (node) {
      await node.stop().catch(() => {});
    }
  }

  private requireNode(): NapiNode {
    if (!this.node) {
      throw new Error('Node not started');
    }
    return this.node;
  }

  private requireStore(): NapiSyncedStore {
    if (!this.store) {
      throw new Error('Node not started');
    }
    return this.store;
  }

  private requireFileTransfer(): NapiFileTransfer {
    if (!this.fileTransfer) {
      throw new Error('Node not started');
    }
    return this.fileTransfer;
  }

  private requireIdentity(): NodeIdentity {
    if (!this.identity) {
      throw new Error('Node not started');
    }
    return this.identity;
  }
}

// ─── NAPI → IPC shape converters ────────────────────────────────────────

function toNodeIdentity(info: NapiNodeIdentity): NodeIdentity {
  const out: NodeIdentity = {
    appId: info.appId,
    deviceId: info.deviceId,
    deviceName: info.deviceName,
    tailscaleHostname: info.tailscaleHostname,
    tailscaleId: info.tailscaleId,
  };
  if (info.dnsName !== undefined) out.dnsName = info.dnsName;
  if (info.ip !== undefined) out.ip = info.ip;
  return out;
}

function toPeer(peer: NapiPeer): Peer {
  const out: Peer = {
    deviceId: peer.deviceId,
    deviceName: peer.deviceName,
    tailscaleId: peer.tailscaleId,
    ip: peer.ip,
    online: peer.online,
    wsConnected: peer.wsConnected,
    connectionType: peer.connectionType,
  };
  if (peer.os !== undefined) out.os = peer.os;
  if (peer.lastSeen !== undefined) out.lastSeen = peer.lastSeen;
  return out;
}

function toPeerEvent(event: NapiPeerEvent): PeerEvent | null {
  // Narrow the stringly-typed NAPI `eventType` to our typed union.
  const allowed: readonly PeerEventType[] = [
    'joined',
    'left',
    'ws_connected',
    'ws_disconnected',
    'updated',
  ];
  if (!allowed.includes(event.eventType as PeerEventType)) {
    return null;
  }
  const out: PeerEvent = {
    eventType: event.eventType as PeerEventType,
    peerId: event.peerId,
  };
  if (event.peer) out.peer = toPeer(event.peer);
  return out;
}

function toHealthInfo(info: {
  state: string;
  keyExpiry?: string;
  warnings: string[];
  healthy: boolean;
}): HealthInfo {
  const out: HealthInfo = {
    state: info.state,
    warnings: [...info.warnings],
    healthy: info.healthy,
  };
  if (info.keyExpiry !== undefined) out.keyExpiry = info.keyExpiry;
  return out;
}

function toStoreSlice(slice: NapiSlice, isLocal: boolean): StoreSlice {
  // `slice.data` is an opaque JSON value at the NAPI boundary. Coerce it to
  // our `PlaygroundStoreData` shape, falling back to an empty kv if a peer
  // wrote something unexpected.
  const raw = slice.data as Partial<PlaygroundStoreData> | null | undefined;
  const data: PlaygroundStoreData = {
    kv: raw && typeof raw.kv === 'object' && raw.kv !== null ? raw.kv : {},
    updatedAt:
      raw && typeof raw.updatedAt === 'number' ? raw.updatedAt : slice.updatedAt,
  };
  return {
    deviceId: slice.deviceId,
    data,
    version: slice.version,
    updatedAt: slice.updatedAt,
    isLocal,
  };
}

function toTransferResult(result: NapiTransferResult): TransferResult {
  return {
    bytesTransferred: result.bytesTransferred,
    sha256: result.sha256,
    elapsedSecs: result.elapsedSecs,
  };
}
