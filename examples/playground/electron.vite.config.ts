import { resolve } from 'node:path';
import { defineConfig, externalizeDepsPlugin } from 'electron-vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  main: {
    plugins: [externalizeDepsPlugin()],
    build: {
      rollupOptions: {
        // @vibecook/truffle uses CommonJS `require()` internally to load its
        // platform-specific native addon and sidecar binary. Externalizing it
        // (instead of bundling) keeps the runtime resolution behavior intact.
        external: [
          '@vibecook/truffle',
          /^@vibecook\/truffle-native.*/,
          /^@vibecook\/truffle-sidecar.*/,
        ],
      },
    },
    resolve: {
      alias: {
        '@shared': resolve('src/shared'),
      },
    },
  },
  preload: {
    plugins: [externalizeDepsPlugin()],
    resolve: {
      alias: {
        '@shared': resolve('src/shared'),
      },
    },
  },
  renderer: {
    plugins: [react(), tailwindcss()],
    resolve: {
      alias: {
        '@': resolve('src/renderer/src'),
        '@shared': resolve('src/shared'),
      },
    },
  },
});
