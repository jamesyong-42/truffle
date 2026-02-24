import { defineCommand } from 'citty';
import { consola } from 'consola';
import { writeFile, mkdir, access } from 'node:fs/promises';
import { join } from 'node:path';

const TRUFFLE_CONFIG_TEMPLATE = `import type { MeshNodeConfig } from '@vibecook/truffle';

const config: Partial<MeshNodeConfig> = {
  // Unique device ID (auto-generated at runtime if not set)
  // deviceId: 'my-device',

  // Human-readable device name
  deviceName: 'my-device',

  // Device type (e.g., 'desktop', 'mobile', 'server')
  deviceType: 'desktop',

  // Hostname prefix for peer discovery on the tailnet
  hostnamePrefix: 'myapp',

  // Path to the Truffle sidecar binary
  sidecarPath: './sidecar',

  // Directory to store Tailscale state
  stateDir: './.truffle-state',
};

export default config;
`;

export const initCommand = defineCommand({
  meta: {
    name: 'init',
    description: 'Initialize a Truffle project with config file',
  },
  args: {
    dir: {
      type: 'positional',
      description: 'Project directory',
      default: '.',
    },
  },
  async run({ args }) {
    const dir = args.dir;
    const configPath = join(dir, 'truffle.config.ts');

    try {
      await access(configPath);
      consola.warn(`Config file already exists: ${configPath}`);
      return;
    } catch {
      // File doesn't exist, proceed
    }

    await mkdir(dir, { recursive: true });
    await writeFile(configPath, TRUFFLE_CONFIG_TEMPLATE, 'utf-8');
    consola.success(`Created ${configPath}`);

    const stateDir = join(dir, '.truffle-state');
    await mkdir(stateDir, { recursive: true });
    consola.success(`Created ${stateDir}/`);

    consola.info('');
    consola.info('Next steps:');
    consola.info('  1. Edit truffle.config.ts with your settings');
    consola.info('  2. Run: npx @vibecook/truffle-cli dev');
  },
});
