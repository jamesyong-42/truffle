/**
 * WebSocketTransport - Pure transport layer over Tailscale
 *
 * Manages WebSocket connections (incoming and outgoing) over Tailscale.
 * Constructor injection of SidecarClient - no singletons.
 *
 * Architecture:
 *   Layer 2: Application (MessageBus)
 *       |
 *   Layer 1: Routing & Serialization (MeshNode)
 *       |
 *   Layer 0: Transport (WebSocketTransport) <- This layer
 *       |
 *   SidecarClient (Tailscale Network)
 */

import type { ISidecarClient } from '@vibecook/truffle-sidecar-client';
import { TypedEventEmitter, createLogger } from '@vibecook/truffle-types';
import type { Logger } from '@vibecook/truffle-types';

// ═══════════════════════════════════════════════════════════════════════════
// HEARTBEAT CONSTANTS
// ═══════════════════════════════════════════════════════════════════════════

const DEFAULT_HEARTBEAT_PING_INTERVAL_MS = 2000;
const DEFAULT_HEARTBEAT_TIMEOUT_MS = 5000;

// ═══════════════════════════════════════════════════════════════════════════
// TYPES
// ═══════════════════════════════════════════════════════════════════════════

export interface WSConnection {
  id: string;
  deviceId: string | null;
  direction: 'incoming' | 'outgoing';
  remoteAddr: string;
  status: 'connecting' | 'connected' | 'disconnected';
  connectedAt: Date | null;
  metadata: Record<string, unknown>;
}

export interface TransportConfig {
  port?: number;
  maxConnections?: number;
}

export interface TransportEvents {
  connected: (conn: WSConnection) => void;
  disconnected: (connectionId: string, reason: string) => void;
  reconnecting: (deviceId: string, attempt: number, delayMs: number) => void;
  deviceIdentified: (connectionId: string, deviceId: string) => void;
  data: (connectionId: string, data: string) => void;
  error: (payload: { connectionId?: string; error: unknown }) => void;
}

// ═══════════════════════════════════════════════════════════════════════════
// INTERNAL TYPES
// ═══════════════════════════════════════════════════════════════════════════

interface InternalConnection extends WSConnection {
  lastActivityTime: number;
  heartbeatInterval: ReturnType<typeof setInterval> | null;
}

interface ReconnectInfo {
  deviceId: string;
  hostname: string;
  dnsName?: string;
  port: number;
  attempt: number;
  timer: ReturnType<typeof setTimeout> | null;
}

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

export interface TransportTimingConfig {
  heartbeatPingMs?: number;
  heartbeatTimeoutMs?: number;
  autoReconnect?: boolean;
  maxReconnectDelayMs?: number;
}

export class WebSocketTransport extends TypedEventEmitter<TransportEvents> {
  private readonly sidecar: ISidecarClient;
  private readonly log: Logger;
  private readonly heartbeatPingMs: number;
  private readonly heartbeatTimeoutMs: number;
  private readonly autoReconnect: boolean;
  private readonly maxReconnectDelayMs: number;
  private running = false;

  private connections = new Map<string, InternalConnection>();
  private deviceToConnection = new Map<string, string>();
  private reconnects = new Map<string, ReconnectInfo>();

  private sidecarHandlers: {
    wsConnect?: (connectionId: string, remoteAddr: string) => void;
    wsMessage?: (connectionId: string, data: string) => void;
    wsDisconnect?: (connectionId: string, reason?: string) => void;
    dialConnected?: (deviceId: string, remoteAddr: string) => void;
    dialMessage?: (deviceId: string, data: string) => void;
    dialDisconnect?: (deviceId: string, reason?: string) => void;
    dialError?: (deviceId: string, error: string) => void;
  } = {};

