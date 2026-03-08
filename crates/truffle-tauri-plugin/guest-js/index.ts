/**
 * @truffle/tauri-plugin - Guest JS bindings for the Truffle Tauri plugin.
 *
 * Provides typed wrappers around Tauri's invoke() for mesh networking commands
 * and event listeners for mesh/proxy events.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

export interface DeviceInfo {
  id: string;
  deviceType: string;
  name: string;
  tailscaleHostname: string;
  tailscaleDnsName?: string;
  tailscaleIp?: string;
  role?: 'primary' | 'secondary';
  status: 'online' | 'offline' | 'connecting';
  capabilities: string[];
  metadata?: Record<string, unknown>;
  lastSeen?: number;
  startedAt?: number;
  os?: string;
  latencyMs?: number;
}

export interface TailnetPeer {
  id: string;
  hostname: string;
  dnsName: string;
  tailscaleIps: string[];
  online: boolean;
  os?: string;
}

export interface ProxyConfig {
  id: string;
  name: string;
  port: number;
  targetPort: number;
  targetScheme?: string;
}

export interface ProxyInfo {
  id: string;
  name: string;
  port: number;
  targetPort: number;
  targetScheme: string;
  isActive: boolean;
}

export interface MeshEventPayload {
  eventType: string;
  deviceId?: string;
  payload: unknown;
}

export interface ProxyEventPayload {
  eventType: string;
  proxyId: string;
  payload: unknown;
}

// ═══════════════════════════════════════════════════════════════════════════
// Mesh Node Commands
// ═══════════════════════════════════════════════════════════════════════════

export async function start(): Promise<void> {
  return invoke('plugin:truffle|start');
}

export async function stop(): Promise<void> {
  return invoke('plugin:truffle|stop');
}

export async function isRunning(): Promise<boolean> {
  return invoke('plugin:truffle|is_running');
}

export async function deviceId(): Promise<string> {
  return invoke('plugin:truffle|device_id');
}

export async function devices(): Promise<DeviceInfo[]> {
  return invoke('plugin:truffle|devices');
}

export async function deviceById(id: string): Promise<DeviceInfo | null> {
  return invoke('plugin:truffle|device_by_id', { id });
}

export async function isPrimary(): Promise<boolean> {
  return invoke('plugin:truffle|is_primary');
}

export async function primaryId(): Promise<string | null> {
  return invoke('plugin:truffle|primary_id');
}

export async function role(): Promise<'primary' | 'secondary'> {
  return invoke('plugin:truffle|role');
}

export async function sendEnvelope(
  deviceId: string,
  namespace: string,
  msgType: string,
  payload: unknown,
): Promise<boolean> {
  return invoke('plugin:truffle|send_envelope', {
    deviceId,
    namespace,
    msgType,
    payload,
  });
}

export async function broadcastEnvelope(
  namespace: string,
  msgType: string,
  payload: unknown,
): Promise<void> {
  return invoke('plugin:truffle|broadcast_envelope', {
    namespace,
    msgType,
    payload,
  });
}

export async function handleTailnetPeers(peers: TailnetPeer[]): Promise<void> {
  return invoke('plugin:truffle|handle_tailnet_peers', { peers });
}

export async function setLocalOnline(
  tailscaleIp: string,
  dnsName?: string,
): Promise<void> {
  return invoke('plugin:truffle|set_local_online', { tailscaleIp, dnsName });
}

// ═══════════════════════════════════════════════════════════════════════════
// Reverse Proxy Commands
// ═══════════════════════════════════════════════════════════════════════════

export async function proxyAdd(config: ProxyConfig): Promise<void> {
  return invoke('plugin:truffle|proxy_add', { config });
}

export async function proxyRemove(id: string): Promise<void> {
  return invoke('plugin:truffle|proxy_remove', { id });
}

export async function proxyList(): Promise<ProxyInfo[]> {
  return invoke('plugin:truffle|proxy_list');
}

// ═══════════════════════════════════════════════════════════════════════════
// Event Listeners
// ═══════════════════════════════════════════════════════════════════════════

export async function onMeshEvent(
  callback: (event: MeshEventPayload) => void,
): Promise<UnlistenFn> {
  return listen('truffle://mesh-event', (event) => {
    callback(event.payload as MeshEventPayload);
  });
}

export async function onProxyEvent(
  callback: (event: ProxyEventPayload) => void,
): Promise<UnlistenFn> {
  return listen('truffle://proxy-event', (event) => {
    callback(event.payload as ProxyEventPayload);
  });
}
