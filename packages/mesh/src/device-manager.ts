/**
 * DeviceManager - Manages device state for the mesh network
 *
 * Generic - no hardcoded device types, hostname prefixes, or app-specific logic.
 * All config provided via constructor injection.
 */

import { TypedEventEmitter, parseHostname, createLogger } from '@vibecook/truffle-types';
import type {
  BaseDevice,
  DeviceRole,
  DeviceAnnouncePayload,
  DeviceListPayload,
  TailnetPeer,
  Logger,
} from '@vibecook/truffle-types';

// ═══════════════════════════════════════════════════════════════════════════
// TYPES
// ═══════════════════════════════════════════════════════════════════════════

export interface DeviceIdentity {
  id: string;
  type: string;
  name: string;
  tailscaleHostname: string;
}

export interface DeviceManagerEvents {
  deviceDiscovered: (device: BaseDevice) => void;
  deviceUpdated: (device: BaseDevice) => void;
  deviceOffline: (deviceId: string) => void;
  devicesChanged: (devices: BaseDevice[]) => void;
  primaryChanged: (primaryId: string | null) => void;
  localDeviceChanged: (device: BaseDevice) => void;
}

interface DeviceManagerConfig {
  identity: DeviceIdentity;
  hostnamePrefix: string;
  capabilities?: string[];
  metadata?: Record<string, unknown>;
  logger?: Logger;
}

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

export class DeviceManager extends TypedEventEmitter<DeviceManagerEvents> {
  private readonly identity: DeviceIdentity;
  private readonly hostnamePrefix: string;
  private localDevice: BaseDevice;
  private devices = new Map<string, BaseDevice>();
  private primaryId: string | null = null;
  private readonly log: Logger;

  constructor(config: DeviceManagerConfig) {
    super();
    this.identity = config.identity;
    this.hostnamePrefix = config.hostnamePrefix;
    this.log = config.logger ?? createLogger('DeviceManager');

    this.localDevice = {
      id: config.identity.id,
      type: config.identity.type,
      name: config.identity.name,
      tailscaleHostname: config.identity.tailscaleHostname,
      status: 'offline',
      capabilities: config.capabilities ?? [],
      metadata: config.metadata,
    };
  }

  // ─────────────────────────────────────────────────────────────────────────
  // IDENTITY
  // ─────────────────────────────────────────────────────────────────────────

  getDeviceId(): string {
    return this.identity.id;
  }

  getDeviceType(): string {
    return this.identity.type;
  }

  getDeviceIdentity(): DeviceIdentity {
    return { ...this.identity };
  }

  // ─────────────────────────────────────────────────────────────────────────
  // LOCAL DEVICE
  // ─────────────────────────────────────────────────────────────────────────

  getLocalDevice(): BaseDevice {
    return { ...this.localDevice };
  }

  setLocalOnline(tailscaleIP: string, startedAt: number, dnsName?: string): void {
    this.localDevice.tailscaleIP = tailscaleIP;
    this.localDevice.status = 'online';
    this.localDevice.startedAt = startedAt;
    if (dnsName) {
      this.localDevice.tailscaleDNSName = dnsName;
    }
    this.emit('localDeviceChanged', this.getLocalDevice());
  }

  setLocalDNSName(dnsName: string): void {
    if (this.localDevice.tailscaleDNSName !== dnsName) {
      this.localDevice.tailscaleDNSName = dnsName;
      this.emit('localDeviceChanged', this.getLocalDevice());
    }
  }

  setLocalOffline(): void {
    this.localDevice.status = 'offline';
    this.localDevice.tailscaleIP = undefined;
    this.emit('localDeviceChanged', this.getLocalDevice());
  }

  setLocalRole(role: DeviceRole): void {
    if (this.localDevice.role !== role) {
      this.localDevice.role = role;
      if (role === 'primary') {
        this.primaryId = this.identity.id;
      }
      this.emit('localDeviceChanged', this.getLocalDevice());
    }
  }

  updateDeviceName(name: string): void {
    this.localDevice.name = name;
    this.log.info(`Device name updated to: ${name}`);
    this.emit('localDeviceChanged', this.getLocalDevice());
  }

  updateMetadata(metadata: Record<string, unknown>): void {
    this.localDevice.metadata = { ...this.localDevice.metadata, ...metadata };
    this.emit('localDeviceChanged', this.getLocalDevice());
  }

  // ─────────────────────────────────────────────────────────────────────────
  // REMOTE DEVICES
  // ─────────────────────────────────────────────────────────────────────────

  getDevices(): BaseDevice[] {
    return Array.from(this.devices.values());
  }

  getDeviceById(id: string): BaseDevice | undefined {
    if (id === this.identity.id) {
      return this.getLocalDevice();
    }
    return this.devices.get(id);
  }

