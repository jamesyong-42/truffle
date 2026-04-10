/**
 * Shared IPC contract between the Electron main process and the renderer.
 *
 * The main process owns the `NapiNode` from `@vibecook/truffle` and exposes
 * truffle operations through typed `ipcMain.handle()` channels. The preload
 * script bridges these via `contextBridge.exposeInMainWorld('truffle', ...)`
 * so the renderer sees a strongly-typed `window.truffle` API.
 *
 * This file is the single source of truth for all cross-process types.
 * Main process code imports from `@shared/ipc`, renderer imports from
 * `@shared/ipc`, preload imports from `@shared/ipc`. No other file should
 * redefine any of these shapes.
 */

// ─── Lifecycle ──────────────────────────────────────────────────────────

export interface StartConfig {
  /** Node name advertised to the tailnet (becomes the hostname prefix). */
  name: string;
  /** State directory for Tailscale. Defaults to `./.truffle-state/{name}`. */
  stateDir?: string;
  /** Ephemeral node — removed from tailnet on shutdown. */
  ephemeral?: boolean;
  /** WebSocket listener port. Defaults to 0 (auto-pick). */
  wsPort?: number;
}

export interface NodeIdentity {
  id: string;
  hostname: string;
  name: string;
  dnsName?: string;
  ip?: string;
}

/** State the main process broadcasts when the node lifecycle transitions. */
export type NodeState = 'idle' | 'starting' | 'running' | 'stopping' | 'error';

export interface NodeStateEvent {
  state: NodeState;
  identity?: NodeIdentity;
  error?: string;
}

// ─── Peers ──────────────────────────────────────────────────────────────

export interface Peer {
  id: string;
  name: string;
  ip: string;
  online: boolean;
  wsConnected: boolean;
  connectionType: string;
  os?: string;
  lastSeen?: string;
}

export type PeerEventType =
  | 'joined'
  | 'left'
  | 'ws_connected'
  | 'ws_disconnected'
  | 'updated';

export interface PeerEvent {
  eventType: PeerEventType;
  peerId: string;
  peer?: Peer;
}

// ─── Ping / Health ──────────────────────────────────────────────────────

export interface PingResult {
  latencyMs: number;
  connection: string;
  peerAddr?: string;
}

export interface HealthInfo {
  state: string;
  keyExpiry?: string;
  warnings: string[];
  healthy: boolean;
}

// ─── Messaging (chat namespace) ─────────────────────────────────────────

export interface ChatMessage {
  /** Stable node ID of the sender. */
  from: string;
  /** Human-readable sender name (resolved by main process from peer cache). */
  fromName: string;
  /** Message body. */
  text: string;
  /** Millisecond epoch at sender. */
  ts: number;
  /** Present for broadcast messages; absent for direct messages. */
  broadcast?: boolean;
}

// ─── SyncedStore ────────────────────────────────────────────────────────

/** The shape our playground writes into the SyncedStore. */
export interface PlaygroundStoreData {
  kv: Record<string, string>;
  updatedAt: number;
}

export interface StoreSlice {
  deviceId: string;
  data: PlaygroundStoreData;
  version: number;
  updatedAt: number;
  /** Injected by main process so the renderer can render "(you)" decorations. */
  isLocal: boolean;
}

export type StoreEventType = 'local_changed' | 'peer_updated' | 'peer_removed';

export interface StoreEvent {
  eventType: StoreEventType;
  deviceId?: string;
  version?: number;
}

// ─── File Transfer ──────────────────────────────────────────────────────

export interface FileOffer {
  token: string;
  fromPeer: string;
  fromName: string;
  fileName: string;
  size: number;
  sha256: string;
  suggestedPath: string;
}

export interface TransferProgress {
  token: string;
  direction: 'send' | 'receive';
  fileName: string;
  bytesTransferred: number;
  totalBytes: number;
  speedBps: number;
}

export interface TransferCompleted {
  token: string;
  direction: 'send' | 'receive';
  fileName: string;
  bytesTransferred: number;
  sha256: string;
  elapsedSecs: number;
  /** For received files: where it was saved. */
  savedPath?: string;
}

export interface TransferFailed {
  token: string;
  fileName?: string;
  direction?: 'send' | 'receive';
  reason: string;
  eventType: 'failed' | 'rejected';
}

export interface TransferResult {
  bytesTransferred: number;
  sha256: string;
  elapsedSecs: number;
}

// ─── Event discriminated unions ─────────────────────────────────────────
// These are what the preload exposes as `onX(callback)` subscribers.
// Each corresponds to a `webContents.send('truffle:event:X', ...)` call
// in the main process.

