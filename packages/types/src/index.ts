import { z } from 'zod';
import { EventEmitter } from 'events';

// ═══════════════════════════════════════════════════════════════════════════
// DEVICE MODEL
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Device role in STAR topology.
 * - primary: Hub device that all others connect to
 * - secondary: Connects to primary, routes through primary
 */
export type DeviceRole = 'primary' | 'secondary';

/**
 * Device status.
 */
export type DeviceStatus = 'online' | 'offline' | 'connecting';

/**
 * A device in the mesh network.
 * Generic - device type and capabilities are strings, not fixed enums.
 */
export const BaseDeviceSchema = z.object({
  /** Unique device identifier (persisted, never changes) */
  id: z.string(),
  /** Device type (e.g., 'desktop', 'mobile', 'server') - application-defined */
  type: z.string(),
  /** User-editable friendly name */
  name: z.string(),
  /** Tailscale hostname: {prefix}-{type}-{id} */
  tailscaleHostname: z.string(),
  /** Full MagicDNS name for TLS (e.g., "myapp-desktop-xxx.tailnet.ts.net") */
  tailscaleDNSName: z.string().optional(),
  /** Tailscale IP address (assigned when connected) */
  tailscaleIP: z.string().optional(),
  /** Device role in STAR topology */
  role: z.enum(['primary', 'secondary']).optional(),
  /** Device status */
  status: z.enum(['online', 'offline', 'connecting']),
  /** Device capabilities - application-defined strings */
  capabilities: z.array(z.string()),
  /** Application-specific metadata */
  metadata: z.record(z.unknown()).optional(),
  /** Last seen timestamp */
  lastSeen: z.number().optional(),
  /** Startup timestamp (used for primary election) */
  startedAt: z.number().optional(),
  /** OS information */
  os: z.string().optional(),
  /** Latency to this device in ms */
  latencyMs: z.number().optional(),
});
export type BaseDevice = z.infer<typeof BaseDeviceSchema>;

// ═══════════════════════════════════════════════════════════════════════════
// HOSTNAME UTILITIES
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Generate Tailscale hostname from prefix, device type, and ID.
 * Format: {prefix}-{type}-{id}
 */
export function generateHostname(prefix: string, type: string, id: string): string {
  return `${prefix}-${type}-${id}`;
}

/**
 * Parse device type and ID from Tailscale hostname.
 * Returns null if hostname doesn't match the pattern: {prefix}-{type}-{id}
 */
export function parseHostname(
  prefix: string,
  hostname: string
): { type: string; id: string } | null {
  const regex = new RegExp(`^${prefix}-([^-]+)-(.+)$`);
  const match = hostname.match(regex);
  if (!match) {
    return null;
  }
  return {
    type: match[1],
    id: match[2],
  };
}

// ═══════════════════════════════════════════════════════════════════════════
// MESH ENVELOPE (for MessageBus layer)
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Envelope format for messages sent over the mesh network.
 * Used by MessageBus for namespace-based pub/sub.
 *
 * Architecture:
 *   Layer 2: Application (MessageBus) - uses MeshEnvelope
 *       |
 *   Layer 1: Routing & Serialization (MeshNode)
 *       |
 *   Layer 0: Transport (WebSocketTransport)
 */
export const MeshEnvelopeSchema = z.object({
  /** Message namespace (e.g., 'sync', 'custom') */
  namespace: z.string(),
  /** Message type within namespace */
  type: z.string(),
  /** Message payload */
  payload: z.unknown(),
  /** Timestamp (optional, added by transport) */
  timestamp: z.number().optional(),
});
export type MeshEnvelope = z.infer<typeof MeshEnvelopeSchema>;

// ═══════════════════════════════════════════════════════════════════════════
// STAR TOPOLOGY MESSAGE PROTOCOL
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Message types for STAR topology communication.
 * Only infrastructure types - no application-specific types.
 */
export type MeshMessageType =
  // Device lifecycle
  | 'device:announce'
  | 'device:update'
  | 'device:goodbye'
  | 'device:list'
  // Primary election
  | 'election:start'
  | 'election:candidate'
  | 'election:vote'
  | 'election:result'
  // Message routing (via primary)
  | 'route:message'
  | 'route:broadcast'
  // Health check
  | 'ping'
  | 'pong'
  // Error
  | 'error';

/**
 * Base mesh message wrapper.
 */
export interface MeshMessage<T = unknown> {
  type: MeshMessageType;
  /** Source device ID */
  from: string;
  /** Target device ID (for routed messages, omit for broadcast) */
  to?: string;
  /** Message payload */
  payload: T;
  /** Timestamp */
  timestamp: number;
  /** Correlation ID for request/response matching */
  correlationId?: string;
}

/**
 * Create a mesh message.
 */
export function createMeshMessage<T>(
  type: MeshMessageType,
  from: string,
  payload: T,
  to?: string,
  correlationId?: string
): MeshMessage<T> {
  return {
    type,
    from,
    to,
    payload,
    timestamp: Date.now(),
    correlationId,
  };
}

// ═══════════════════════════════════════════════════════════════════════════
// MESSAGE PAYLOADS
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Device announce payload.
 */