  getOnlineDevices(): BaseDevice[] {
    return this.getDevices().filter((d) => d.status === 'online');
  }

  getDevicesByType(type: string): BaseDevice[] {
    return this.getDevices().filter((d) => d.type === type);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // ELECTION SUPPORT
  // ─────────────────────────────────────────────────────────────────────────

  getElectionCandidates(): BaseDevice[] {
    return this.getOnlineDevices();
  }

  setDeviceRole(deviceId: string, role: DeviceRole): void {
    const device = this.devices.get(deviceId);
    if (device && device.role !== role) {
      device.role = role;
      if (role === 'primary') {
        this.primaryId = deviceId;
        this.emit('primaryChanged', deviceId);
      }
      this.emit('deviceUpdated', device);
    }
  }

  getPrimaryDevice(): BaseDevice | undefined {
    if (!this.primaryId) return undefined;
    return this.getDeviceById(this.primaryId);
  }

  getPrimaryId(): string | undefined {
    return this.primaryId ?? undefined;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // DEVICE LIFECYCLE
  // ─────────────────────────────────────────────────────────────────────────

  addDiscoveredPeer(peer: TailnetPeer): BaseDevice | null {
    if (peer.hostname === this.identity.tailscaleHostname) {
      return null;
    }

    const parsed = parseHostname(this.hostnamePrefix, peer.hostname);
    if (!parsed) {
      return null;
    }

    const device: BaseDevice = {
      id: parsed.id,
      type: parsed.type,
      name: peer.hostname,
      tailscaleHostname: peer.hostname,
      tailscaleDNSName: peer.dnsName,
      tailscaleIP: peer.tailscaleIPs[0],
      status: peer.online ? 'online' : 'offline',
      capabilities: [],
      os: peer.os,
    };

    const existing = this.devices.get(device.id);
    if (!existing) {
      this.devices.set(device.id, device);
      this.log.info(`Discovered new peer: ${device.name} (${device.type})`);
      this.emit('deviceDiscovered', device);
      this.emit('devicesChanged', this.getDevices());
    } else {
      existing.status = peer.online ? 'online' : 'offline';
      existing.tailscaleIP = peer.tailscaleIPs[0];
      existing.tailscaleDNSName = peer.dnsName;
      if (peer.os) {
        existing.os = peer.os;
      }
      this.emit('deviceUpdated', existing);
      this.emit('devicesChanged', this.getDevices());
    }

    return this.devices.get(device.id) ?? null;
  }

  handleDeviceAnnounce(from: string, payload: DeviceAnnouncePayload): void {
    const device = payload.device;
    const existing = this.devices.get(device.id);

    this.log.info(`Device announce from ${from}: ${device.name}`);

    if (existing?.tailscaleDNSName && !device.tailscaleDNSName) {
      device.tailscaleDNSName = existing.tailscaleDNSName;
    }

    this.devices.set(device.id, device);

    if (!existing) {
      this.emit('deviceDiscovered', device);
    } else {
      this.emit('deviceUpdated', device);
    }

    this.emit('devicesChanged', this.getDevices());
  }

  handleDeviceList(from: string, payload: DeviceListPayload): void {
    this.log.info(`Device list from ${from}: ${payload.devices.length} devices`);

    for (const device of payload.devices) {
      if (device.id !== this.identity.id) {
        const existing = this.devices.get(device.id);
        if (existing?.tailscaleDNSName && !device.tailscaleDNSName) {
          device.tailscaleDNSName = existing.tailscaleDNSName;
        }
        this.devices.set(device.id, device);
      }
    }

    if (payload.primaryId) {
      this.primaryId = payload.primaryId;

      if (payload.primaryId === this.identity.id) {
        this.localDevice.role = 'primary';
      } else {
        this.localDevice.role = 'secondary';
      }

      for (const device of this.devices.values()) {
        device.role = device.id === payload.primaryId ? 'primary' : 'secondary';
      }

      this.emit('primaryChanged', payload.primaryId);
    }

    this.emit('devicesChanged', this.getDevices());
  }

  handleDeviceGoodbye(deviceId: string): void {
    this.markDeviceOffline(deviceId);
  }

  markDeviceOffline(deviceId: string): void {
    const device = this.devices.get(deviceId);
    if (device) {
      device.status = 'offline';
      this.log.info(`Device offline: ${device.name}`);
      this.emit('deviceOffline', deviceId);
      this.emit('devicesChanged', this.getDevices());

      if (deviceId === this.primaryId) {
        this.primaryId = null;
        this.emit('primaryChanged', null);
      }
    }
  }

  clear(): void {
    this.devices.clear();
    this.primaryId = null;
    this.emit('devicesChanged', []);
  }
}
