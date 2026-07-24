const test = require('node:test');
const assert = require('node:assert/strict');

async function loadController() {
  return import('../dist/synced-store-controller.js');
}

function fakeStore() {
  let local = null;
  let version = 0;
  let slices = [];
  let listener = null;
  let subscriptionCloses = 0;
  let stops = 0;

  return {
    store: {
      async set(data) {
        local = data;
        version += 1;
        slices = [
          {
            deviceId: 'local',
            data,
            version,
            updatedAt: Date.now(),
          },
        ];
        listener?.({});
      },
      async local() {
        return local;
      },
      async all() {
        return slices;
      },
      version() {
        return version;
      },
      onChange(callback) {
        listener = callback;
        return {
          close() {
            subscriptionCloses += 1;
            listener = null;
          },
        };
      },
      async stop() {
        stops += 1;
      },
    },
    emit() {
      listener?.({});
    },
    counts() {
      return { subscriptionCloses, stops };
    },
  };
}

test('slicesToMap keys snapshots by durable device id', async () => {
  const { slicesToMap } = await loadController();
  const map = slicesToMap([
    { deviceId: 'device-a', data: { online: true }, version: 3, updatedAt: 42 },
  ]);
  assert.deepEqual(map.get('device-a'), {
    deviceId: 'device-a',
    data: { online: true },
    version: 3,
    updatedAt: 42,
  });
});

test('controller refreshes current native state and disposes both resources', async () => {
  const { subscribeSyncedStore } = await loadController();
  const fake = fakeStore();
  const snapshots = [];
  const controller = subscribeSyncedStore(fake.store, (snapshot) => snapshots.push(snapshot));

  await controller.refresh();
  await fake.store.set({ count: 1 });
  await controller.refresh();

  assert.equal(snapshots.at(-1).localData.count, 1);
  assert.equal(snapshots.at(-1).version, 1);
  assert.equal(snapshots.at(-1).allSlices.get('local').data.count, 1);

  await controller.close();
  fake.emit();
  await Promise.resolve();

  assert.deepEqual(fake.counts(), { subscriptionCloses: 1, stops: 1 });
  assert.equal(snapshots.at(-1).version, 1);
});

test('controller suppresses an in-flight snapshot after disposal', async () => {
  let resolveLocal;
  let resolveSlices;
  const local = new Promise((resolve) => {
    resolveLocal = resolve;
  });
  const slices = new Promise((resolve) => {
    resolveSlices = resolve;
  });
  const snapshots = [];
  const store = {
    async set() {},
    local() {
      return local;
    },
    all() {
      return slices;
    },
    version() {
      return 1;
    },
    onChange() {
      return { close() {} };
    },
    async stop() {},
  };

  const { subscribeSyncedStore } = await loadController();
  const controller = subscribeSyncedStore(store, (snapshot) => snapshots.push(snapshot));
  const pending = controller.refresh();
  await controller.close();
  resolveLocal({ count: 1 });
  resolveSlices([]);
  await pending;

  assert.deepEqual(snapshots, []);
});
