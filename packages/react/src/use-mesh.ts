import { useState, useEffect, useCallback, useRef } from 'react';
import type {
  NapiBaseDevice as BaseDevice,
  NapiMeshNode as MeshNode,
  NapiMeshEvent,
} from '@vibecook/truffle';

export interface UseMeshResult {
  devices: BaseDevice[];
  localDevice: BaseDevice | null;
  isConnected: boolean;
  broadcast: (namespace: string, type: string, payload: unknown) => void;
  sendTo: (deviceId: string, namespace: string, type: string, payload: unknown) => void;
}

/**
 * React hook for Truffle mesh networking (P2P).
 *
 * Subscribes to mesh events via the NAPI callback API and provides messaging helpers.
 * The MeshNode must already be started before using this hook.
 */
export function useMesh(node: MeshNode | null): UseMeshResult {
  const [devices, setDevices] = useState<BaseDevice[]>([]);
  const [localDevice, setLocalDevice] = useState<BaseDevice | null>(null);
  const [isConnected, setIsConnected] = useState(false);
  const nodeRef = useRef<MeshNode | null>(null);

  useEffect(() => {
    if (!node) return;

    nodeRef.current = node;
    let cancelled = false;

    // Fetch initial state
    const init = async () => {
      try {
        const [running, local, devs] = await Promise.all([
          node.isRunning(),
          node.localDevice(),
          node.devices(),
        ]);
        if (cancelled) return;
        setIsConnected(running);
        setLocalDevice(local);
        setDevices(devs);
      } catch {
        // Node may not be started yet
      }
    };
    init();

    // Subscribe to mesh events via NAPI callback
    node.onEvent((err: null | Error, event: NapiMeshEvent) => {
      if (err || cancelled) return;

      switch (event.eventType) {
        case 'started':
          setIsConnected(true);
          node.localDevice().then((d) => {
            if (!cancelled) setLocalDevice(d);
          });
          break;

        case 'stopped':
          setIsConnected(false);
          setDevices([]);
          break;

        case 'peersChanged':
          if (Array.isArray(event.payload)) {
            setDevices(event.payload);
          } else {
            node.devices().then((d) => {
              if (!cancelled) setDevices(d);
            });
          }
          break;

        case 'peerDiscovered':
        case 'peerOffline':
          node.devices().then((d) => {
            if (!cancelled) setDevices(d);
          });
          break;
      }
    });

    return () => {
      cancelled = true;
      nodeRef.current = null;
    };
  }, [node]);

  const broadcast = useCallback((namespace: string, type: string, payload: unknown) => {
    nodeRef.current?.broadcastEnvelope(namespace, type, payload);
  }, []);

  const sendTo = useCallback(
    (deviceId: string, namespace: string, type: string, payload: unknown): void => {
      nodeRef.current?.sendEnvelope(deviceId, namespace, type, payload);
    },
    [],
  );

  return { devices, localDevice, isConnected, broadcast, sendTo };
}
