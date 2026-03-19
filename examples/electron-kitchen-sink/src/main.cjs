const { app, BrowserWindow, ipcMain, dialog, shell } = require('electron');
const path = require('path');
const {
  NapiMeshNode,
  NapiFileTransferAdapter,
  NapiStoreSyncAdapter,
} = require('@vibecook/truffle-native');
const { resolveSidecarPath } = require('@vibecook/truffle-native/helpers');

let win = null;
let meshNode = null;
let fileTransfer = null;
let storeSync = null;

function send(channel, data) {
  if (win && !win.isDestroyed()) {
    win.webContents.send(channel, data);
  }
}

// ── MeshNode IPC ──────────────────────────────────────────────────────

ipcMain.handle('mesh:create', async (_, config) => {
  try {
    if (!config.sidecarPath) {
      config.sidecarPath = resolveSidecarPath();
    }
    meshNode = new NapiMeshNode(config);

    // Forward all events to renderer
    meshNode.onEvent((err, event) => {
      if (err) {
        send('mesh:event', { eventType: 'error', payload: err.message });
        return;
      }
      send('mesh:event', event);
      // Auto-open Tailscale auth URL in default browser
      if (event.eventType === 'authRequired' && event.payload) {
        const url = typeof event.payload === 'string' ? event.payload : String(event.payload);
        shell.openExternal(url);
      }
    });

    return { ok: true };
  } catch (e) {
    return { ok: false, error: e.message };
  }
});

ipcMain.handle('mesh:start', async () => {
  if (!meshNode) return { ok: false, error: 'No mesh node created' };
  try {
    await meshNode.start();
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e.message };
  }
});

ipcMain.handle('mesh:stop', async () => {
  if (!meshNode) return { ok: false, error: 'No mesh node' };
  try {
    await meshNode.stop();
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e.message };
  }
});

ipcMain.handle('mesh:isRunning', async () => {
  if (!meshNode) return false;
  return meshNode.isRunning();
});

ipcMain.handle('mesh:localDevice', async () => {
  if (!meshNode) return null;
  return meshNode.localDevice();
});

ipcMain.handle('mesh:deviceId', async () => {
  if (!meshNode) return null;
  return meshNode.deviceId();
});

ipcMain.handle('mesh:devices', async () => {
  if (!meshNode) return [];
  return meshNode.devices();
});

ipcMain.handle('mesh:deviceById', async (_, id) => {
  if (!meshNode) return null;
  return meshNode.deviceById(id);
});

ipcMain.handle('mesh:isPrimary', async () => {
  if (!meshNode) return false;
  return meshNode.isPrimary();
});

ipcMain.handle('mesh:primaryId', async () => {
  if (!meshNode) return null;
  return meshNode.primaryId();
});

ipcMain.handle('mesh:role', async () => {
  if (!meshNode) return 'secondary';
  return meshNode.role();
});

ipcMain.handle('mesh:authStatus', async () => {
  if (!meshNode) return 'unknown';
  return meshNode.authStatus();
});

ipcMain.handle('mesh:protocolVersion', () => {
  if (!meshNode) return null;
  return meshNode.protocolVersion();
});

ipcMain.handle('mesh:authUrl', async () => {
  if (!meshNode) return null;
  return meshNode.authUrl();
});

ipcMain.handle('mesh:sendEnvelope', async (_, deviceId, ns, type, payload) => {
  if (!meshNode) return false;
  return meshNode.sendEnvelope(deviceId, ns, type, payload);
});

ipcMain.handle('mesh:broadcastEnvelope', async (_, ns, type, payload) => {
  if (!meshNode) return;
  return meshNode.broadcastEnvelope(ns, type, payload);
});

ipcMain.handle('mesh:subscribedNamespaces', async () => {
  if (!meshNode) return [];
  const bus = meshNode.messageBus();
  return bus.subscribedNamespaces();
});

// ── Store Sync IPC ────────────────────────────────────────────────────

ipcMain.handle('sync:create', async (_, config) => {
  try {
    storeSync = new NapiStoreSyncAdapter(config);
    storeSync.onOutgoing((err, msg) => {
      if (err) return;
      send('sync:outgoing', msg);
      if (meshNode) {
        meshNode.broadcastEnvelope('sync', msg.msgType, msg.payload);
      }
    });
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e.message };
  }
});

