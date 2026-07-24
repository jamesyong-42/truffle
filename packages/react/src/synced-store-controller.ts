import type { Slice } from './use-synced-store.js';

export interface DisposableSubscription {
  close(): void;
}

export interface NativeSlice<T> {
  deviceId: string;
  data: T;
  version: number;
  updatedAt: number;
}

export interface SyncedStoreLike<T> {
  set(data: T): Promise<void>;
  local(): Promise<T | null>;
  all(): Promise<Array<NativeSlice<T>>>;
  version(): number;
  onChange(callback: (event: unknown) => void): DisposableSubscription;
  stop(): Promise<void>;
}

export interface StoreSnapshot<T> {
  localData: T | null;
  allSlices: Map<string, Slice<T>>;
  version: number;
}

export interface SyncedStoreController {
  refresh(): Promise<void>;
  close(): Promise<void>;
}

export function slicesToMap<T>(slices: Array<NativeSlice<T>>): Map<string, Slice<T>> {
  return new Map(
    slices.map((slice) => [
      slice.deviceId,
      {
        deviceId: slice.deviceId,
        data: slice.data,
        version: slice.version,
        updatedAt: slice.updatedAt,
      },
    ]),
  );
}

/**
 * Keep a React-facing snapshot current while owning the native subscription
 * and store lifetime. Exported for deterministic lifecycle tests.
 */
export function subscribeSyncedStore<T>(
  store: SyncedStoreLike<T>,
  onSnapshot: (snapshot: StoreSnapshot<T>) => void,
  onError: (error: unknown) => void = () => {},
): SyncedStoreController {
  let closed = false;
  let requestedRefresh = 0;

  const refresh = async (): Promise<void> => {
    const request = ++requestedRefresh;
    try {
      const [localData, slices] = await Promise.all([store.local(), store.all()]);
      if (closed || request !== requestedRefresh) return;
      onSnapshot({
        localData,
        allSlices: slicesToMap(slices),
        version: store.version(),
      });
    } catch (error) {
      if (!closed && request === requestedRefresh) onError(error);
    }
  };

  const subscription = store.onChange(() => {
    void refresh();
  });
  void refresh();

  return {
    refresh,
    async close(): Promise<void> {
      if (closed) return;
      closed = true;
      subscription.close();
      try {
        await store.stop();
      } catch (error) {
        onError(error);
      }
    },
  };
}
