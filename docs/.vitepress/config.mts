import { defineConfig } from 'vitepress';

export default defineConfig({
  title: 'Truffle',
  description: 'Mesh networking for your devices, built on Tailscale',
  base: '/truffle/',
  srcExclude: ['rfcs/**', 'cross-platform-*.md', 'tailscale-*.md', 'cli-design.md'],
  themeConfig: {
    nav: [
      { text: 'Guide', link: '/guide/getting-started' },
      { text: 'CLI', link: '/guide/cli' },
      { text: 'API', link: '/api/' },
      { text: 'Install', link: '/guide/install' },
      { text: 'GitHub', link: 'https://github.com/jamesyong-42/truffle' },
    ],
    sidebar: {
      '/guide/': [
        {
          text: 'Introduction',
          items: [
            { text: 'Getting Started', link: '/guide/getting-started' },
            { text: 'Installation', link: '/guide/install' },
            { text: 'Architecture', link: '/guide/architecture' },
          ],
        },
        {
          text: 'CLI',
          items: [
            { text: 'CLI Quick Start', link: '/guide/cli' },
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