export type UnsubscribeFn = () => void;

// ─── Top-level API surface exposed on `window.truffle` ──────────────────

export interface TruffleAPI {
  // Lifecycle
  start(config: StartConfig): Promise<NodeIdentity>;
  stop(): Promise<void>;
  getLocalInfo(): Promise<NodeIdentity | null>;
  getNodeState(): Promise<NodeStateEvent>;
  onNodeState(cb: (event: NodeStateEvent) => void): UnsubscribeFn;

  // Peers
  getPeers(): Promise<Peer[]>;
  onPeerEvent(cb: (event: PeerEvent) => void): UnsubscribeFn;

  // Ping / Health
  ping(peerId: string): Promise<PingResult>;
  health(): Promise<HealthInfo>;
  onHealthUpdate(cb: (info: HealthInfo) => void): UnsubscribeFn;

  // Chat
  sendMessage(peerId: string, text: string): Promise<void>;
  broadcast(text: string): Promise<void>;
  onMessage(cb: (msg: ChatMessage) => void): UnsubscribeFn;

  // SyncedStore
  storeSet(key: string, value: string): Promise<void>;
  storeUnset(key: string): Promise<void>;
  storeGet(): Promise<StoreSlice | null>;
  storeAll(): Promise<StoreSlice[]>;
  onStoreEvent(cb: (event: StoreEvent) => void): UnsubscribeFn;

  // File Transfer
  sendFile(peerId: string, localPath: string): Promise<TransferResult>;
  acceptOffer(token: string, savePath: string): Promise<void>;
  rejectOffer(token: string, reason: string): Promise<void>;
  getDownloadsDir(): Promise<string>;
  pickFile(): Promise<string | null>;
  pickSavePath(defaultName: string): Promise<string | null>;
  onFileOffer(cb: (offer: FileOffer) => void): UnsubscribeFn;
  onFileProgress(cb: (progress: TransferProgress) => void): UnsubscribeFn;
  onFileCompleted(cb: (result: TransferCompleted) => void): UnsubscribeFn;
  onFileFailed(cb: (error: TransferFailed) => void): UnsubscribeFn;

  // Auth
  onAuthRequired(cb: (url: string) => void): UnsubscribeFn;
  openAuthUrl(url: string): Promise<void>;
}

// ─── IPC channel name constants ─────────────────────────────────────────
// Centralised so the main process and preload agree on exact strings.
// Renderer code should never reference these directly — it uses `window.truffle`.

export const IPC = {
  // Invoke (renderer → main)
  START: 'truffle:start',
  STOP: 'truffle:stop',
  GET_LOCAL_INFO: 'truffle:getLocalInfo',
  GET_NODE_STATE: 'truffle:getNodeState',
  GET_PEERS: 'truffle:getPeers',
  PING: 'truffle:ping',
  HEALTH: 'truffle:health',
  SEND_MESSAGE: 'truffle:sendMessage',
  BROADCAST: 'truffle:broadcast',
  STORE_SET: 'truffle:storeSet',
  STORE_UNSET: 'truffle:storeUnset',
  STORE_GET: 'truffle:storeGet',
  STORE_ALL: 'truffle:storeAll',
  SEND_FILE: 'truffle:sendFile',
  ACCEPT_OFFER: 'truffle:acceptOffer',
  REJECT_OFFER: 'truffle:rejectOffer',
  GET_DOWNLOADS_DIR: 'truffle:getDownloadsDir',
  PICK_FILE: 'truffle:pickFile',
  PICK_SAVE_PATH: 'truffle:pickSavePath',
  OPEN_AUTH_URL: 'truffle:openAuthUrl',

  // Push (main → renderer)
  EVT_NODE_STATE: 'truffle:event:nodeState',
  EVT_PEER: 'truffle:event:peer',
  EVT_HEALTH: 'truffle:event:health',
  EVT_MESSAGE: 'truffle:event:message',
  EVT_STORE: 'truffle:event:store',
  EVT_FILE_OFFER: 'truffle:event:fileOffer',
  EVT_FILE_PROGRESS: 'truffle:event:fileProgress',
  EVT_FILE_COMPLETED: 'truffle:event:fileCompleted',
  EVT_FILE_FAILED: 'truffle:event:fileFailed',
  EVT_AUTH_REQUIRED: 'truffle:event:authRequired',
} as const;

export type IpcChannel = (typeof IPC)[keyof typeof IPC];
