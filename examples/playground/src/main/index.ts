/**
 * Electron main entry for the Truffle Playground.
 *
 * Owns the single `BrowserWindow`, the `TruffleManager`, and the IPC
 * bridge. The renderer is a Vite-served React app in dev mode and a
 * static bundle (loaded via `loadFile`) in the packaged build.
 */

import { app, BrowserWindow, shell } from 'electron';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

import { TruffleManager } from './truffle-manager.js';
import { registerIpcHandlers } from './ipc-handlers.js';

// ESM-safe __dirname (electron-vite emits ESM for the main process).
const __dirname = dirname(fileURLToPath(import.meta.url));

const manager = new TruffleManager();
let mainWindow: BrowserWindow | null = null;

function createWindow(): BrowserWindow {
  const win = new BrowserWindow({
    width: 1100,
    height: 740,
    minWidth: 900,
    minHeight: 600,
    backgroundColor: '#0f1117',
    titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'default',
    show: false,
    webPreferences: {
      preload: join(__dirname, '../preload/index.js'),
      contextIsolation: true,
      sandbox: false,
      nodeIntegration: false,
    },
  });

  win.once('ready-to-show', () => {
    win.show();
  });

  // Route target=_blank links through the OS browser instead of spawning
  // a new BrowserWindow.
  win.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url);
    return { action: 'deny' };
  });

  // electron-vite sets ELECTRON_RENDERER_URL in dev mode.
  const devUrl = process.env['ELECTRON_RENDERER_URL'];
  if (devUrl) {
    void win.loadURL(devUrl);
  } else {
    void win.loadFile(join(__dirname, '../renderer/index.html'));
  }

  return win;
}

app.whenReady().then(() => {
  mainWindow = createWindow();
  registerIpcHandlers(manager, mainWindow);

  mainWindow.on('closed', () => {
    mainWindow = null;
  });

  app.on('activate', () => {
    // macOS: re-create the window when the dock icon is clicked and no
    // windows remain. We still keep the process running (standard macOS
    // pattern) but `window-all-closed` also quits below since this is a
    // dev tool, not a consumer-facing app.
    if (BrowserWindow.getAllWindows().length === 0) {
      mainWindow = createWindow();
      registerIpcHandlers(manager, mainWindow);
      mainWindow.on('closed', () => {
        mainWindow = null;
      });
    }
  });
});

// Dev tool semantics: quit when all windows close, even on macOS.
app.on('window-all-closed', () => {
  app.quit();
});

// Gracefully stop the truffle node before exiting. `before-quit` fires
// before windows are destroyed, giving the manager a chance to flush
// WebSocket sends and shut the sidecar down cleanly.
let isQuitting = false;
app.on('before-quit', (event) => {
  if (isQuitting) return;
  event.preventDefault();
  isQuitting = true;
  manager
    .stop()
    .catch((err) => {
      console.error('[main] TruffleManager.stop() failed:', err);
    })
    .finally(() => {
      app.quit();
    });
});