export const DeviceAnnouncePayloadSchema = z.object({
  device: BaseDeviceSchema,
  /** Protocol version for compatibility */
  protocolVersion: z.number().default(2),
});
export type DeviceAnnouncePayload = z.infer<typeof DeviceAnnouncePayloadSchema>;

/**
 * Device list payload (response from primary).
 */
export const DeviceListPayloadSchema = z.object({
  devices: z.array(BaseDeviceSchema),
  /** ID of the current primary */
  primaryId: z.string(),
});
export type DeviceListPayload = z.infer<typeof DeviceListPayloadSchema>;

/**
 * Election candidate payload.
 */
export const ElectionCandidatePayloadSchema = z.object({
  deviceId: z.string(),
  /** How long this device has been online (ms) */
  uptime: z.number(),
  /** Whether user explicitly designated this as primary */
  userDesignated: z.boolean().default(false),
});
export type ElectionCandidatePayload = z.infer<typeof ElectionCandidatePayloadSchema>;

/**
 * Election result payload.
 */
export const ElectionResultPayloadSchema = z.object({
  newPrimaryId: z.string(),
  previousPrimaryId: z.string().optional(),
  reason: z.string(),
});
export type ElectionResultPayload = z.infer<typeof ElectionResultPayloadSchema>;

/**
 * Route message payload (wraps another message for routing).
 */
export const RouteMessagePayloadSchema = z.object({
  /** Target device ID */
  targetDeviceId: z.string(),
  /** Wrapped message */
  message: z.unknown(),
});
export type RouteMessagePayload = z.infer<typeof RouteMessagePayloadSchema>;

/**
 * Ping payload.
 */
export const PingPayloadSchema = z.object({
  timestamp: z.number(),
});
export type PingPayload = z.infer<typeof PingPayloadSchema>;

/**
 * Pong payload.
 */
export const PongPayloadSchema = z.object({
  timestamp: z.number(),
  echoTimestamp: z.number(),
});
export type PongPayload = z.infer<typeof PongPayloadSchema>;

/**
 * Mesh error payload.
 */
export const MeshErrorPayloadSchema = z.object({
  code: z.string(),
  message: z.string(),
  details: z.record(z.unknown()).optional(),
});
export type MeshErrorPayload = z.infer<typeof MeshErrorPayloadSchema>;

// ═══════════════════════════════════════════════════════════════════════════
// TAILNET PEER (from Tailscale API)
// ═══════════════════════════════════════════════════════════════════════════

export const TailnetPeerSchema = z.object({
  id: z.string(),
  hostname: z.string(),
  dnsName: z.string(),
  tailscaleIPs: z.array(z.string()),
  online: z.boolean(),
  os: z.string().optional(),
});
export type TailnetPeer = z.infer<typeof TailnetPeerSchema>;

// ═══════════════════════════════════════════════════════════════════════════
// MESH CONFIG
// ═══════════════════════════════════════════════════════════════════════════

export const MeshConfigSchema = z.object({
  /** Enable mesh networking */
  enabled: z.boolean().default(false),
  /** Device ID */
  deviceId: z.string().optional(),
  /** User-editable device name */
  deviceName: z.string().optional(),
  /** Path to sidecar binary */
  sidecarPath: z.string().optional(),
  /** Tailscale auth key */
  authKey: z.string().optional(),
  /** Mark this device as preferred primary */
  preferPrimary: z.boolean().default(false),
});
export type MeshConfig = z.infer<typeof MeshConfigSchema>;

// ═══════════════════════════════════════════════════════════════════════════
// SIDECAR IPC TYPES
// ═══════════════════════════════════════════════════════════════════════════

export type SidecarState = 'stopped' | 'starting' | 'running' | 'stopping' | 'error';

export interface SidecarStatus {
  state: SidecarState;
  hostname?: string;
  dnsName?: string;
  tailscaleIP?: string;
  error?: string;
}

export interface SidecarConfig {
  /** Path to sidecar binary */
  sidecarPath: string;
  /** Desired hostname on the tailnet */
  hostname: string;
  /** Directory to store Tailscale state */
  stateDir: string;
  /** Optional Tailscale auth key */
  authKey?: string;
  /** Path to static files to serve (e.g., PWA) */
  staticPath?: string;
  /** Hostname prefix for peer filtering */
  hostnamePrefix?: string;
}

export interface SidecarServiceEvents {
  statusChanged: (status: SidecarStatus) => void;
  authRequired: (authUrl: string) => void;
  tailnetPeers: (peers: TailnetPeer[]) => void;
  wsConnect: (connectionId: string, remoteAddr: string) => void;
  wsMessage: (connectionId: string, data: string) => void;
  wsDisconnect: (connectionId: string, reason?: string) => void;
  dialConnected: (deviceId: string, remoteAddr: string) => void;
  dialMessage: (deviceId: string, data: string) => void;
  dialDisconnect: (deviceId: string, reason?: string) => void;
  dialError: (deviceId: string, error: string) => void;
  error: (error: Error) => void;
}

