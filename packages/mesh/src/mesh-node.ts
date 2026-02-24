/**
 * MeshNode - The main entry point for Truffle mesh networking
 *
 * Orchestrates device discovery, STAR topology routing, primary election,
 * and message serialization. No singletons, no Electron deps.
 *
 * All config injected via MeshNodeConfig.
 */

import {
  TypedEventEmitter,
  createMeshMessage,
  generateHostname,
  createLogger,
  MeshEnvelopeSchema,
  DeviceAnnouncePayloadSchema,
  DeviceListPayloadSchema,
  ElectionCandidatePayloadSchema,
  ElectionResultPayloadSchema,
} from '@vibecook/truffle-types';
import type {
  BaseDevice,
  DeviceRole,
  MeshMessage,
  DeviceAnnouncePayload,
  DeviceListPayload,
  MeshEnvelope,
  TailnetPeer,
  SidecarStatus,
  Logger,
} from '@vibecook/truffle-types';
import type { IMessageBus } from '@vibecook/truffle-protocol';
import { SidecarClient, type ISidecarClient } from '@vibecook/truffle-sidecar-client';
import { WebSocketTransport, type WSConnection } from '@vibecook/truffle-transport';
import { DeviceManager, type DeviceIdentity } from './device-manager.js';
import { PrimaryElection } from './primary-election.js';
import { MeshMessageBus } from './mesh-message-bus.js';

// ═══════════════════════════════════════════════════════════════════════════
// CONSTANTS
// ═══════════════════════════════════════════════════════════════════════════

const DEFAULT_ANNOUNCE_INTERVAL_MS = 30000;
const DEFAULT_DISCOVERY_TIMEOUT_MS = 5000;
const MESH_NAMESPACE = 'mesh';

// ═══════════════════════════════════════════════════════════════════════════
// TYPES
// ═══════════════════════════════════════════════════════════════════════════

export interface MeshTimingConfig {
  announceIntervalMs?: number;
  discoveryTimeoutMs?: number;
  electionTimeoutMs?: number;
  primaryLossGraceMs?: number;
  heartbeatPingMs?: number;
  heartbeatTimeoutMs?: number;
}

export interface MeshNodeConfig {
  deviceId: string;
  deviceName: string;
  deviceType: string;
  hostnamePrefix: string;
  sidecarPath: string;
  stateDir: string;
  authKey?: string;
  preferPrimary?: boolean;
  staticPath?: string;
  capabilities?: string[];
  metadata?: Record<string, unknown>;
  logger?: Logger;
  timing?: MeshTimingConfig;
}

export interface IncomingMeshMessage {
  from: string | undefined;
  connectionId: string;
  namespace: string;
  type: string;
  payload: unknown;
}

export interface MeshNodeEvents {
  started: () => void;
  stopped: () => void;
  authRequired: (authUrl: string) => void;
  deviceDiscovered: (device: BaseDevice) => void;
  deviceUpdated: (device: BaseDevice) => void;
  deviceOffline: (deviceId: string) => void;
  devicesChanged: (devices: BaseDevice[]) => void;
  roleChanged: (role: DeviceRole, isPrimary: boolean) => void;
  primaryChanged: (primaryId: string | null) => void;
  message: (message: IncomingMeshMessage) => void;
  error: (error: Error) => void;
}

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

export class MeshNode extends TypedEventEmitter<MeshNodeEvents> {
  private readonly config: MeshNodeConfig;
  private readonly sidecar: ISidecarClient;
  private readonly transport: WebSocketTransport;
  private readonly election: PrimaryElection;
  private readonly deviceManager: DeviceManager;
  private readonly log: Logger;
  private readonly announceIntervalMs: number;
  private readonly discoveryTimeoutMs: number;
  private messageBus: MeshMessageBus | null = null;

  private running = false;
  private startedAt = 0;
  private announceInterval: ReturnType<typeof setInterval> | null = null;

  private sidecarHandlers: {
    statusChanged?: (status: SidecarStatus) => void;
    authRequired?: (authUrl: string) => void;
    tailnetPeers?: (peers: TailnetPeer[]) => void;
    error?: (error: Error) => void;
  } = {};
  private transportHandlers: {
    connected?: (conn: WSConnection) => void;
    disconnected?: (connectionId: string, reason: string) => void;
    deviceIdentified?: (connectionId: string, deviceId: string) => void;
    data?: (connectionId: string, data: string) => void;
  } = {};

