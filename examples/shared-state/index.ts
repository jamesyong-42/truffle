/**
 * Shared State Example - Todo list synced across devices
 *
 * Demonstrates StoreSyncAdapter for cross-device state synchronization.
 *
 * Usage:
 *   npx tsx examples/shared-state/index.ts
 *
 * Run on multiple devices to see todos sync in real-time.
 */

import { createMeshNode, StoreSyncAdapter } from '@vibecook/truffle';
import type { ISyncableStore } from '@vibecook/truffle';
import { EventEmitter } from 'events';
import { createInterface } from 'readline';

// ─────────────────────────────────────────────────────────────────────────
// Define a simple todo store
// ─────────────────────────────────────────────────────────────────────────

interface TodoData {
  todos: Array<{ id: string; text: string; done: boolean }>;
}

class TodoStore extends EventEmitter implements ISyncableStore<TodoData> {
  readonly storeId = 'todos';
  private data: TodoData = { todos: [] };
  private version = 0;

  getData(): TodoData {
    return { ...this.data, todos: [...this.data.todos] };
  }

  getVersion(): number {
    return this.version;
  }

  applyRemoteData(data: TodoData, version: number): void {
    this.data = data;
    this.version = Math.max(this.version, version);
    this.emit('changed', this.storeId);
  }

  addTodo(text: string): void {
    const id = `${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
    this.data.todos.push({ id, text, done: false });
    this.version++;
    this.emit('changed', this.storeId);
  }

  toggleTodo(index: number): void {
    if (index >= 0 && index < this.data.todos.length) {
      this.data.todos[index].done = !this.data.todos[index].done;
      this.version++;
      this.emit('changed', this.storeId);
    }
  }

  removeTodo(index: number): void {
    if (index >= 0 && index < this.data.todos.length) {
      this.data.todos.splice(index, 1);
      this.version++;
      this.emit('changed', this.storeId);
    }
  }

  printTodos(): void {
    if (this.data.todos.length === 0) {
      console.log('  (no todos)');
      return;
    }
    this.data.todos.forEach((t, i) => {
      const check = t.done ? '[x]' : '[ ]';
      console.log(`  ${i}. ${check} ${t.text}`);
    });
  }
}

// ─────────────────────────────────────────────────────────────────────────
// Set up mesh + sync
// ─────────────────────────────────────────────────────────────────────────

const deviceId = `todo-${Date.now()}`;

const node = createMeshNode({
  deviceId,
  deviceName: `Todo App (${deviceId.slice(-4)})`,
  deviceType: 'desktop',
  hostnamePrefix: 'truffle-todo',
  sidecarPath: './packages/sidecar/bin/tsnet-sidecar',
  stateDir: `./tmp/todo-state-${deviceId}`,
});

const store = new TodoStore();
const bus = node.getMessageBus();

const sync = new StoreSyncAdapter({
  localDeviceId: deviceId,
  messageBus: bus,
  stores: [store],
});

store.on('changed', () => {
  console.log('\nTodos:');
  store.printTodos();
  rl.prompt();
});

node.on('started', () => {
  console.log('Todo app started. Commands: add <text>, toggle <n>, rm <n>, list, quit');
  sync.start();
});

node.on('authRequired', (url) => {
  console.log(`Auth required - visit: ${url}`);
});

await node.start();

// ─────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────

const rl = createInterface({ input: process.stdin, output: process.stdout });
rl.setPrompt('todo> ');
rl.prompt();

rl.on('line', (line) => {
  const parts = line.trim().split(/\s+/);
  const cmd = parts[0]?.toLowerCase();

  switch (cmd) {
    case 'add':
      store.addTodo(parts.slice(1).join(' '));
      break;
    case 'toggle':
      store.toggleTodo(parseInt(parts[1], 10));
      break;
    case 'rm':
    case 'remove':
      store.removeTodo(parseInt(parts[1], 10));
      break;
    case 'list':
      console.log('\nTodos:');
      store.printTodos();
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
  sync.dispose();
  await node.stop();
  process.exit(0);
});
