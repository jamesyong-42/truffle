import { useState, useEffect, useCallback, useRef } from 'react';
import type { NapiBaseDevice as BaseDevice, NapiMeshNode as MeshNode, NapiMeshEvent } from '@vibecook/truffle';

type DeviceRole = string;

export interface UseMeshResult {
  devices: BaseDevice[];
  localDevice: BaseDevice | null;
  isPrimary: boolean;
  isConnected: boolean;
  role: DeviceRole;
  broadcast: (namespace: string, type: string, payload: unknown) => void;
  sendTo: (deviceId: string, namespace: string, type: string, payload: unknown) => void;
}

/**
 * React hook for Truffle mesh networking.
 *
 * Subscribes to mesh events via the NAPI callback API and provides messaging helpers.
 * The MeshNode must already be started before using this hook.
 */
export function useMesh(node: MeshNode | null): UseMeshResult {
  const [devices, setDevices] = useState<BaseDevice[]>([]);
  const [localDevice, setLocalDevice] = useState<BaseDevice | null>(null);
  const [isPrimary, setIsPrimary] = useState(false);
  const [isConnected, setIsConnected] = useState(false);
  const [role, setRole] = useState<DeviceRole>('secondary');
  const nodeRef = useRef<MeshNode | null>(null);

  useEffect(() => {
    if (!node) return;

    nodeRef.current = node;
    let cancelled = false;

    // Fetch initial state (all NAPI methods are async)
    const init = async () => {
      try {
        const [running, local, devs, primary, currentRole] = await Promise.all([
          node.isRunning(),
          node.localDevice(),
          node.devices(),
          node.isPrimary(),
          node.role(),
        ]);
        if (cancelled) return;
        setIsConnected(running);
        setLocalDevice(local);
        setDevices(devs);
        setIsPrimary(primary);
        setRole(currentRole);
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
          // Re-fetch local device after start
          node.localDevice().then((d) => {
            if (!cancelled) setLocalDevice(d);
          });
          break;

        case 'stopped':
          setIsConnected(false);
          setDevices([]);
          break;

        case 'devicesChanged':
          // Payload contains the updated device list
          if (Array.isArray(event.payload)) {
            setDevices(event.payload);
          } else {
            // Fallback: re-fetch devices
            node.devices().then((d) => {
              if (!cancelled) setDevices(d);
            });
          }
          break;

        case 'deviceDiscovered':
        case 'deviceOffline':
          // Re-fetch devices list on discovery/offline events
          node.devices().then((d) => {
            if (!cancelled) setDevices(d);
          });
          break;

        case 'roleChanged':
          // Re-fetch role and primary status
          Promise.all([node.role(), node.isPrimary()]).then(([r, p]) => {
            if (!cancelled) {
              setRole(r);
              setIsPrimary(p);
            }
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

  return { devices, localDevice, isPrimary, isConnected, role, broadcast, sendTo };
}
