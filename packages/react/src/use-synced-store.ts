import { useState, useEffect, useRef } from 'react';
import type { NapiDeviceSlice as DeviceSlice, NapiStoreSyncAdapter } from '@vibecook/truffle';

export interface UseSyncedStoreResult<T> {
  localSlice: DeviceSlice | null;
  localData: T | null;
  version: number;
}

/**
 * React hook for a synced store backed by NapiStoreSyncAdapter.
 *
 * Tracks local slice state and listens for outgoing sync messages.
 * Call `handleLocalChanged` on the adapter when your local data changes,
 * and this hook will reflect the updated slice.
 *
 * @param adapter - The NapiStoreSyncAdapter instance (or null if not ready)
 * @param storeId - The store identifier to track
 * @param initialSlice - Optional initial slice value
 */
export function useSyncedStore<T>(
  adapter: NapiStoreSyncAdapter | null,
  storeId: string,
  initialSlice?: DeviceSlice | null,
): UseSyncedStoreResult<T> {
  const [localSlice, setLocalSlice] = useState<DeviceSlice | null>(initialSlice ?? null);
  const adapterRef = useRef<NapiStoreSyncAdapter | null>(null);

  useEffect(() => {
    if (!adapter) return;

    adapterRef.current = adapter;
    let cancelled = false;

    // Listen for outgoing sync messages to detect local changes
    adapter.onOutgoing((err, message) => {
      if (err || cancelled) return;

      // When the adapter emits an outgoing message, the local data has changed.
      // The payload contains the sync data including the slice.
      if (message.payload && typeof message.payload === 'object') {
        const payload = message.payload as Record<string, unknown>;
        if (payload.storeId === storeId && payload.slice) {
          setLocalSlice(payload.slice as DeviceSlice);
        }
      }
    });

    return () => {
      cancelled = true;
      adapterRef.current = null;
    };
  }, [adapter, storeId]);

  return {
    localSlice,
    localData: (localSlice?.data as T) ?? null,
    version: localSlice?.version ?? 0,
  };
}
