import { defineConfig } from 'vitepress';

export default defineConfig({
  title: 'Truffle',
  description: 'Cross-device mesh networking for TypeScript apps',
  base: '/truffle/',
  themeConfig: {
    nav: [
      { text: 'Guide', link: '/guide/getting-started' },
      { text: 'API', link: '/api/' },
      { text: 'GitHub', link: 'https://github.com/vibecook/truffle' },
    ],
    sidebar: {
      '/guide/': [
        {
          text: 'Introduction',
          items: [
            { text: 'Getting Started', link: '/guide/getting-started' },
            { text: 'Architecture', link: '/guide/architecture' },
          ],
        },
        {
          text: 'Core Concepts',
          items: [
            { text: 'Mesh Networking', link: '/guide/mesh-networking' },
            { text: 'Store Sync', link: '/guide/store-sync' },
            { text: 'Examples', link: '/guide/examples' },
          ],
        },
      ],
      '/api/': [
        {
          text: 'API Reference',
          items: [
            { text: 'Overview', link: '/api/' },
            { text: 'MeshNode', link: '/api/mesh-node' },
            { text: 'MessageBus', link: '/api/message-bus' },
            { text: 'StoreSyncAdapter', link: '/api/store-sync' },
            { text: 'React Hooks', link: '/api/react' },
          ],
        },
      ],
    },
    footer: {
      message: 'Released under the MIT License.',
      copyright: 'Copyright vibecook',
    },
  },
});
