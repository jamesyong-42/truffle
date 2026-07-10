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
 * `onPeerEvent` and applies incremental mutations, keyed on the stable
 * `peerRef` handle token (RFC 022) — never `deviceId`, which is a nullable
 * ULID:
 *   - joined / updated / identity → upsert peer (identity fills in deviceId)
 *   - left                        → remove peer
 *   - ws_connected                → upsert / mark wsConnected = true
 *   - ws_disconnected             → mark wsConnected = false
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
        const evPeer = event.peer;
        // Match on the stable `peerRef` handle token when we have the peer
        // view, else the Tailscale routing key (`event.peerId`). Never
        // `deviceId` — it is a nullable ULID (RFC 022).
        const idx = evPeer
          ? current.findIndex((p) => p.peerRef === evPeer.peerRef)
          : current.findIndex((p) => p.tailscaleId === event.peerId);
        switch (event.eventType) {
          case 'joined':
          case 'updated':
          case 'identity': {
            if (!evPeer) return current;
            if (idx >= 0) {
              const next = current.slice();
              next[idx] = evPeer;
              return next;
            }
            return [...current, evPeer];
          }
          case 'left': {
            if (idx < 0) return current;
            const next = current.slice();
            next.splice(idx, 1);
            return next;
          }
          case 'ws_connected': {
            if (idx < 0) return evPeer ? [...current, evPeer] : current;
            const next = current.slice();
            next[idx] = evPeer ?? { ...next[idx], wsConnected: true };
            return next;
          }
          case 'ws_disconnected': {
            if (idx < 0) return current;
            const next = current.slice();
            next[idx] = evPeer ?? { ...next[idx], wsConnected: false };
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
