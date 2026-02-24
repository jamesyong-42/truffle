#!/usr/bin/env node
import { defineCommand, runMain } from 'citty';
import { initCommand } from './commands/init.js';
import { devCommand } from './commands/dev.js';
import { statusCommand } from './commands/status.js';

const main = defineCommand({
  meta: {
    name: 'truffle',
    version: '0.1.0',
    description: 'CLI for Truffle mesh networking',
  },
  subCommands: {
    init: initCommand,
    dev: devCommand,
    status: statusCommand,
  },
});

runMain(main);
