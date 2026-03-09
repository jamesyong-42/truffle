/**
 * Shared State Example - Todo list synced across devices
 *
 * Demonstrates NapiStoreSyncAdapter for cross-device state synchronization.
 *
 * Usage:
 *   npx tsx examples/shared-state/index.ts
 *
 * Run on multiple devices to see todos sync in real-time.
 */

import { NapiMeshNode, NapiStoreSyncAdapter, resolveSidecarPath } from '@vibecook/truffle';
import type { NapiMeshEvent, NapiDeviceSlice, NapiOutgoingSyncMessage } from '@vibecook/truffle';
import { createInterface } from 'readline';

// ─────────────────────────────────────────────────────────────────────────
// Local todo state
// ─────────────────────────────────────────────────────────────────────────

interface TodoData {
  todos: Array<{ id: string; text: string; done: boolean }>;
}

const deviceId = `todo-${Date.now()}`;
let localData: TodoData = { todos: [] };
let version = 0;

function getSlice(): NapiDeviceSlice {
  return {
    deviceId,
    data: localData as unknown as Record<string, unknown>,
    updatedAt: Date.now(),
    version,
  };
}

function addTodo(text: string): void {
  const id = `${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
  localData.todos.push({ id, text, done: false });
  version++;
  onLocalChanged();
}

function toggleTodo(index: number): void {
  if (index >= 0 && index < localData.todos.length) {
    localData.todos[index].done = !localData.todos[index].done;
    version++;
    onLocalChanged();
  }
}

function removeTodo(index: number): void {
  if (index >= 0 && index < localData.todos.length) {
    localData.todos.splice(index, 1);
    version++;
    onLocalChanged();
  }
}

function printTodos(): void {
  if (localData.todos.length === 0) {
    console.log('  (no todos)');
    return;
  }
  localData.todos.forEach((t, i) => {
    const check = t.done ? '[x]' : '[ ]';
    console.log(`  ${i}. ${check} ${t.text}`);
  });
}

// ─────────────────────────────────────────────────────────────────────────
// Set up mesh + sync
// ─────────────────────────────────────────────────────────────────────────

const node = new NapiMeshNode({
  deviceId,
  deviceName: `Todo App (${deviceId.slice(-4)})`,
  deviceType: 'desktop',
  hostnamePrefix: 'truffle-todo',
  sidecarPath: resolveSidecarPath(),
  stateDir: `./tmp/todo-state-${deviceId}`,
});

const sync = new NapiStoreSyncAdapter({ localDeviceId: deviceId });

// When local state changes, notify the sync adapter
function onLocalChanged(): void {
  sync.handleLocalChanged('todos', getSlice());
  console.log('\nTodos:');
  printTodos();
  rl.prompt();
}

// Relay outgoing sync messages to the mesh
sync.onOutgoing((_err: null | Error, msg: NapiOutgoingSyncMessage) => {
  node.broadcastEnvelope('sync', msg.msgType, msg.payload);
});

// Subscribe to mesh events
node.onEvent(async (err: null | Error, event: NapiMeshEvent) => {
  if (err) return;

  switch (event.eventType) {
    case 'started':
      console.log('Todo app started. Commands: add <text>, toggle <n>, rm <n>, list, quit');
      await sync.start();
      rl.prompt();
      break;

    case 'authRequired':
      console.log(`Auth required - visit: ${event.payload}`);
      break;

    case 'deviceDiscovered':
      if (event.deviceId) {
        await sync.handleDeviceDiscovered(event.deviceId);
      }
      break;

    case 'deviceOffline':
      if (event.deviceId) {
        await sync.handleDeviceOffline(event.deviceId);
      }
      break;

    case 'message': {
      // Route sync-namespace messages to the adapter
      if (event.payload && typeof event.payload === 'object') {
        const p = event.payload as Record<string, unknown>;
        if (p.namespace === 'sync') {
          await sync.handleSyncMessage(
            event.deviceId ?? undefined,
            p.type as string,
            p.payload as Record<string, unknown>,
          );
        }
      }
      break;
    }
  }
});

await node.start();

// ─────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────

const rl = createInterface({ input: process.stdin, output: process.stdout });
rl.setPrompt('todo> ');

rl.on('line', (line) => {
  const parts = line.trim().split(/\s+/);
  const cmd = parts[0]?.toLowerCase();

  switch (cmd) {
    case 'add':
      addTodo(parts.slice(1).join(' '));
      break;
    case 'toggle':
      toggleTodo(parseInt(parts[1], 10));
      break;
    case 'rm':
    case 'remove':
      removeTodo(parseInt(parts[1], 10));
      break;
    case 'list':
      console.log('\nTodos:');
      printTodos();
      break;
    case 'quit':
    case 'exit':
      rl.close();
      return;
    default:
      console.log('Commands: add <text>, toggle <n>, rm <n>, list, quit');
  }
  rl.prompt();
});

rl.on('close', async () => {
  await sync.dispose();
  await node.stop();
  process.exit(0);
});
