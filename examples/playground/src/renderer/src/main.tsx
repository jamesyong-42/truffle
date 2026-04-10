import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import './index.css';

const container = document.getElementById('root');
if (!container) {
  throw new Error('Root container #root not found in index.html');
}

// If the preload script failed to load, `window.truffle` is undefined and
// every IPC call in the React tree will crash with "Cannot read properties
// of undefined". Fail loudly here with an actionable message instead.
if (typeof window.truffle === 'undefined') {
  container.innerHTML = `
    <div style="
      display:flex;flex-direction:column;align-items:center;justify-content:center;
      height:100vh;padding:2rem;font-family:system-ui,sans-serif;
      background:#0f1117;color:#e2e8f0;text-align:center;">
      <h1 style="font-size:1.5rem;margin:0 0 1rem;">Preload failed to load</h1>
      <p style="max-width:40rem;color:#94a3b8;line-height:1.6;">
        The Electron preload script didn't initialize, so <code>window.truffle</code>
        is undefined. This is usually caused by a mismatch between the preload
        path in <code>src/main/index.ts</code> and the file electron-vite emits
        (<code>out/preload/index.mjs</code>).
      </p>
      <p style="max-width:40rem;color:#94a3b8;margin-top:1rem;">
        Check the terminal running <code>pnpm dev</code> and the main process
        console for the actual error.
      </p>
    </div>
  `;
  throw new Error('window.truffle is undefined — preload script failed to load');
}

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
