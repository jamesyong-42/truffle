import { useCallback, useEffect, useState } from 'react';
import type { StoreSlice } from '@shared/ipc';

interface UseStoreResult {
  slices: StoreSlice[];
  localKv: Record<string, string>;
  localSlice: StoreSlice | null;
  setKey: (key: string, value: string) => Promise<void>;
  unsetKey: (key: string) => Promise<void>;
  refresh: () => Promise<void>;
}

/**
 * SyncedStore state — local slice plus all peer slices. The main process
 * emits events for local_changed, peer_updated, and peer_removed; on any
 * of those we refetch the full snapshot (simplest + correct).
 */
export function useStore(): UseStoreResult {
  const [slices, setSlices] = useState<StoreSlice[]>([]);
  const [localSlice, setLocalSlice] = useState<StoreSlice | null>(null);

  const refresh = useCallback(async () => {
    const [all, local] = await Promise.all([
      window.truffle.storeAll(),
      window.truffle.storeGet(),
    ]);
    setSlices(all);
    setLocalSlice(local);
  }, []);

  useEffect(() => {
    void refresh();
    const unsub = window.truffle.onStoreEvent(() => {
      void refresh();
    });
    return () => unsub();
  }, [refresh]);

  const setKey = useCallback(async (key: string, value: string) => {
    await window.truffle.storeSet(key, value);
  }, []);

  const unsetKey = useCallback(async (key: string) => {
    await window.truffle.storeUnset(key);
  }, []);

  const localKv = localSlice?.data.kv ?? {};

  return { slices, localKv, localSlice, setKey, unsetKey, refresh };
}
