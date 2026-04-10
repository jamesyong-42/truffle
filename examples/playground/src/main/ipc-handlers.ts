/**
 * Wires up every `ipcMain.handle()` channel and forwards every
 * `TruffleManager` event to the renderer via `webContents.send()`.
 *
 * The mapping between channels, manager methods, and manager events is
 * exhaustive — renderer-visible functionality lives entirely in this
 * file plus the preload bridge.
 */

import { app, dialog, ipcMain, shell, type BrowserWindow } from 'electron';

import {
  IPC,
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
} from '@shared/ipc';

import type { TruffleManager } from './truffle-manager.js';

export function registerIpcHandlers(
  manager: TruffleManager,
  mainWindow: BrowserWindow,
): void {
  // ─── Invoke handlers (renderer → main) ────────────────────────────────

  ipcMain.handle(
    IPC.START,
    async (_event, config: StartConfig): Promise<NodeIdentity> => {
      return manager.start(config);
    },
  );

  ipcMain.handle(IPC.STOP, async (): Promise<void> => {
    await manager.stop();
  });

  ipcMain.handle(IPC.GET_LOCAL_INFO, (): NodeIdentity | null => {
    return manager.getLocalInfo();
  });

  ipcMain.handle(IPC.GET_NODE_STATE, (): NodeStateEvent => {
    return manager.getNodeState();
  });

  ipcMain.handle(IPC.GET_PEERS, async (): Promise<Peer[]> => {
    return manager.getPeers();
  });

  ipcMain.handle(
    IPC.PING,
    async (_event, peerId: string): Promise<PingResult> => {
      return manager.ping(peerId);
    },
  );

  ipcMain.handle(IPC.HEALTH, async (): Promise<HealthInfo> => {
    return manager.health();
  });

  ipcMain.handle(
    IPC.SEND_MESSAGE,
    async (_event, peerId: string, text: string): Promise<void> => {
      await manager.sendMessage(peerId, text);
    },
  );

  ipcMain.handle(
    IPC.BROADCAST,
    async (_event, text: string): Promise<void> => {
      await manager.broadcast(text);
    },
  );

  ipcMain.handle(
    IPC.STORE_SET,
    async (_event, key: string, value: string): Promise<void> => {
      await manager.storeSet(key, value);
    },
  );

  ipcMain.handle(
    IPC.STORE_UNSET,
    async (_event, key: string): Promise<void> => {
      await manager.storeUnset(key);
    },
  );

  ipcMain.handle(IPC.STORE_GET, async (): Promise<StoreSlice | null> => {
    return manager.storeGet();
  });

  ipcMain.handle(IPC.STORE_ALL, async (): Promise<StoreSlice[]> => {
    return manager.storeAll();
  });

  ipcMain.handle(
    IPC.SEND_FILE,
    async (_event, peerId: string, localPath: string): Promise<TransferResult> => {
      return manager.sendFile(peerId, localPath);
    },
  );

  ipcMain.handle(
    IPC.ACCEPT_OFFER,
    async (_event, token: string, savePath: string): Promise<void> => {
      await manager.acceptOffer(token, savePath);
    },
  );

  ipcMain.handle(
    IPC.REJECT_OFFER,
    async (_event, token: string, reason: string): Promise<void> => {
      await manager.rejectOffer(token, reason);
    },
  );

  ipcMain.handle(IPC.GET_DOWNLOADS_DIR, (): string => {
    return app.getPath('downloads');
  });

  ipcMain.handle(IPC.PICK_FILE, async (): Promise<string | null> => {
    const result = await dialog.showOpenDialog(mainWindow, {
      properties: ['openFile'],
    });
    if (result.canceled || result.filePaths.length === 0) {
      return null;
    }
    return result.filePaths[0] ?? null;
  });

  ipcMain.handle(
    IPC.PICK_SAVE_PATH,
    async (_event, defaultName: string): Promise<string | null> => {
      const result = await dialog.showSaveDialog(mainWindow, {
        defaultPath: defaultName,
      });
      if (result.canceled || !result.filePath) {
        return null;
      }
      return result.filePath;
    },
  );

  ipcMain.handle(IPC.OPEN_AUTH_URL, async (_event, url: string): Promise<void> => {
    await shell.openExternal(url);
  });

  // ─── Push events (main → renderer) ────────────────────────────────────
  // Every send is guarded against a destroyed BrowserWindow so shutdown
  // races don't crash the main process.

  const send = <T>(channel: string, payload: T): void => {
    if (mainWindow.isDestroyed()) return;
    const { webContents } = mainWindow;
    if (webContents.isDestroyed()) return;
    webContents.send(channel, payload);
  };

  const onNodeState = (event: NodeStateEvent): void => {
    send(IPC.EVT_NODE_STATE, event);
  };
  const onPeerEvent = (event: PeerEvent): void => {
    send(IPC.EVT_PEER, event);
  };
  const onMessage = (msg: ChatMessage): void => {
    send(IPC.EVT_MESSAGE, msg);
  };
  const onStoreEvent = (event: StoreEvent): void => {
    send(IPC.EVT_STORE, event);
  };
  const onFileOffer = (offer: FileOffer): void => {
    send(IPC.EVT_FILE_OFFER, offer);
  };
  const onFileProgress = (progress: TransferProgress): void => {
    send(IPC.EVT_FILE_PROGRESS, progress);
  };
  const onFileCompleted = (result: TransferCompleted): void => {
    send(IPC.EVT_FILE_COMPLETED, result);
  };
  const onFileFailed = (failure: TransferFailed): void => {
    send(IPC.EVT_FILE_FAILED, failure);
  };
  const onAuthRequired = (url: string): void => {
    send(IPC.EVT_AUTH_REQUIRED, url);
  };
  const onHealth = (info: HealthInfo): void => {
    send(IPC.EVT_HEALTH, info);
  };

  manager.on('nodeState', onNodeState);
  manager.on('peerEvent', onPeerEvent);
  manager.on('message', onMessage);
  manager.on('storeEvent', onStoreEvent);
  manager.on('fileOffer', onFileOffer);
  manager.on('fileProgress', onFileProgress);
  manager.on('fileCompleted', onFileCompleted);
  manager.on('fileFailed', onFileFailed);
  manager.on('authRequired', onAuthRequired);
  manager.on('health', onHealth);

  // Detach listeners when the window closes so repeat `registerIpcHandlers`
  // calls (if any) do not double-fire. IPC invoke handlers are not removed
  // because this playground creates exactly one window for the lifetime of
  // the app.
  mainWindow.on('closed', () => {
    manager.off('nodeState', onNodeState);
    manager.off('peerEvent', onPeerEvent);
    manager.off('message', onMessage);
    manager.off('storeEvent', onStoreEvent);
    manager.off('fileOffer', onFileOffer);
    manager.off('fileProgress', onFileProgress);
    manager.off('fileCompleted', onFileCompleted);
    manager.off('fileFailed', onFileFailed);
    manager.off('authRequired', onAuthRequired);
    manager.off('health', onHealth);
  });
}