  constructor(config: MeshNodeConfig) {
    super();
    this.config = config;
    this.log = config.logger ?? createLogger('MeshNode');
    this.announceIntervalMs = config.timing?.announceIntervalMs ?? DEFAULT_ANNOUNCE_INTERVAL_MS;
    this.discoveryTimeoutMs = config.timing?.discoveryTimeoutMs ?? DEFAULT_DISCOVERY_TIMEOUT_MS;

    const hostname = generateHostname(config.hostnamePrefix, config.deviceType, config.deviceId);

    const identity: DeviceIdentity = {
      id: config.deviceId,
      type: config.deviceType,
      name: config.deviceName,
      tailscaleHostname: hostname,
    };

    this.deviceManager = new DeviceManager({
      identity,
      hostnamePrefix: config.hostnamePrefix,
      capabilities: config.capabilities,
      metadata: config.metadata,
      logger: config.logger,
    });

    this.sidecar = new SidecarClient(config.logger);
    this.transport = new WebSocketTransport(this.sidecar, config.logger, {
      heartbeatPingMs: config.timing?.heartbeatPingMs,
      heartbeatTimeoutMs: config.timing?.heartbeatTimeoutMs,
    });
    this.election = new PrimaryElection(config.logger, {
      electionTimeoutMs: config.timing?.electionTimeoutMs,
      primaryLossGraceMs: config.timing?.primaryLossGraceMs,
    });

    this.setupDeviceManagerListeners();
    this.setupElectionListeners();
  }

  // ─────────────────────────────────────────────────────────────────────────
  // LIFECYCLE
  // ─────────────────────────────────────────────────────────────────────────

  async start(): Promise<void> {
    if (this.running) {
      throw new Error('MeshNode already running');
    }

    this.startedAt = Date.now();
    this.running = true;

    this.election.configure({
      deviceId: this.config.deviceId,
      startedAt: this.startedAt,
      preferPrimary: this.config.preferPrimary ?? false,
    });

    this.log.info(`Starting with identity: ${this.config.deviceId} (${this.config.deviceName})`);

    this.setupSidecarListeners();

    await this.sidecar.start({
      sidecarPath: this.config.sidecarPath,
      hostname: this.deviceManager.getDeviceIdentity().tailscaleHostname,
      stateDir: this.config.stateDir,
      authKey: this.config.authKey,
      staticPath: this.config.staticPath,
      hostnamePrefix: this.config.hostnamePrefix,
    });

    this.transport.start();
    this.setupTransportListeners();

    const status = this.sidecar.getStatus();
    this.deviceManager.setLocalOnline(status.tailscaleIP || '', this.startedAt, status.dnsName);

    this.startAnnounceInterval();

    setTimeout(() => this.discoverDevices(), 1000);

    this.emit('started');
  }

  async stop(): Promise<void> {
    if (!this.running) return;

    this.log.info('Stopping...');

    this.stopAnnounceInterval();
    this.broadcastAnnounce('goodbye');

    this.transport.stop();
    this.removeTransportListeners();

    await this.sidecar.stop();
    this.removeSidecarListeners();

    this.running = false;
    this.deviceManager.setLocalOffline();
    this.election.reset();
    this.deviceManager.clear();

    this.emit('stopped');
  }