  constructor(sidecar: ISidecarClient, logger?: Logger, timing?: TransportTimingConfig) {
    super();
    this.sidecar = sidecar;
    this.log = logger ?? createLogger('Transport');
    this.heartbeatPingMs = timing?.heartbeatPingMs ?? DEFAULT_HEARTBEAT_PING_INTERVAL_MS;
    this.heartbeatTimeoutMs = timing?.heartbeatTimeoutMs ?? DEFAULT_HEARTBEAT_TIMEOUT_MS;
    this.autoReconnect = timing?.autoReconnect ?? true;
    this.maxReconnectDelayMs = timing?.maxReconnectDelayMs ?? 30000;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // LIFECYCLE
  // ─────────────────────────────────────────────────────────────────────────

  start(_config?: TransportConfig): void {
    if (this.running) {
      throw new Error('Transport already running');
    }
    this.running = true;
    this.setupSidecarListeners();
    this.log.info('Started');
  }

  stop(): void {
    if (!this.running) return;

    this.cancelAllReconnects();

    for (const [id, conn] of Array.from(this.connections)) {
      this.stopHeartbeat(id);
      if (conn.direction === 'outgoing' && conn.deviceId) {
        this.sidecar.dialClose(conn.deviceId);
      }
      this.connections.delete(id);
      this.emit('disconnected', id, 'service_stopped');
    }

    this.deviceToConnection.clear();
    this.removeSidecarListeners();
    this.running = false;
    this.log.info('Stopped');
  }

  isRunning(): boolean {
    return this.running;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // CONNECTIONS
  // ─────────────────────────────────────────────────────────────────────────

  async connect(
    deviceId: string,
    hostname: string,
    dnsName?: string,
    port: number = 443
  ): Promise<WSConnection> {
    if (!this.running) {
      throw new Error('Transport not running');
    }

    const existing = this.getConnectionByDeviceId(deviceId);
    if (existing?.status === 'connected') {
      return existing;
    }

    const connectionId = `dial:${deviceId}`;

    const conn: InternalConnection = {
      id: connectionId,
      deviceId,
      direction: 'outgoing',
      remoteAddr: `${hostname}:${port}`,
      status: 'connecting',
      connectedAt: null,
      metadata: {},
      lastActivityTime: Date.now(),
      heartbeatInterval: null,
    };

    this.connections.set(connectionId, conn);
    this.deviceToConnection.set(deviceId, connectionId);

    // Track for auto-reconnect
    this.cancelReconnect(deviceId);
    if (this.autoReconnect) {
      this.reconnects.set(deviceId, {
        deviceId,
        hostname,
        dnsName,
        port,
        attempt: 0,
        timer: null,
      });
    }

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        cleanup();
        this.connections.delete(connectionId);
        this.deviceToConnection.delete(deviceId);
        reject(new Error('Connection timeout'));
      }, 10000);

      const onConnected = (connDeviceId: string) => {
        if (connDeviceId !== deviceId) return;
        cleanup();
        conn.status = 'connected';
        conn.connectedAt = new Date();
        this.emit('connected', { ...conn });
        resolve({ ...conn });
      };

      const onError = (errDeviceId: string, error: string) => {
        if (errDeviceId !== deviceId) return;
        cleanup();
        this.connections.delete(connectionId);
        this.deviceToConnection.delete(deviceId);
        reject(new Error(error));
      };

      const cleanup = () => {
        clearTimeout(timeout);
        this.sidecar.off('dialConnected', onConnected);
        this.sidecar.off('dialError', onError);
      };

      this.sidecar.on('dialConnected', onConnected);
      this.sidecar.on('dialError', onError);

      this.sidecar.dial(deviceId, hostname, dnsName, port);
    });
  }

  getConnections(): WSConnection[] {
    return Array.from(this.connections.values()).map((c) => ({ ...c }));
  }

  getConnection(connectionId: string): WSConnection | null {
    const conn = this.connections.get(connectionId);
    return conn ? { ...conn } : null;
  }

  getConnectionByDeviceId(deviceId: string): WSConnection | null {
    const connectionId = this.deviceToConnection.get(deviceId);
    if (!connectionId) return null;
    return this.getConnection(connectionId);
  }

  closeConnection(connectionId: string): void {
    const conn = this.connections.get(connectionId);
    if (!conn) return;

    this.stopHeartbeat(connectionId);

    if (conn.direction === 'outgoing' && conn.deviceId) {
      this.sidecar.dialClose(conn.deviceId);
    }

    if (conn.deviceId) {
      this.deviceToConnection.delete(conn.deviceId);
    }
    this.connections.delete(connectionId);
    this.emit('disconnected', connectionId, 'closed_by_local');
  }

  setConnectionMetadata(connectionId: string, metadata: Record<string, unknown>): void {
    const conn = this.connections.get(connectionId);
    if (conn) {
      conn.metadata = { ...conn.metadata, ...metadata };
    }
  }

  setConnectionDeviceId(connectionId: string, deviceId: string): void {
    const conn = this.connections.get(connectionId);
    if (!conn) return;

    if (conn.deviceId) {
      this.deviceToConnection.delete(conn.deviceId);
    }

    conn.deviceId = deviceId;
    this.deviceToConnection.set(deviceId, connectionId);
    this.emit('deviceIdentified', connectionId, deviceId);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // MESSAGING
  // ─────────────────────────────────────────────────────────────────────────

  sendRaw(connectionId: string, data: string): boolean {
    const conn = this.connections.get(connectionId);
    if (!conn || conn.status !== 'connected') {
      return false;
    }

    try {
      if (conn.direction === 'outgoing' && conn.deviceId) {
        this.sidecar.dialSend(conn.deviceId, data);
      } else {
        const tsnetConnId = connectionId.startsWith('incoming:')
          ? connectionId.slice(9)
          : connectionId;
        this.sidecar.sendToConnection(tsnetConnId, data);
      }
      return true;
    } catch (error) {
      this.emitError({ connectionId, error });
      return false;
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - SIDECAR LISTENERS
  // ─────────────────────────────────────────────────────────────────────────

  private setupSidecarListeners(): void {
    this.sidecarHandlers.wsConnect = this.handleWsConnect.bind(this);
    this.sidecarHandlers.wsMessage = this.handleWsMessage.bind(this);
    this.sidecarHandlers.wsDisconnect = this.handleWsDisconnect.bind(this);
    this.sidecarHandlers.dialConnected = this.handleDialConnected.bind(this);
    this.sidecarHandlers.dialMessage = this.handleDialMessage.bind(this);
    this.sidecarHandlers.dialDisconnect = this.handleDialDisconnect.bind(this);
    this.sidecarHandlers.dialError = this.handleDialError.bind(this);

    this.sidecar.on('wsConnect', this.sidecarHandlers.wsConnect);
    this.sidecar.on('wsMessage', this.sidecarHandlers.wsMessage);
    this.sidecar.on('wsDisconnect', this.sidecarHandlers.wsDisconnect);
    this.sidecar.on('dialConnected', this.sidecarHandlers.dialConnected);
    this.sidecar.on('dialMessage', this.sidecarHandlers.dialMessage);
    this.sidecar.on('dialDisconnect', this.sidecarHandlers.dialDisconnect);
    this.sidecar.on('dialError', this.sidecarHandlers.dialError);
  }

  private removeSidecarListeners(): void {
    if (this.sidecarHandlers.wsConnect) this.sidecar.off('wsConnect', this.sidecarHandlers.wsConnect);
    if (this.sidecarHandlers.wsMessage) this.sidecar.off('wsMessage', this.sidecarHandlers.wsMessage);
    if (this.sidecarHandlers.wsDisconnect) this.sidecar.off('wsDisconnect', this.sidecarHandlers.wsDisconnect);
    if (this.sidecarHandlers.dialConnected) this.sidecar.off('dialConnected', this.sidecarHandlers.dialConnected);
    if (this.sidecarHandlers.dialMessage) this.sidecar.off('dialMessage', this.sidecarHandlers.dialMessage);
    if (this.sidecarHandlers.dialDisconnect) this.sidecar.off('dialDisconnect', this.sidecarHandlers.dialDisconnect);
    if (this.sidecarHandlers.dialError) this.sidecar.off('dialError', this.sidecarHandlers.dialError);
    this.sidecarHandlers = {};
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - INCOMING CONNECTION HANDLERS
  // ─────────────────────────────────────────────────────────────────────────

  private handleWsConnect(tsnetConnectionId: string, remoteAddr: string): void {
    const connectionId = `incoming:${tsnetConnectionId}`;
    this.log.info(`Incoming connection: ${connectionId} from ${remoteAddr}`);

    const conn: InternalConnection = {
      id: connectionId,
      deviceId: null,
      direction: 'incoming',
      remoteAddr,
      status: 'connected',
      connectedAt: new Date(),
      metadata: {},
      lastActivityTime: Date.now(),
      heartbeatInterval: null,
    };

    this.connections.set(connectionId, conn);
    this.startHeartbeat(connectionId);
    this.emit('connected', { ...conn });
  }

  private handleWsMessage(tsnetConnectionId: string, data: string): void {
    const connectionId = `incoming:${tsnetConnectionId}`;
    this.updateActivity(connectionId);

    if (this.handleHeartbeatMessage(connectionId, data)) {
      return;
    }

    this.emit('data', connectionId, data);
  }

  private handleWsDisconnect(tsnetConnectionId: string, reason?: string): void {
    const connectionId = `incoming:${tsnetConnectionId}`;
    const conn = this.connections.get(connectionId);

    if (conn) {
      this.log.info(`Incoming disconnected: ${connectionId} (${reason || 'unknown'})`);
      this.stopHeartbeat(connectionId);

      if (conn.deviceId) {
        this.deviceToConnection.delete(conn.deviceId);
      }
      this.connections.delete(connectionId);
      this.emit('disconnected', connectionId, reason || 'remote_closed');
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - OUTGOING CONNECTION HANDLERS
  // ─────────────────────────────────────────────────────────────────────────

  private handleDialConnected(deviceId: string, remoteAddr: string): void {
    const connectionId = `dial:${deviceId}`;
    const conn = this.connections.get(connectionId);

    if (conn) {
      this.log.info(`Dial connected: ${connectionId} to ${remoteAddr}`);
      conn.status = 'connected';
      conn.connectedAt = new Date();
      conn.remoteAddr = remoteAddr;
      conn.lastActivityTime = Date.now();
      conn.heartbeatInterval = null;
      this.startHeartbeat(connectionId);

      // Reset reconnect attempt counter on successful connect
      const reconnectInfo = this.reconnects.get(deviceId);
      if (reconnectInfo) {
        reconnectInfo.attempt = 0;
      }

      this.emit('connected', { ...conn });
    }
  }

  private handleDialMessage(deviceId: string, data: string): void {
    const connectionId = `dial:${deviceId}`;
    this.updateActivity(connectionId);

    if (this.handleHeartbeatMessage(connectionId, data)) {
      return;
    }

    this.emit('data', connectionId, data);
  }

  private handleDialDisconnect(deviceId: string, reason?: string): void {
    const connectionId = `dial:${deviceId}`;
    const conn = this.connections.get(connectionId);

    if (conn) {
      this.log.info(`Dial disconnected: ${connectionId} (${reason || 'unknown'})`);
      this.stopHeartbeat(connectionId);
      conn.status = 'disconnected';
      this.deviceToConnection.delete(deviceId);
      this.connections.delete(connectionId);
      this.emit('disconnected', connectionId, reason || 'remote_closed');

      this.scheduleReconnect(deviceId);
    }
  }

  private handleDialError(deviceId: string, error: string): void {
    const connectionId = `dial:${deviceId}`;
    this.log.error(`Dial error: ${connectionId} - ${error}`);
    this.emitError({ connectionId, error: new Error(error) });
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - HEARTBEAT
  // ─────────────────────────────────────────────────────────────────────────

  private startHeartbeat(connectionId: string): void {
    const conn = this.connections.get(connectionId);
    if (!conn) return;

    conn.lastActivityTime = Date.now();
    conn.heartbeatInterval = setInterval(() => {
      this.heartbeatTick(connectionId);
    }, this.heartbeatPingMs);
  }

  private stopHeartbeat(connectionId: string): void {
    const conn = this.connections.get(connectionId);
    if (!conn) return;

    if (conn.heartbeatInterval) {
      clearInterval(conn.heartbeatInterval);
      conn.heartbeatInterval = null;
    }
  }

  private heartbeatTick(connectionId: string): void {
    const conn = this.connections.get(connectionId);
    if (!conn || conn.status !== 'connected') {
      this.stopHeartbeat(connectionId);
      return;
    }

    const now = Date.now();
    const timeSinceActivity = now - conn.lastActivityTime;

    if (timeSinceActivity > this.heartbeatTimeoutMs) {
      this.log.info(`Heartbeat timeout for ${connectionId} (no activity for ${timeSinceActivity}ms)`);
      this.stopHeartbeat(connectionId);
      this.handleHeartbeatTimeout(connectionId);
      return;
    }

    try {
      const ping = JSON.stringify({ type: 'ping', timestamp: now });
      this.sendRaw(connectionId, ping);
    } catch (error) {
      this.log.error(`Failed to send ping to ${connectionId}:`, error);
    }
  }

  private handleHeartbeatTimeout(connectionId: string): void {
    const conn = this.connections.get(connectionId);
    if (!conn) return;

    const deviceId = conn.deviceId;

    if (conn.direction === 'outgoing' && deviceId) {
      this.sidecar.dialClose(deviceId);
    }

    if (deviceId) {
      this.deviceToConnection.delete(deviceId);
    }
    this.connections.delete(connectionId);
    this.emit('disconnected', connectionId, 'heartbeat_timeout');

    if (conn.direction === 'outgoing' && deviceId) {
      this.scheduleReconnect(deviceId);
    }
  }

  private updateActivity(connectionId: string): void {
    const conn = this.connections.get(connectionId);
    if (conn) {
      conn.lastActivityTime = Date.now();
    }
  }

  private handleHeartbeatMessage(connectionId: string, data: string): boolean {
    try {
      if (!data.includes('"type":"ping"') && !data.includes('"type":"pong"')) {
        return false;
      }

      const msg = JSON.parse(data);
      if (msg.type === 'ping') {
        const pong = JSON.stringify({
          type: 'pong',
          timestamp: Date.now(),
          echoTimestamp: msg.timestamp,
        });
        this.sendRaw(connectionId, pong);
        return true;
      } else if (msg.type === 'pong') {
        return true;
      }
    } catch {
      // Not valid JSON - not a heartbeat message
    }
    return false;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - RECONNECTION
  // ─────────────────────────────────────────────────────────────────────────

  private scheduleReconnect(deviceId: string): void {
    if (!this.autoReconnect || !this.running) return;

    const info = this.reconnects.get(deviceId);
    if (!info) return;

    info.attempt++;
    const delayMs = Math.min(1000 * Math.pow(2, info.attempt - 1), this.maxReconnectDelayMs);

    this.log.info(`Reconnecting to ${deviceId} in ${delayMs}ms (attempt ${info.attempt})`);
    this.emit('reconnecting', deviceId, info.attempt, delayMs);

    info.timer = setTimeout(() => {
      if (!this.running) return;
      info.timer = null;
      this.connect(deviceId, info.hostname, info.dnsName, info.port).catch((err) => {
        this.log.warn(`Reconnect to ${deviceId} failed: ${err.message}`);
        this.scheduleReconnect(deviceId);
      });
    }, delayMs);
  }

  private cancelReconnect(deviceId: string): void {
    const info = this.reconnects.get(deviceId);
    if (info?.timer) {
      clearTimeout(info.timer);
      info.timer = null;
    }
    this.reconnects.delete(deviceId);
  }

  private cancelAllReconnects(): void {
    for (const [deviceId] of this.reconnects) {
      this.cancelReconnect(deviceId);
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - ERROR HANDLING
  // ─────────────────────────────────────────────────────────────────────────

  private emitError(payload: { connectionId?: string; error: unknown }): void {
    if (this.listenerCount('error') > 0) {
      this.emit('error', payload);
      return;
    }

    const errorMessage = payload.error instanceof Error ? payload.error.message : String(payload.error);
    const connectionInfo = payload.connectionId ? ` connection=${payload.connectionId}` : '';
    this.log.error(`error:${connectionInfo} ${errorMessage}`);
  }
}
