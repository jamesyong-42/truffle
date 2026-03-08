import { useState, useEffect, useCallback, useRef } from 'react';
// TODO: Rewrite to use NapiMeshNode API (async methods, callback-based events)
import type { NapiBaseDevice as BaseDevice, NapiMeshNode as MeshNode } from '@vibecook/truffle';
type DeviceRole = string;

export interface UseMeshResult {
  devices: BaseDevice[];
  localDevice: BaseDevice | null;
  isPrimary: boolean;
  isConnected: boolean;
  role: DeviceRole;
  broadcast: (namespace: string, type: string, payload: unknown) => void;
  sendTo: (deviceId: string, namespace: string, type: string, payload: unknown) => boolean;
}

/**
 * React hook for Truffle mesh networking.
 *
 * Subscribes to device changes and provides messaging helpers.
 * The MeshNode must already be started before using this hook.
 */
export function useMesh(node: MeshNode | null): UseMeshResult {
  const [devices, setDevices] = useState<BaseDevice[]>([]);
  const [localDevice, setLocalDevice] = useState<BaseDevice | null>(null);
  const [isPrimary, setIsPrimary] = useState(false);
  const [isConnected, setIsConnected] = useState(false);
  const [role, setRole] = useState<DeviceRole>('secondary');
  const busRef = useRef<ReturnType<MeshNode['getMessageBus']> | null>(null);

  useEffect(() => {
    if (!node) return;

    busRef.current = node.getMessageBus();
    setLocalDevice(node.getLocalDevice());
    setDevices(node.getDevices());
    setIsConnected(node.isRunning());
    setIsPrimary(node.isPrimary());
    setRole(node.getRole());

    const onDevicesChanged = (devs: BaseDevice[]) => setDevices(devs);
    const onRoleChanged = (r: DeviceRole, primary: boolean) => {
      setRole(r);
      setIsPrimary(primary);
    };
    const onStarted = () => {
      setIsConnected(true);
      setLocalDevice(node.getLocalDevice());
    };
    const onStopped = () => {
      setIsConnected(false);
      setDevices([]);
    };

    node.on('devicesChanged', onDevicesChanged);
    node.on('roleChanged', onRoleChanged);
    node.on('started', onStarted);
    node.on('stopped', onStopped);

    return () => {
      node.off('devicesChanged', onDevicesChanged);
      node.off('roleChanged', onRoleChanged);
      node.off('started', onStarted);
      node.off('stopped', onStopped);
      busRef.current = null;
    };
  }, [node]);

  const broadcast = useCallback((namespace: string, type: string, payload: unknown) => {
    busRef.current?.broadcast(namespace, type, payload);
  }, []);

  const sendTo = useCallback(
    (deviceId: string, namespace: string, type: string, payload: unknown): boolean => {
      return busRef.current?.publish(deviceId, namespace, type, payload) ?? false;
    },
    [],
  );

  return { devices, localDevice, isPrimary, isConnected, role, broadcast, sendTo };
}