  isRunning(): boolean {
    return this.running;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // DEVICE INFO
  // ─────────────────────────────────────────────────────────────────────────

  getLocalDevice(): BaseDevice {
    return this.deviceManager.getLocalDevice();
  }

  getDeviceId(): string {
    return this.deviceManager.getDeviceId();
  }

  getDevices(): BaseDevice[] {
    return this.deviceManager.getDevices();
  }

  getDeviceById(id: string): BaseDevice | undefined {
    return this.deviceManager.getDeviceById(id);
  }

  updateDeviceName(name: string): void {
    this.deviceManager.updateDeviceName(name);
  }

  updateMetadata(metadata: Record<string, unknown>): void {
    this.deviceManager.updateMetadata(metadata);
  }

  getDnsName(): string | undefined {
    return this.sidecar.getStatus().dnsName;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // ROLE MANAGEMENT
  // ─────────────────────────────────────────────────────────────────────────

  isPrimary(): boolean {
    return this.election.isPrimary();
  }

  getPrimaryDevice(): BaseDevice | undefined {
    return this.deviceManager.getPrimaryDevice();
  }

  getRole(): DeviceRole {
    return this.isPrimary() ? 'primary' : 'secondary';
  }

  // ─────────────────────────────────────────────────────────────────────────
  // DISCOVERY
  // ─────────────────────────────────────────────────────────────────────────

  discoverDevices(): void {
    if (!this.running) return;
    this.log.info('Discovering devices...');
    this.sidecar.getTailnetPeers();
  }

  async connectToDevice(deviceId: string): Promise<void> {
    const device = this.deviceManager.getDeviceById(deviceId);
    if (!device) {
      throw new Error(`Device not found: ${deviceId}`);
    }
    if (!device.tailscaleIP) {
      throw new Error(`Device has no IP: ${deviceId}`);
    }

    const existing = this.transport.getConnectionByDeviceId(deviceId);
    if (existing?.status === 'connected') {
      return;
    }

    await this.transport.connect(deviceId, device.tailscaleHostname, device.tailscaleDNSName, 443);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // MESSAGING - Envelope API (for MessageBus)
  // ─────────────────────────────────────────────────────────────────────────

  sendEnvelope(deviceId: string, envelope: MeshEnvelope): boolean {
    const localDeviceId = this.deviceManager.getDeviceId();

    if (deviceId === localDeviceId) {
      this.emit('message', {
        from: localDeviceId,
        connectionId: 'local',
        namespace: envelope.namespace,
        type: envelope.type,
        payload: envelope.payload,
      });
      return true;
    }

    const conn = this.transport.getConnectionByDeviceId(deviceId);
    if (!conn) {
      if (!this.isPrimary()) {
        const primaryId = this.election.getPrimaryId();
        if (primaryId) {
          const routeEnvelope: MeshEnvelope = {
            namespace: MESH_NAMESPACE,
            type: 'route:message',
            payload: { targetDeviceId: deviceId, envelope },
          };
          return this.sendEnvelopeDirect(primaryId, routeEnvelope);
        }
      }
      this.log.warn(`No connection to device ${deviceId}`);
      return false;
    }

    return this.sendEnvelopeDirect(deviceId, envelope);
  }

  broadcastEnvelope(envelope: MeshEnvelope): void {
    const localDeviceId = this.deviceManager.getDeviceId();

    if (this.isPrimary()) {
      for (const conn of this.transport.getConnections()) {
        if (conn.deviceId && conn.status === 'connected') {
          this.sendEnvelopeDirect(conn.deviceId, envelope);
        }
      }
      this.emit('message', {
        from: localDeviceId,
        connectionId: 'local',
        namespace: envelope.namespace,
        type: envelope.type,
        payload: envelope.payload,
      });
    } else {
      const primaryId = this.election.getPrimaryId();
      if (primaryId) {
        const routeEnvelope: MeshEnvelope = {
          namespace: MESH_NAMESPACE,
          type: 'route:broadcast',
          payload: { envelope },
        };
        this.sendEnvelopeDirect(primaryId, routeEnvelope);
      }
    }
  }

  private sendEnvelopeDirect(deviceId: string, envelope: MeshEnvelope): boolean {
    const conn = this.transport.getConnectionByDeviceId(deviceId);
    if (!conn) return false;

    const data = JSON.stringify({
      ...envelope,
      timestamp: envelope.timestamp ?? Date.now(),
    });
    return this.transport.sendRaw(conn.id, data);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // MESSAGE BUS
  // ─────────────────────────────────────────────────────────────────────────

  getMessageBus(): IMessageBus {
    if (!this.messageBus) {
      this.messageBus = new MeshMessageBus(this);
    }
    return this.messageBus;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // INTERNAL ACCESS
  // ─────────────────────────────────────────────────────────────────────────

  getTransport(): WebSocketTransport {
    return this.transport;
  }

  getDeviceManager(): DeviceManager {
    return this.deviceManager;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - SETUP
  // ─────────────────────────────────────────────────────────────────────────

  private setupDeviceManagerListeners(): void {
    this.deviceManager.on('deviceDiscovered', (device) => this.emit('deviceDiscovered', device));
    this.deviceManager.on('deviceUpdated', (device) => this.emit('deviceUpdated', device));
    this.deviceManager.on('deviceOffline', (deviceId) => {
      this.emit('deviceOffline', deviceId);
      if (deviceId === this.election.getPrimaryId()) {
        this.election.handlePrimaryLost(deviceId);
      }
    });
    this.deviceManager.on('devicesChanged', (devices) => this.emit('devicesChanged', devices));
    this.deviceManager.on('primaryChanged', (primaryId) => this.emit('primaryChanged', primaryId));
    this.deviceManager.on('localDeviceChanged', () => {
      if (this.running) {
        this.broadcastAnnounce();
      }
    });
  }

  private setupSidecarListeners(): void {
    this.sidecarHandlers.statusChanged = (status: SidecarStatus) => {
      if (status.tailscaleIP) {
        this.deviceManager.setLocalOnline(status.tailscaleIP, this.startedAt, status.dnsName);
      }
      if (status.dnsName) {
        this.deviceManager.setLocalDNSName(status.dnsName);
      }
      if (status.state === 'error') {
        this.emit('error', new Error(status.error || 'Tailscale error'));
      }
    };

    this.sidecarHandlers.authRequired = (authUrl: string) => {
      this.emit('authRequired', authUrl);
    };

    this.sidecarHandlers.tailnetPeers = (peers: TailnetPeer[]) => {
      this.handleTailnetPeers(peers);
    };

    this.sidecarHandlers.error = (error: Error) => {
      this.emit('error', error);
    };

    this.sidecar.on('statusChanged', this.sidecarHandlers.statusChanged!);
    this.sidecar.on('authRequired', this.sidecarHandlers.authRequired!);
    this.sidecar.on('tailnetPeers', this.sidecarHandlers.tailnetPeers!);
    this.sidecar.on('error', this.sidecarHandlers.error!);
  }

  private removeSidecarListeners(): void {
    if (this.sidecarHandlers.statusChanged) this.sidecar.off('statusChanged', this.sidecarHandlers.statusChanged);
    if (this.sidecarHandlers.authRequired) this.sidecar.off('authRequired', this.sidecarHandlers.authRequired);
    if (this.sidecarHandlers.tailnetPeers) this.sidecar.off('tailnetPeers', this.sidecarHandlers.tailnetPeers);
    if (this.sidecarHandlers.error) this.sidecar.off('error', this.sidecarHandlers.error);
    this.sidecarHandlers = {};
  }

  private setupTransportListeners(): void {
    const localDeviceId = this.deviceManager.getDeviceId();

    this.transportHandlers.connected = (conn: WSConnection) => {
      this.log.info(`WS connected: ${conn.id} (${conn.direction})`);

      const localDevice = this.deviceManager.getLocalDevice();
      const announce = createMeshMessage<DeviceAnnouncePayload>(
        'device:announce',
        localDeviceId,
        { device: localDevice, protocolVersion: 2 }
      );
      this.sendMeshMessageRaw(conn.id, announce);

      if (this.isPrimary()) {
        const devices = [localDevice, ...this.deviceManager.getDevices()];
        const listMessage = createMeshMessage<DeviceListPayload>(
          'device:list',
          localDeviceId,
          { devices, primaryId: localDeviceId }
        );
        this.sendMeshMessageRaw(conn.id, listMessage);
      }
    };

    this.transportHandlers.disconnected = (connectionId: string, reason: string) => {
      this.log.info(`WS disconnected: ${connectionId} (${reason})`);

      for (const device of this.deviceManager.getDevices()) {
        const conn = this.transport.getConnectionByDeviceId(device.id);
        if (!conn) {
          this.deviceManager.markDeviceOffline(device.id);
        }
      }
    };

    this.transportHandlers.deviceIdentified = (connectionId: string, deviceId: string) => {
      this.log.info(`Device identified: ${connectionId} -> ${deviceId}`);
    };

    this.transportHandlers.data = (connectionId: string, data: string) => {
      this.handleIncomingData(connectionId, data);
    };

    this.transport.on('connected', this.transportHandlers.connected!);
    this.transport.on('disconnected', this.transportHandlers.disconnected!);
    this.transport.on('deviceIdentified', this.transportHandlers.deviceIdentified!);
    this.transport.on('data', this.transportHandlers.data!);
  }

  private removeTransportListeners(): void {
    if (this.transportHandlers.connected) this.transport.off('connected', this.transportHandlers.connected);
    if (this.transportHandlers.disconnected) this.transport.off('disconnected', this.transportHandlers.disconnected);
    if (this.transportHandlers.deviceIdentified) this.transport.off('deviceIdentified', this.transportHandlers.deviceIdentified);
    if (this.transportHandlers.data) this.transport.off('data', this.transportHandlers.data);
    this.transportHandlers = {};
  }

  private setupElectionListeners(): void {
    this.election.on('primaryElected', (deviceId: string, isLocal: boolean) => {
      this.log.info(`Primary elected: ${deviceId} (isLocal=${isLocal})`);

      this.deviceManager.setLocalRole(isLocal ? 'primary' : 'secondary');

      for (const device of this.deviceManager.getDevices()) {
        this.deviceManager.setDeviceRole(device.id, device.id === deviceId ? 'primary' : 'secondary');
      }

      this.emit('roleChanged', isLocal ? 'primary' : 'secondary', isLocal);

      if (isLocal) {
        this.broadcastDeviceList();
      }
    });

    this.election.on('primaryLost', (previousPrimaryId: string) => {
      this.log.info(`Primary lost: ${previousPrimaryId}`);
    });

    this.election.on('broadcast', (message: MeshMessage) => {
      this.broadcastMeshMessage(message);
    });
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - INCOMING DATA
  // ─────────────────────────────────────────────────────────────────────────

  private handleIncomingData(connectionId: string, data: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(data);
    } catch (e) {
      this.log.error('Failed to parse JSON:', e);
      return;
    }

    const result = MeshEnvelopeSchema.safeParse(parsed);
    if (!result.success) {
      this.log.warn('Invalid mesh envelope:', result.error.message);
      return;
    }

    const envelope = result.data;
    const conn = this.transport.getConnection(connectionId);
    const fromDeviceId = conn?.deviceId ?? undefined;

    if (envelope.namespace === MESH_NAMESPACE) {
      if (envelope.type === 'message') {
        const message = envelope.payload as MeshMessage;
        if (!message || !message.type) return;

        if (message.type === 'device:announce') {
          const announceResult = DeviceAnnouncePayloadSchema.safeParse(message.payload);
          if (announceResult.success) {
            this.transport.setConnectionDeviceId(connectionId, announceResult.data.device.id);
          }
        }

        this.handleMeshMessageInternal(message);
      } else {
        this.handleRouteEnvelope(connectionId, envelope);
      }
      return;
    }

    this.emit('message', {
      from: fromDeviceId,
      connectionId,
      namespace: envelope.namespace,
      type: envelope.type,
      payload: envelope.payload,
    });
  }

  private handleRouteEnvelope(connectionId: string, envelope: MeshEnvelope): void {
    const conn = this.transport.getConnection(connectionId);
    const fromDeviceId = conn?.deviceId ?? undefined;

    if (envelope.type === 'route:message') {
      if (!this.isPrimary()) {
        this.log.warn('Received route:message but not primary');
        return;
      }
      const { targetDeviceId, envelope: innerEnvelope } = envelope.payload as {
        targetDeviceId: string;
        envelope: MeshEnvelope;
      };
      this.sendEnvelopeDirect(targetDeviceId, innerEnvelope);
    } else if (envelope.type === 'route:broadcast') {
      if (!this.isPrimary()) {
        this.log.warn('Received route:broadcast but not primary');
        return;
      }
      const { envelope: innerEnvelope } = envelope.payload as { envelope: MeshEnvelope };

      for (const c of this.transport.getConnections()) {
        if (c.deviceId && c.deviceId !== fromDeviceId) {
          this.sendEnvelopeDirect(c.deviceId, innerEnvelope);
        }
      }

      this.emit('message', {
        from: fromDeviceId,
        connectionId,
        namespace: innerEnvelope.namespace,
        type: innerEnvelope.type,
        payload: innerEnvelope.payload,
      });
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - MESSAGING HELPERS
  // ─────────────────────────────────────────────────────────────────────────

  private sendMeshMessageRaw(connectionId: string, message: MeshMessage): boolean {
    const envelope: MeshEnvelope = {
      namespace: MESH_NAMESPACE,
      type: 'message',
      payload: message,
      timestamp: Date.now(),
    };
    const data = JSON.stringify(envelope);
    return this.transport.sendRaw(connectionId, data);
  }

  private sendMeshMessageToDevice(deviceId: string, message: MeshMessage): boolean {
    const conn = this.transport.getConnectionByDeviceId(deviceId);
    if (!conn) {
      this.log.warn(`No connection to device ${deviceId}`);
      return false;
    }
    return this.sendMeshMessageRaw(conn.id, message);
  }

  private broadcastMeshMessage(message: MeshMessage): void {
    for (const conn of this.transport.getConnections()) {
      if (conn.status === 'connected') {
        this.sendMeshMessageRaw(conn.id, message);
      }
    }
  }

  private handleMeshMessageInternal(message: MeshMessage): void {
    switch (message.type) {
      case 'device:announce': {
        const result = DeviceAnnouncePayloadSchema.safeParse(message.payload);
        if (!result.success) {
          this.log.warn('Invalid device:announce payload:', result.error.message);
          return;
        }
        this.deviceManager.handleDeviceAnnounce(message.from, result.data);
        break;
      }
      case 'device:list': {
        const result = DeviceListPayloadSchema.safeParse(message.payload);
        if (!result.success) {
          this.log.warn('Invalid device:list payload:', result.error.message);
          return;
        }
        this.deviceManager.handleDeviceList(message.from, result.data);
        if (result.data.primaryId) {
          this.election.setPrimary(result.data.primaryId);
        }
        break;
      }
      case 'device:goodbye':
        this.deviceManager.handleDeviceGoodbye(message.from);
        break;
      case 'election:start':
        this.election.handleElectionStart(message.from);
        break;
      case 'election:candidate': {
        const result = ElectionCandidatePayloadSchema.safeParse(message.payload);
        if (!result.success) {
          this.log.warn('Invalid election:candidate payload:', result.error.message);
          return;
        }
        this.election.handleElectionCandidate(message.from, result.data);
        break;
      }
      case 'election:result': {
        const result = ElectionResultPayloadSchema.safeParse(message.payload);
        if (!result.success) {
          this.log.warn('Invalid election:result payload:', result.error.message);
          return;
        }
        this.election.handleElectionResult(message.from, result.data);
        break;
      }
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - DISCOVERY
  // ─────────────────────────────────────────────────────────────────────────

  private handleTailnetPeers(peers: TailnetPeer[]): void {
    const identity = this.deviceManager.getDeviceIdentity();
    this.log.info(`Discovered ${peers.length} tailnet peers`);

    for (const peer of peers) {
      if (peer.hostname === identity.tailscaleHostname) continue;

      const device = this.deviceManager.addDiscoveredPeer(peer);
      if (!device) continue;

      if (peer.online) {
        const conn = this.transport.getConnectionByDeviceId(device.id);
        if (!conn || conn.status !== 'connected') {
          this.transport.connect(device.id, device.tailscaleHostname, device.tailscaleDNSName, 443).catch((err) => {
            this.log.error(`Failed to connect to ${device.name}:`, err);
          });
        }
      }
    }

    const onlineDevices = this.deviceManager.getOnlineDevices();
    if (!this.election.getPrimaryId() && onlineDevices.length > 0) {
      setTimeout(() => {
        if (!this.election.getPrimaryId()) {
          this.election.handleNoPrimaryOnStartup();
        }
      }, this.discoveryTimeoutMs);
    } else if (onlineDevices.length === 0 && !this.election.getPrimaryId()) {
      this.log.info('No other devices found, becoming primary');
      this.deviceManager.setLocalRole('primary');
      this.election.setPrimary(identity.id);
      this.emit('roleChanged', 'primary', true);
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - ANNOUNCEMENTS
  // ─────────────────────────────────────────────────────────────────────────

  private broadcastAnnounce(type: 'announce' | 'goodbye' = 'announce'): void {
    const localDeviceId = this.deviceManager.getDeviceId();

    if (type === 'goodbye') {
      const goodbye = createMeshMessage('device:goodbye', localDeviceId, {
        deviceId: localDeviceId,
        reason: 'shutdown',
      });
      this.broadcastMeshMessage(goodbye);
      return;
    }

    const localDevice = this.deviceManager.getLocalDevice();
    const announce = createMeshMessage<DeviceAnnouncePayload>(
      'device:announce',
      localDeviceId,
      { device: localDevice, protocolVersion: 2 }
    );
    this.broadcastMeshMessage(announce);
  }

  private broadcastDeviceList(): void {
    const localDeviceId = this.deviceManager.getDeviceId();
    const localDevice = this.deviceManager.getLocalDevice();
    const devices = [localDevice, ...this.deviceManager.getDevices()];
    const message = createMeshMessage<DeviceListPayload>(
      'device:list',
      localDeviceId,
      { devices, primaryId: localDeviceId }
    );
    this.broadcastMeshMessage(message);
  }

  private startAnnounceInterval(): void {
    this.stopAnnounceInterval();
    this.announceInterval = setInterval(() => {
      this.broadcastAnnounce();
    }, this.announceIntervalMs);
  }

  private stopAnnounceInterval(): void {
    if (this.announceInterval) {
      clearInterval(this.announceInterval);
      this.announceInterval = null;
    }
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// FACTORY
// ═══════════════════════════════════════════════════════════════════════════

export function createMeshNode(config: MeshNodeConfig): MeshNode {
  return new MeshNode(config);
}
