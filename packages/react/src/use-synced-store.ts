import { useState, useEffect, useCallback, useRef } from 'react';
import type { NapiNode, NapiNamespacedMessage } from '@vibecook/truffle';

/** A device-owned slice of data. */
export interface Slice<T> {
  deviceId: string;
  data: T;
  version: number;
  updatedAt: number;
}

/** Events emitted by the synced store. */
export type StoreEvent<T> =
  | { type: 'local_changed'; data: T }
  | { type: 'peer_updated'; deviceId: string; data: T; version: number }
  | { type: 'peer_removed'; deviceId: string };

export interface UseSyncedStoreResult<T> {
  /** This device's current data, or null if not yet set. */
  localData: T | null;
  /** All device slices (local + remote), keyed by device ID. */
  allSlices: Map<string, Slice<T>>;
  /** List of device IDs with data in this store. */
  deviceIds: string[];
  /** Current local version number. */
  version: number;
  /** Update this device's data (broadcasts automatically). */
  set: (data: T) => void;
}

/**
 * React hook for a Truffle synced store.
 *
 * Provides reactive access to device-owned synchronized state.
 * Built on the Node API's namespace messaging — syncs automatically
 * when peers join or leave.
 *
 * This hook implements the client-side sync protocol (RFC 016) over
 * the namespace `"ss:{storeId}"` using `node.onMessage()` and
 * `node.broadcast()`.
 *
 * @example
 * ```tsx
 * const { localData, allSlices, set } = useSyncedStore<MyState>(node, 'app-state');
 *
 * // Update local state (broadcasts to all peers)
 * set({ activeProject: 'truffle', status: 'coding' });
 *
 * // Read all peers' state
 * for (const [deviceId, slice] of allSlices) {
 *   console.log(`${deviceId}: ${slice.data.activeProject}`);
 * }
 * ```
 */
export function useSyncedStore<T>(
  node: NapiNode | null,
  storeId: string,
): UseSyncedStoreResult<T> {
  const [localData, setLocalData] = useState<T | null>(null);
  const [allSlices, setAllSlices] = useState<Map<string, Slice<T>>>(new Map());
  const [version, setVersion] = useState(0);
  const nodeRef = useRef<NapiNode | null>(null);
  const versionRef = useRef(0);
  const deviceIdRef = useRef<string>('');

  const namespace = `ss:${storeId}`;

  useEffect(() => {
    if (!node) {
      setLocalData(null);
      setAllSlices(new Map());
      setVersion(0);
      return;
    }

    nodeRef.current = node;
    let cancelled = false;

    try {
      deviceIdRef.current = node.getLocalInfo().id;
    } catch {
      return;
    }

    // Listen for sync messages from peers
    node.onMessage(namespace, (msg: NapiNamespacedMessage) => {
      if (cancelled) return;

      const payload = msg.payload as Record<string, unknown>;
      const type = payload?.type as string;

      if (type === 'update' || type === 'full') {
        const deviceId = payload.device_id as string;
        if (deviceId === deviceIdRef.current) return; // ignore echo

        const incomingVersion = payload.version as number;
        const data = payload.data as T;
        const updatedAt = payload.updated_at as number;

        setAllSlices((prev) => {
          const existing = prev.get(deviceId);
          if (existing && incomingVersion <= existing.version) return prev; // stale
          const next = new Map(prev);
          next.set(deviceId, { deviceId, data, version: incomingVersion, updatedAt });
          return next;
        });
      } else if (type === 'request') {
        // Peer wants our data — send it
        if (localData !== null) {
          const full = {
            type: 'full',
            device_id: deviceIdRef.current,
            data: localData,
            version: versionRef.current,
            updated_at: Date.now(),
          };
          const buf = Buffer.from(JSON.stringify(full));
          node.send(msg.from, namespace, buf).catch(() => {});
        }
      } else if (type === 'clear') {
        const deviceId = payload.device_id as string;
        setAllSlices((prev) => {
          if (!prev.has(deviceId)) return prev;
          const next = new Map(prev);
          next.delete(deviceId);
          return next;
        });
      }
    });

    // Listen for peer join/leave to trigger sync
    node.onPeerChange((event) => {
      if (cancelled) return;

      if (event.eventType === 'joined') {
        // Request new peer's data
        const request = { type: 'request' };
        const buf = Buffer.from(JSON.stringify(request));
        node.send(event.peerId, namespace, buf).catch(() => {});

        // Send our data to the new peer
        if (localData !== null) {
          const full = {
            type: 'full',
            device_id: deviceIdRef.current,
            data: localData,
            version: versionRef.current,
            updated_at: Date.now(),
          };
          const buf = Buffer.from(JSON.stringify(full));
          node.send(event.peerId, namespace, buf).catch(() => {});
        }
      } else if (event.eventType === 'left') {
        setAllSlices((prev) => {
          if (!prev.has(event.peerId)) return prev;
          const next = new Map(prev);
          next.delete(event.peerId);
          return next;
        });
      }
    });

    return () => {
      cancelled = true;
      nodeRef.current = null;
    };
  }, [node, storeId]); // eslint-disable-line react-hooks/exhaustive-deps

  const set = useCallback(
    (data: T) => {
      if (!nodeRef.current) return;

      const newVersion = versionRef.current + 1;
      versionRef.current = newVersion;
      setVersion(newVersion);
      setLocalData(data);

      // Update local slice in allSlices
      const deviceId = deviceIdRef.current;
      const now = Date.now();
      setAllSlices((prev) => {
        const next = new Map(prev);
        next.set(deviceId, { deviceId, data, version: newVersion, updatedAt: now });
        return next;
      });

      // Broadcast to all peers
      const update = {
        type: 'update',
        device_id: deviceId,
        data,
        version: newVersion,
        updated_at: now,
      };
      const buf = Buffer.from(JSON.stringify(update));
      nodeRef.current.broadcast(namespace, buf).catch(() => {});
    },
    [namespace],
  );

  return {
    localData,
    allSlices,
    deviceIds: Array.from(allSlices.keys()),
    version,
    set,
  };
}
