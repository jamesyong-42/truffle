import { useCallback, useEffect, useState } from 'react';
import type { Peer, PeerEvent } from '@shared/ipc';

interface UsePeersResult {
  peers: Peer[];
  refresh: () => Promise<void>;
}

/**
 * Live peer list sourced from the main process.
 *
 * On mount: pulls the initial list via `getPeers()`, then subscribes to
 * `onPeerEvent` and applies incremental mutations:
 *   - joined / updated  → upsert peer
 *   - left              → remove peer
 *   - ws_connected      → update in place (wsConnected = true)
 *   - ws_disconnected   → update in place (wsConnected = false)
 */
export function usePeers(): UsePeersResult {
  const [peers, setPeers] = useState<Peer[]>([]);

  const refresh = useCallback(async () => {
    const next = await window.truffle.getPeers();
    setPeers(next);
  }, []);

  useEffect(() => {
    let cancelled = false;

    void window.truffle.getPeers().then((initial) => {
      if (!cancelled) setPeers(initial);
    });

    const unsub = window.truffle.onPeerEvent((event: PeerEvent) => {
      setPeers((current) => {
        const idx = current.findIndex((p) => p.id === event.peerId);
        switch (event.eventType) {
          case 'joined':
          case 'updated': {
            if (!event.peer) return current;
            if (idx >= 0) {
              const next = current.slice();
              next[idx] = event.peer;
              return next;
            }
            return [...current, event.peer];
          }
          case 'left': {
            if (idx < 0) return current;
            return current.filter((p) => p.id !== event.peerId);
          }
          case 'ws_connected': {
            if (idx < 0) {
              if (event.peer) return [...current, event.peer];
              return current;
            }
            const next = current.slice();
            next[idx] = { ...next[idx], wsConnected: true };
            return next;
          }
          case 'ws_disconnected': {
            if (idx < 0) return current;
            const next = current.slice();
            next[idx] = { ...next[idx], wsConnected: false };
            return next;
          }
          default:
            return current;
        }
      });
    });

    return () => {
      cancelled = true;
      unsub();
    };
  }, []);

  return { peers, refresh };
}
