import { useState, useEffect, useCallback, useRef } from 'react';
import type { NapiNode, NapiPeer, NapiPeerEvent, NapiNodeIdentity } from '@vibecook/truffle';

export interface UseMeshResult {
  /** All known peers (from Tailscale discovery). */
  peers: NapiPeer[];
  /** Local node identity (id, hostname, name, ip). */
  localInfo: NapiNodeIdentity | null;
  /** Whether the node has been started. */
  isStarted: boolean;
  /** Send a namespaced JSON message to a specific peer. */
  send: (peerId: string, namespace: string, payload: unknown) => Promise<void>;
  /** Broadcast a namespaced JSON message to all connected peers. */
  broadcast: (namespace: string, payload: unknown) => Promise<void>;
}

/**
 * React hook for Truffle mesh networking.
 *
 * Provides reactive peer state and messaging helpers built on the new
 * Node API (RFC 012). The NapiNode must already be started.
 *
 * @example
 * ```tsx
 * const { peers, send, broadcast } = useMesh(node);
 *
 * await send('peer-id', 'chat', { text: 'hello' });
 * await broadcast('chat', { text: 'hello everyone' });
 * ```
 */
export function useMesh(node: NapiNode | null): UseMeshResult {
  const [peers, setPeers] = useState<NapiPeer[]>([]);
  const [localInfo, setLocalInfo] = useState<NapiNodeIdentity | null>(null);
  const [isStarted, setIsStarted] = useState(false);
  const nodeRef = useRef<NapiNode | null>(null);

  useEffect(() => {
    if (!node) {
      setIsStarted(false);
      setLocalInfo(null);
      setPeers([]);
      return;
    }

    nodeRef.current = node;
    let cancelled = false;

    // Fetch initial state
    const init = async () => {
      try {
        const info = node.getLocalInfo();
        if (!cancelled) {
          setLocalInfo(info);
          setIsStarted(true);
        }

        const currentPeers = await node.getPeers();
        if (!cancelled) setPeers(currentPeers);
      } catch {
        if (!cancelled) setIsStarted(false);
      }
    };
    init();

    // Subscribe to peer change events
    node.onPeerChange((event: NapiPeerEvent) => {
      if (cancelled) return;

      switch (event.eventType) {
        case 'joined':
          if (event.peer) {
            setPeers((prev) => {
              const idx = prev.findIndex((p) => p.deviceId === event.peer!.deviceId);
              if (idx >= 0) {
                const next = [...prev];
                next[idx] = event.peer!;
                return next;
              }
              return [...prev, event.peer!];
            });
          }
          break;

        case 'left':
          setPeers((prev) => prev.filter((p) => p.deviceId !== event.peerId));
          break;

        case 'updated':
          if (event.peer) {
            setPeers((prev) =>
              prev.map((p) => (p.deviceId === event.peer!.deviceId ? event.peer! : p)),
            );
          }
          break;

        case 'ws_connected':
          setPeers((prev) =>
            prev.map((p) => (p.deviceId === event.peerId ? { ...p, wsConnected: true } : p)),
          );
          break;

        case 'ws_disconnected':
          setPeers((prev) =>
            prev.map((p) => (p.deviceId === event.peerId ? { ...p, wsConnected: false } : p)),
          );
          break;
      }
    });

    return () => {
      cancelled = true;
      nodeRef.current = null;
    };
  }, [node]);

  const send = useCallback(async (peerId: string, namespace: string, payload: unknown) => {
    if (!nodeRef.current) return;
    const data = Buffer.from(JSON.stringify(payload));
    await nodeRef.current.send(peerId, namespace, data);
  }, []);

  const broadcast = useCallback(async (namespace: string, payload: unknown) => {
    if (!nodeRef.current) return;
    const data = Buffer.from(JSON.stringify(payload));
    await nodeRef.current.broadcast(namespace, data);
  }, []);

  return { peers, localInfo, isStarted, send, broadcast };
}
