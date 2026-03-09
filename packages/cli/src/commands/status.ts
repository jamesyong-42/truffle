import { defineCommand } from 'citty';
import { consola } from 'consola';
import { readFile, access } from 'node:fs/promises';
import { join } from 'node:path';
import { resolveSidecarPath } from '@vibecook/truffle';

export const statusCommand = defineCommand({
  meta: {
    name: 'status',
    description: 'Show Truffle mesh status and configuration',
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

    consola.info('Truffle Mesh Status');
    consola.info('='.repeat(40));

    // Check for config file
    const configPath = join(dir, 'truffle.config.ts');
    try {
      await access(configPath);
      consola.success(`Config: ${configPath}`);
    } catch {
      consola.warn('No truffle.config.ts found. Run `truffle init` first.');
    }

    // Check state directory
    const stateDir = join(dir, '.truffle-state');
    try {
      await access(stateDir);
      consola.success(`State dir: ${stateDir}`);
    } catch {
      consola.warn(`State dir not found: ${stateDir}`);
    }

    // Check for sidecar binary (auto-resolved from platform package)
    try {
      const sidecarPath = resolveSidecarPath();
      await access(sidecarPath);
      consola.success(`Sidecar: ${sidecarPath}`);
    } catch {
      consola.warn('Sidecar binary not found');
      consola.info(
        'Reinstall @vibecook/truffle or build from source: cd packages/sidecar-slim && go build',
      );
    }

    // Check for package.json dependencies
    const pkgPath = join(dir, 'package.json');
    try {
      const pkg = JSON.parse(await readFile(pkgPath, 'utf-8'));
      const deps = { ...pkg.dependencies, ...pkg.devDependencies };
      const truffleDeps = Object.keys(deps).filter((k) => k.startsWith('@vibecook/truffle'));
      if (truffleDeps.length > 0) {
        consola.success(`Truffle packages: ${truffleDeps.join(', ')}`);
      }
    } catch {
      // No package.json or parse error
    }
  },
});
