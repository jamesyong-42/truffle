/**
 * Shared State Example - Todo list synced across devices
 *
 * Demonstrates node.syncedStore() for cross-device state synchronization
 * (RFC 016): each device owns a slice of JSON data, and truffle handles
 * broadcasting it and requesting/merging peers' slices internally — no
 * hand-rolled sync protocol required.
 *
 * Usage:
 *   npx tsx examples/shared-state/index.ts
 *
 * Run on multiple devices (same appId) to see todos sync in real-time:
 * `list` shows your own todos, `peers` shows every device's slice.
 */

import { createMeshNode } from '@vibecook/truffle';
import { createInterface } from 'readline';

interface Todo {
  id: string;
  text: string;
  done: boolean;
}

interface TodoData {
  todos: Todo[];
}

const STORE_ID = 'todos';
const localData: TodoData = { todos: [] };

const rl = createInterface({ input: process.stdin, output: process.stdout });
rl.setPrompt('todo> ');

const mesh = await createMeshNode({
  appId: 'shared-state-example',
  onAuthRequired: (url) => {
    console.log(`Auth required - visit: ${url}`);
  },
});

const store = mesh.syncedStore(STORE_ID);

function printTodos(): void {
  if (localData.todos.length === 0) {
    console.log('  (no todos)');
    return;
  }
  localData.todos.forEach((t, i) => {
    console.log(`  ${i}. ${t.done ? '[x]' : '[ ]'} ${t.text}`);
  });
}

/** Push the current local data to the store (syncs to peers) and reprint. */
async function syncLocal(): Promise<void> {
  await store.set(localData);
  console.log('\nTodos:');
  printTodos();
  rl.prompt();
}

async function addTodo(text: string): Promise<void> {
  const id = `${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
  localData.todos.push({ id, text, done: false });
  await syncLocal();
}

async function toggleTodo(index: number): Promise<void> {
  const todo = localData.todos[index];
  if (todo) {
    todo.done = !todo.done;
    await syncLocal();
  }
}

async function removeTodo(index: number): Promise<void> {
  if (index >= 0 && index < localData.todos.length) {
    localData.todos.splice(index, 1);
    await syncLocal();
  }
}

console.log('Todo app started. Commands: add <text>, toggle <n>, rm <n>, list, peers, quit');
rl.prompt();

rl.on('line', async (line) => {
  const parts = line.trim().split(/\s+/);
  const cmd = parts[0]?.toLowerCase();

  switch (cmd) {
    case 'add':
      await addTodo(parts.slice(1).join(' '));
      return;
    case 'toggle':
      await toggleTodo(parseInt(parts[1], 10));
      return;
    case 'rm':
    case 'remove':
      await removeTodo(parseInt(parts[1], 10));
      return;
    case 'list':
      console.log('\nTodos:');
      printTodos();
      break;
    case 'peers': {
      const localId = mesh.getLocalInfo().deviceId;
      const slices = await store.all();
      console.log('\nAll devices:');
      for (const slice of slices) {
        const data = slice.data as TodoData | null;
        const who = slice.deviceId === localId ? `${slice.deviceId} (you)` : slice.deviceId;
        console.log(`  ${who} (v${slice.version}): ${data?.todos.length ?? 0} todo(s)`);
      }
      break;
    }
    case 'quit':
    case 'exit':
      rl.close();
      return;
    default:
      console.log('Commands: add <text>, toggle <n>, rm <n>, list, peers, quit');
  }
  rl.prompt();
});

rl.on('close', async () => {
  await store.stop();
  await mesh.stop();
  process.exit(0);
});
