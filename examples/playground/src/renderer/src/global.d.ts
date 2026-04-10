/**
 * Global type augmentations for the Electron renderer process.
 *
 * - `window.truffle` is exposed by the preload script via contextBridge.
 * - Electron extends the HTML5 `File` interface with a `.path` property that
 *   yields the absolute filesystem path of a dropped file. This is an
 *   Electron-only extension and is not present in browser TypeScript lib defs.
 */

import type { TruffleAPI } from '@shared/ipc';

declare global {
  interface Window {
    truffle: TruffleAPI;
  }

  /**
   * Electron augments File with an absolute filesystem path. This only
   * exists inside Electron renderer processes.
   */
  interface File {
    readonly path?: string;
  }
}

export {};