ipcMain.handle('sync:start', async () => {
  if (!storeSync) return { ok: false, error: 'No sync adapter' };
  try {
    await storeSync.start();
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e.message };
  }
});

ipcMain.handle('sync:stop', async () => {
  if (!storeSync) return;
  return storeSync.stop();
});

ipcMain.handle('sync:dispose', async () => {
  if (!storeSync) return;
  await storeSync.dispose();
  storeSync = null;
});

ipcMain.handle('sync:handleLocalChanged', async (_, storeId, slice) => {
  if (!storeSync) return;
  return storeSync.handleLocalChanged(storeId, slice);
});

ipcMain.handle('sync:handleSyncMessage', async (_, from, msgType, payload) => {
  if (!storeSync) return;
  return storeSync.handleSyncMessage(from, msgType, payload);
});

ipcMain.handle('sync:handleDeviceDiscovered', async (_, deviceId) => {
  if (!storeSync) return;
  return storeSync.handleDeviceDiscovered(deviceId);
});

ipcMain.handle('sync:handleDeviceOffline', async (_, deviceId) => {
  if (!storeSync) return;
  return storeSync.handleDeviceOffline(deviceId);
});

// ── File Transfer IPC ─────────────────────────────────────────────────

ipcMain.handle('ft:create', async (_, config) => {
  try {
    fileTransfer = new NapiFileTransferAdapter(config);
    fileTransfer.onEvent((err, event) => {
      if (err) return;
      send('ft:event', event);
    });
    fileTransfer.onBusMessage((err, msg) => {
      if (err) return;
      send('ft:busMessage', msg);
      if (meshNode) {
        meshNode.sendEnvelope(msg.targetDeviceId, 'file-transfer', msg.messageType, JSON.parse(msg.payload));
      }
    });
    await fileTransfer.startManagerEvents();
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e.message };
  }
});

ipcMain.handle('ft:sendFile', async (_, targetDeviceId, filePath) => {
  if (!fileTransfer) return { ok: false, error: 'No file transfer adapter' };
  try {
    const transferId = await fileTransfer.sendFile(targetDeviceId, filePath);
    return { ok: true, transferId };
  } catch (e) {
    return { ok: false, error: e.message };
  }
});

ipcMain.handle('ft:acceptTransfer', async (_, offer, savePath) => {
  if (!fileTransfer) return;
  return fileTransfer.acceptTransfer(offer, savePath);
});

ipcMain.handle('ft:rejectTransfer', async (_, offer, reason) => {
  if (!fileTransfer) return;
  return fileTransfer.rejectTransfer(offer, reason);
});

ipcMain.handle('ft:cancelTransfer', async (_, transferId) => {
  if (!fileTransfer) return;
  return fileTransfer.cancelTransfer(transferId);
});

ipcMain.handle('ft:getTransfers', async () => {
  if (!fileTransfer) return [];
  return fileTransfer.getTransfers();
});

ipcMain.handle('ft:handleBusMessage', async (_, msgType, payload) => {
  if (!fileTransfer) return;
  return fileTransfer.handleBusMessage(msgType, payload);
});

ipcMain.handle('ft:pickFile', async () => {
  const result = await dialog.showOpenDialog(win, { properties: ['openFile'] });
  if (result.canceled) return null;
  return result.filePaths[0];
});

// ── Sidecar ───────────────────────────────────────────────────────────

ipcMain.handle('sidecar:resolve', () => {
  try {
    return { ok: true, path: resolveSidecarPath() };
  } catch (e) {
    return { ok: false, error: e.message };
  }
});

// ── App lifecycle ─────────────────────────────────────────────────────

app.whenReady().then(() => {
  win = new BrowserWindow({
    width: 1200,
    height: 800,
    webPreferences: {
      preload: path.join(__dirname, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  win.loadFile(path.join(__dirname, 'renderer', 'index.html'));
});

app.on('window-all-closed', async () => {
  if (storeSync) {
    try { await storeSync.dispose(); } catch {}
    storeSync = null;
  }
  if (meshNode) {
    try { await meshNode.stop(); } catch {}
    meshNode = null;
  }
  app.quit();
});