// ═══════════════════════════════════════════════════════════════════════════
// STORE SYNC TYPES
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Message types for store synchronization over the mesh network.
 */
export const STORE_SYNC_MESSAGE_TYPES = {
  SYNC_FULL: 'store:sync:full',
  SYNC_UPDATE: 'store:sync:update',
  SYNC_REQUEST: 'store:sync:request',
  SYNC_CLEAR: 'store:sync:clear',
} as const;

export type StoreSyncMessageType =
  (typeof STORE_SYNC_MESSAGE_TYPES)[keyof typeof STORE_SYNC_MESSAGE_TYPES];

/**
 * A device-owned slice of data in a store.
 * Each device only writes to its own slice.
 */
export const DeviceSliceSchema = z.object({
  deviceId: z.string(),
  data: z.unknown(),
  updatedAt: z.number(),
  version: z.number(),
});

export type DeviceSlice<T = unknown> = {
  deviceId: string;
  data: T;
  updatedAt: number;
  version: number;
};

/**
 * Payload for SYNC_FULL message.
 */
export const SyncFullPayloadSchema = z.object({
  storeId: z.string(),
  deviceId: z.string(),
  data: z.unknown(),
  version: z.number(),
  updatedAt: z.number(),
  schemaVersion: z.number().optional(),
});

export type SyncFullPayload<T = unknown> = {
  storeId: string;
  deviceId: string;
  data: T;
  version: number;
  updatedAt: number;
  schemaVersion?: number;
};

/**
 * Payload for SYNC_UPDATE message.
 */
export const SyncUpdatePayloadSchema = z.object({
  storeId: z.string(),
  deviceId: z.string(),
  data: z.unknown(),
  version: z.number(),
  updatedAt: z.number(),
});

export type SyncUpdatePayload<T = unknown> = {
  storeId: string;
  deviceId: string;
  data: T;
  version: number;
  updatedAt: number;
};

/**
 * Payload for SYNC_REQUEST message.
 */
export const SyncRequestPayloadSchema = z.object({
  storeId: z.string(),
  fromDeviceId: z.string().optional(),
});

export type SyncRequestPayload = {
  storeId: string;
  fromDeviceId?: string;
};

/**
 * Payload for SYNC_CLEAR message.
 */
export const SyncClearPayloadSchema = z.object({
  storeId: z.string(),
  deviceId: z.string(),
  reason: z.string().optional(),
});

export type SyncClearPayload = {
  storeId: string;
  deviceId: string;
  reason?: string;
};

/**
 * Types of changes that can occur in a store.
 */
export type StoreChangeType = 'update' | 'delete' | 'full-sync';

/**
 * Change event emitted when store data changes.
 */
export type StoreChangeEvent<T = unknown> = {
  type: StoreChangeType;
  storeId: string;
  deviceId: string;
  previousData: T | undefined;
  currentData: T | undefined;
  version: number;
  isLocal: boolean;
};

/**
 * Default namespace for store sync messages.
 */
export const STORE_SYNC_NAMESPACE = 'sync';

// ═══════════════════════════════════════════════════════════════════════════
// LOGGER INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Simple logger interface. Consumers can provide their own logger.
 */
export interface Logger {
  info(message: string, ...args: unknown[]): void;
  warn(message: string, ...args: unknown[]): void;
  error(message: string, ...args: unknown[]): void;
  debug(message: string, ...args: unknown[]): void;
}

/**
 * Default console logger.
 */
export const consoleLogger: Logger = {
  info: (msg, ...args) => console.log(msg, ...args),
  warn: (msg, ...args) => console.warn(msg, ...args),
  error: (msg, ...args) => console.error(msg, ...args),
  debug: (msg, ...args) => console.log(msg, ...args),
};

/**
 * Create a logger with a prefix tag. Replaces inline logger creation.
 */
export function createLogger(prefix: string): Logger {
  return {
    info: (msg, ...args) => console.log(`[${prefix}] ${msg}`, ...args),
    warn: (msg, ...args) => console.warn(`[${prefix}] ${msg}`, ...args),
    error: (msg, ...args) => console.error(`[${prefix}] ${msg}`, ...args),
    debug: (msg, ...args) => console.log(`[${prefix}] ${msg}`, ...args),
  };
}

// ═══════════════════════════════════════════════════════════════════════════
// TYPED EVENT EMITTER
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Type-safe EventEmitter. Extend with an event map to get typed on/off/emit.
 *
 * Usage: `class Foo extends TypedEventEmitter<{ myEvent: (x: number) => void }>`
 */
export class TypedEventEmitter<
  Events extends {} = {},
> extends EventEmitter {
  override on<K extends string & keyof Events>(
    event: K,
    listener: Events[K] & ((...args: any[]) => void),
  ): this {
    return super.on(event, listener);
  }

  override off<K extends string & keyof Events>(
    event: K,
    listener: Events[K] & ((...args: any[]) => void),
  ): this {
    return super.off(event, listener);
  }

  override emit<K extends string & keyof Events>(
    event: K,
    ...args: Events[K] extends (...args: infer A) => any ? A : never
  ): boolean {
    return super.emit(event, ...args);
  }
}
