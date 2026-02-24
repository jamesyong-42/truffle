import { useState, useEffect } from 'react';
import type { DeviceSlice } from '@vibecook/truffle-types';
import type { ISyncableStore } from '@vibecook/truffle-store-sync';

export interface UseSyncedStoreResult<T> {
  localSlice: DeviceSlice<T> | null;
  localData: T | null;
  version: number;
}

/**
 * React hook for a synced store.
 *
 * Subscribes to store changes and returns the current local slice data.
 * Use with StoreSyncAdapter for cross-device synchronization.
 */
export function useSyncedStore<T>(store: ISyncableStore<T> | null): UseSyncedStoreResult<T> {
  const [localSlice, setLocalSlice] = useState<DeviceSlice<T> | null>(
    store?.getLocalSlice() ?? null,
  );

  useEffect(() => {
    if (!store) return;

    setLocalSlice(store.getLocalSlice());

    const onLocalChanged = (slice: DeviceSlice<T>) => {
      setLocalSlice(slice);
    };

    store.on('localChanged', onLocalChanged);

    return () => {
      store.off('localChanged', onLocalChanged);
    };
  }, [store]);

  return {
    localSlice,
    localData: localSlice?.data ?? null,
    version: localSlice?.version ?? 0,
  };
}
