/**
 * Preload bridge — exposes `window.truffle` to the renderer.
 *
 * This file runs in Electron's preload sandbox (contextIsolation: true,
 * sandbox: false). It MUST be pure IPC routing — do not import any
 * Node-ish module or anything from `@vibecook/truffle`. Only `electron`
 * and the shared IPC contract types are allowed.
 *
 * Every invoke channel is wired to `ipcRenderer.invoke`. Every push
 * channel becomes an `onX(cb)` subscriber that returns an unsubscribe
 * function so React effects can clean up properly.
 */

import { contextBridge, ipcRenderer, type IpcRendererEvent } from 'electron';

import {
  IPC,
  type TruffleAPI,
  type StartConfig,
  type NodeIdentity,
  type NodeStateEvent,
  type Peer,
  type PeerEvent,
  type PingResult,
  type HealthInfo,
  type ChatMessage,
  type StoreSlice,
  type StoreEvent,
  type FileOffer,
  type TransferProgress,
  type TransferCompleted,
  type TransferFailed,
  type TransferResult,
  type UnsubscribeFn,
} from '@shared/ipc';

/**
 * Subscribe to a push channel and return an unsubscribe function.
 *
 * Creates a bound listener that drops the `IpcRendererEvent` and forwards
 * the typed payload to the caller's callback.
 */
function subscribe<T>(
  channel: string,
  cb: (payload: T) => void,
): UnsubscribeFn {
  const listener = (_event: IpcRendererEvent, payload: T): void => {
    cb(payload);
  };
  ipcRenderer.on(channel, listener);
  return () => {
    ipcRenderer.removeListener(channel, listener);
  };
}

const api: TruffleAPI = {
  // ─── Lifecycle ────────────────────────────────────────────────────────
  start: (config: StartConfig): Promise<NodeIdentity> =>
    ipcRenderer.invoke(IPC.START, config),
  stop: (): Promise<void> => ipcRenderer.invoke(IPC.STOP),
  getLocalInfo: (): Promise<NodeIdentity | null> =>
    ipcRenderer.invoke(IPC.GET_LOCAL_INFO),
  getNodeState: (): Promise<NodeStateEvent> =>
    ipcRenderer.invoke(IPC.GET_NODE_STATE),
  onNodeState: (cb): UnsubscribeFn =>
    subscribe<NodeStateEvent>(IPC.EVT_NODE_STATE, cb),

  // ─── Peers ────────────────────────────────────────────────────────────
  getPeers: (): Promise<Peer[]> => ipcRenderer.invoke(IPC.GET_PEERS),
  onPeerEvent: (cb): UnsubscribeFn => subscribe<PeerEvent>(IPC.EVT_PEER, cb),

  // ─── Ping / Health ────────────────────────────────────────────────────
  ping: (peerId: string): Promise<PingResult> =>
    ipcRenderer.invoke(IPC.PING, peerId),
  health: (): Promise<HealthInfo> => ipcRenderer.invoke(IPC.HEALTH),
  onHealthUpdate: (cb): UnsubscribeFn =>
    subscribe<HealthInfo>(IPC.EVT_HEALTH, cb),

  // ─── Chat ─────────────────────────────────────────────────────────────
  sendMessage: (peerId: string, text: string): Promise<void> =>
    ipcRenderer.invoke(IPC.SEND_MESSAGE, peerId, text),
  broadcast: (text: string): Promise<void> =>
    ipcRenderer.invoke(IPC.BROADCAST, text),
  onMessage: (cb): UnsubscribeFn => subscribe<ChatMessage>(IPC.EVT_MESSAGE, cb),

  // ─── SyncedStore ──────────────────────────────────────────────────────
  storeSet: (key: string, value: string): Promise<void> =>
    ipcRenderer.invoke(IPC.STORE_SET, key, value),
  storeUnset: (key: string): Promise<void> =>
    ipcRenderer.invoke(IPC.STORE_UNSET, key),
  storeGet: (): Promise<StoreSlice | null> => ipcRenderer.invoke(IPC.STORE_GET),
  storeAll: (): Promise<StoreSlice[]> => ipcRenderer.invoke(IPC.STORE_ALL),
  onStoreEvent: (cb): UnsubscribeFn => subscribe<StoreEvent>(IPC.EVT_STORE, cb),

  // ─── File Transfer ────────────────────────────────────────────────────
  sendFile: (peerId: string, localPath: string): Promise<TransferResult> =>
    ipcRenderer.invoke(IPC.SEND_FILE, peerId, localPath),
  acceptOffer: (token: string, savePath: string): Promise<void> =>
    ipcRenderer.invoke(IPC.ACCEPT_OFFER, token, savePath),
  rejectOffer: (token: string, reason: string): Promise<void> =>
    ipcRenderer.invoke(IPC.REJECT_OFFER, token, reason),
  getDownloadsDir: (): Promise<string> =>
    ipcRenderer.invoke(IPC.GET_DOWNLOADS_DIR),
  pickFile: (): Promise<string | null> => ipcRenderer.invoke(IPC.PICK_FILE),
  pickSavePath: (defaultName: string): Promise<string | null> =>
    ipcRenderer.invoke(IPC.PICK_SAVE_PATH, defaultName),
  onFileOffer: (cb): UnsubscribeFn =>
    subscribe<FileOffer>(IPC.EVT_FILE_OFFER, cb),
  onFileProgress: (cb): UnsubscribeFn =>
    subscribe<TransferProgress>(IPC.EVT_FILE_PROGRESS, cb),
  onFileCompleted: (cb): UnsubscribeFn =>
    subscribe<TransferCompleted>(IPC.EVT_FILE_COMPLETED, cb),
  onFileFailed: (cb): UnsubscribeFn =>
    subscribe<TransferFailed>(IPC.EVT_FILE_FAILED, cb),

  // ─── Auth ─────────────────────────────────────────────────────────────
  onAuthRequired: (cb): UnsubscribeFn =>
    subscribe<string>(IPC.EVT_AUTH_REQUIRED, cb),
  openAuthUrl: (url: string): Promise<void> =>
    ipcRenderer.invoke(IPC.OPEN_AUTH_URL, url),
};

contextBridge.exposeInMainWorld('truffle', api);
