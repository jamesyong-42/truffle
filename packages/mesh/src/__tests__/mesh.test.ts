import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { EventEmitter } from 'events';
import { DeviceManager, type DeviceIdentity } from '../device-manager.js';
import { PrimaryElection } from '../primary-election.js';
import { MeshMessageBus } from '../mesh-message-bus.js';
import type { BaseDevice, TailnetPeer } from '@vibecook/truffle-types';

// Quiet logger for tests
const quietLogger = {
  info: () => {},
  warn: () => {},
  error: () => {},
  debug: () => {},
};

describe('@vibecook/truffle-mesh', () => {
  // ─────────────────────────────────────────────────────────────────────
  // DeviceManager
  // ─────────────────────────────────────────────────────────────────────

  describe('DeviceManager', () => {
    const identity: DeviceIdentity = {
      id: 'dev-1',
      type: 'desktop',
      name: 'My PC',
      tailscaleHostname: 'myapp-desktop-dev-1',
    };

    let dm: DeviceManager;

    beforeEach(() => {
      dm = new DeviceManager({
        identity,
        hostnamePrefix: 'myapp',
        capabilities: ['pty'],
        logger: quietLogger,
      });
    });

    it('returns device identity', () => {
      expect(dm.getDeviceId()).toBe('dev-1');
      expect(dm.getDeviceType()).toBe('desktop');
      expect(dm.getDeviceIdentity().tailscaleHostname).toBe('myapp-desktop-dev-1');
    });

    it('returns local device', () => {
      const device = dm.getLocalDevice();
      expect(device.id).toBe('dev-1');
      expect(device.status).toBe('offline');
      expect(device.capabilities).toEqual(['pty']);
    });

    it('sets local device online', () => {
      const handler = vi.fn();
      dm.on('localDeviceChanged', handler);

      dm.setLocalOnline('100.64.0.1', Date.now(), 'myapp-desktop-dev-1.ts.net');

      const device = dm.getLocalDevice();
      expect(device.status).toBe('online');
      expect(device.tailscaleIP).toBe('100.64.0.1');
      expect(device.tailscaleDNSName).toBe('myapp-desktop-dev-1.ts.net');
      expect(handler).toHaveBeenCalled();
    });

    it('adds discovered peers', () => {
      const peer: TailnetPeer = {
        id: 'peer-2',
        hostname: 'myapp-desktop-dev-2',
        dnsName: 'myapp-desktop-dev-2.ts.net',
        tailscaleIPs: ['100.64.0.2'],
        online: true,
      };

      const discoveredHandler = vi.fn();
      dm.on('deviceDiscovered', discoveredHandler);

      const device = dm.addDiscoveredPeer(peer);
      expect(device).not.toBeNull();
      expect(device!.id).toBe('dev-2');
      expect(device!.type).toBe('desktop');
      expect(discoveredHandler).toHaveBeenCalled();
    });

    it('skips self peer', () => {
      const peer: TailnetPeer = {
        id: 'self',
        hostname: 'myapp-desktop-dev-1',
        dnsName: 'myapp-desktop-dev-1.ts.net',
        tailscaleIPs: ['100.64.0.1'],
        online: true,
      };

      const device = dm.addDiscoveredPeer(peer);
      expect(device).toBeNull();
    });

    it('skips non-matching hostname prefix', () => {
      const peer: TailnetPeer = {
        id: 'other',
        hostname: 'other-app-abc',
        dnsName: 'other-app-abc.ts.net',
        tailscaleIPs: ['100.64.0.3'],
        online: true,
      };

      const device = dm.addDiscoveredPeer(peer);
      expect(device).toBeNull();
    });

    it('updates existing peer', () => {
      const peer: TailnetPeer = {
        id: 'peer-2',
        hostname: 'myapp-desktop-dev-2',
        dnsName: 'myapp-desktop-dev-2.ts.net',
        tailscaleIPs: ['100.64.0.2'],
        online: true,
      };

      dm.addDiscoveredPeer(peer);

      const updatedPeer = { ...peer, online: false };
      const updatedHandler = vi.fn();
      dm.on('deviceUpdated', updatedHandler);

      dm.addDiscoveredPeer(updatedPeer);
      expect(updatedHandler).toHaveBeenCalled();

      const device = dm.getDeviceById('dev-2');
      expect(device!.status).toBe('offline');
    });

    it('handles device announce', () => {
      const announceDevice: BaseDevice = {
        id: 'dev-3',
        type: 'desktop',
        name: 'Remote PC',
        tailscaleHostname: 'myapp-desktop-dev-3',
        status: 'online',
        capabilities: ['pty'],
        metadata: { version: '1.0' },
      };

      dm.handleDeviceAnnounce('dev-3', {
        device: announceDevice,
        protocolVersion: 2,
      });

      const device = dm.getDeviceById('dev-3');
      expect(device).toBeDefined();
      expect(device!.name).toBe('Remote PC');
    });

    it('handles device list from primary', () => {
      const devices: BaseDevice[] = [
        {
          id: 'dev-1',
          type: 'desktop',
          name: 'Me',
          tailscaleHostname: 'myapp-desktop-dev-1',
          status: 'online',
          capabilities: [],
        },
        {
          id: 'dev-2',
          type: 'desktop',
          name: 'Other',
          tailscaleHostname: 'myapp-desktop-dev-2',
          status: 'online',
          capabilities: [],
          role: 'secondary',
        },
      ];

      const primaryChangedHandler = vi.fn();
      dm.on('primaryChanged', primaryChangedHandler);

      dm.handleDeviceList('dev-1', { devices, primaryId: 'dev-1' });

      expect(dm.getPrimaryId()).toBe('dev-1');
      expect(primaryChangedHandler).toHaveBeenCalledWith('dev-1');
    });

    it('marks device offline', () => {
      const peer: TailnetPeer = {
        id: 'peer',
        hostname: 'myapp-desktop-dev-2',
        dnsName: 'myapp-desktop-dev-2.ts.net',
        tailscaleIPs: ['100.64.0.2'],
        online: true,
      };
      dm.addDiscoveredPeer(peer);

      const offlineHandler = vi.fn();
      dm.on('deviceOffline', offlineHandler);

      dm.markDeviceOffline('dev-2');
      expect(offlineHandler).toHaveBeenCalledWith('dev-2');
    });

    it('clears primary when primary goes offline', () => {
      dm.setLocalRole('primary');
      expect(dm.getPrimaryId()).toBe('dev-1');

      // Add a remote device and make it primary
      const peer: TailnetPeer = {
        id: 'peer',
        hostname: 'myapp-desktop-dev-2',
        dnsName: '',
        tailscaleIPs: ['100.64.0.2'],
        online: true,
      };
      dm.addDiscoveredPeer(peer);
      dm.setDeviceRole('dev-2', 'primary');

      expect(dm.getPrimaryId()).toBe('dev-2');

      const primaryChangedHandler = vi.fn();
      dm.on('primaryChanged', primaryChangedHandler);

      dm.markDeviceOffline('dev-2');
      expect(dm.getPrimaryId()).toBeUndefined();
      expect(primaryChangedHandler).toHaveBeenCalledWith(null);
    });

    it('updates metadata', () => {
      dm.updateMetadata({ key1: 'value1' });
      expect(dm.getLocalDevice().metadata).toEqual({ key1: 'value1' });

      dm.updateMetadata({ key2: 'value2' });
      expect(dm.getLocalDevice().metadata).toEqual({ key1: 'value1', key2: 'value2' });
    });
  });

  // ─────────────────────────────────────────────────────────────────────
  // PrimaryElection
  // ─────────────────────────────────────────────────────────────────────

  describe('PrimaryElection', () => {
    let election: PrimaryElection;

    beforeEach(() => {
      vi.useFakeTimers();
      election = new PrimaryElection(quietLogger);
      election.configure({
        deviceId: 'dev-1',
        startedAt: Date.now() - 60000, // 60s uptime
        preferPrimary: false,
      });
    });

    afterEach(() => {
      election.reset();
      vi.useRealTimers();
    });

    it('starts unconfigured as not primary', () => {
      const fresh = new PrimaryElection(quietLogger);
      expect(fresh.isPrimary()).toBe(false);
      expect(fresh.getPrimaryId()).toBeNull();
    });

    it('becomes primary when no other candidates', () => {
      const primaryHandler = vi.fn();
      election.on('primaryElected', primaryHandler);

      election.handleNoPrimaryOnStartup();
      vi.advanceTimersByTime(4000); // past election timeout

      expect(primaryHandler).toHaveBeenCalledWith('dev-1', true);
      expect(election.isPrimary()).toBe(true);
    });

    it('selects winner by uptime', () => {
      const primaryHandler = vi.fn();
      election.on('primaryElected', primaryHandler);

      election.handleNoPrimaryOnStartup();

      // Another candidate with longer uptime
      election.handleElectionCandidate('dev-2', {
        deviceId: 'dev-2',
        uptime: 120000, // 2 min
        userDesignated: false,
      });

      vi.advanceTimersByTime(4000);

      expect(primaryHandler).toHaveBeenCalledWith('dev-2', false);
      expect(election.isPrimary()).toBe(false);
      expect(election.getPrimaryId()).toBe('dev-2');
    });

    it('user-designated wins over longer uptime', () => {
      election.configure({
        deviceId: 'dev-1',
        startedAt: Date.now() - 10000, // 10s uptime (shorter)
        preferPrimary: true,
      });

      const primaryHandler = vi.fn();
      election.on('primaryElected', primaryHandler);

      election.handleNoPrimaryOnStartup();

      election.handleElectionCandidate('dev-2', {
        deviceId: 'dev-2',
        uptime: 120000,
        userDesignated: false,
      });

      vi.advanceTimersByTime(4000);

      expect(primaryHandler).toHaveBeenCalledWith('dev-1', true);
      expect(election.isPrimary()).toBe(true);
    });

    it('alphabetical tiebreaker when equal', () => {
      const primaryHandler = vi.fn();
      election.on('primaryElected', primaryHandler);

      election.handleNoPrimaryOnStartup();

      election.handleElectionCandidate('aaa', {
        deviceId: 'aaa',
        uptime: 60000, // same uptime
        userDesignated: false,
      });

      vi.advanceTimersByTime(4000);

      // 'aaa' < 'dev-1' alphabetically, so 'aaa' wins
      expect(primaryHandler).toHaveBeenCalledWith('aaa', false);
    });

    it('handles election result from remote', () => {
      const primaryHandler = vi.fn();
      election.on('primaryElected', primaryHandler);

      election.handleElectionResult('dev-2', {
        newPrimaryId: 'dev-2',
        reason: 'election',
      });

      expect(election.getPrimaryId()).toBe('dev-2');
      expect(election.isPrimary()).toBe(false);
      expect(primaryHandler).toHaveBeenCalledWith('dev-2', false);
    });

    it('handles primary loss with grace period', () => {
      election.setPrimary('dev-2');

      const broadcastHandler = vi.fn();
      election.on('broadcast', broadcastHandler);

      election.handlePrimaryLost('dev-2');
      expect(election.getPrimaryId()).toBeNull();

      // Grace period not expired yet
      vi.advanceTimersByTime(3000);
      expect(election.getPhase()).toBe('waiting');

      // After grace period, election starts
      vi.advanceTimersByTime(3000);
      expect(election.getPhase()).toBe('collecting');
    });

    it('joins election on election:start', () => {
      const broadcastHandler = vi.fn();
      election.on('broadcast', broadcastHandler);

      election.handleElectionStart('dev-2');

      // Should have broadcast our candidacy
      expect(broadcastHandler).toHaveBeenCalledWith(
        expect.objectContaining({ type: 'election:candidate' }),
      );
    });

    it('setPrimary directly sets primary', () => {
      election.setPrimary('dev-3');
      expect(election.getPrimaryId()).toBe('dev-3');
      expect(election.getPhase()).toBe('idle');
    });
  });

  // ─────────────────────────────────────────────────────────────────────
  // MeshMessageBus
  // ─────────────────────────────────────────────────────────────────────

  describe('MeshMessageBus', () => {
    it('subscribes and dispatches messages', () => {
      // Create a mock MeshNode
      const mockNode = new EventEmitter();
      (mockNode as any).getDeviceId = () => 'dev-1';
      (mockNode as any).isRunning = () => true;
      (mockNode as any).sendEnvelope = vi.fn().mockReturnValue(true);
      (mockNode as any).broadcastEnvelope = vi.fn();

      const bus = new MeshMessageBus(mockNode as any);

      const handler = vi.fn();
      const unsub = bus.subscribe('sync', handler);

      // Simulate incoming message
      mockNode.emit('message', {
        from: 'dev-2',
        connectionId: 'conn-1',
        namespace: 'sync',
        type: 'update',
        payload: { data: 'test' },
      });

      expect(handler).toHaveBeenCalledWith({
        from: 'dev-2',
        namespace: 'sync',
        type: 'update',
        payload: { data: 'test' },
      });

      unsub();

      // After unsubscribe, handler should not be called
      mockNode.emit('message', {
        from: 'dev-2',
        connectionId: 'conn-1',
        namespace: 'sync',
        type: 'update',
        payload: { data: 'test2' },
      });

      expect(handler).toHaveBeenCalledTimes(1);
    });

    it('does not dispatch to wrong namespace', () => {
      const mockNode = new EventEmitter();
      (mockNode as any).getDeviceId = () => 'dev-1';
      (mockNode as any).isRunning = () => true;

      const bus = new MeshMessageBus(mockNode as any);

      const handler = vi.fn();
      bus.subscribe('sync', handler);

      mockNode.emit('message', {
        from: 'dev-2',
        connectionId: 'conn-1',
        namespace: 'pty',
        type: 'output',
        payload: 'data',
      });

      expect(handler).not.toHaveBeenCalled();
    });

    it('publishes to mesh node', () => {
      const mockNode = new EventEmitter();
      (mockNode as any).getDeviceId = () => 'dev-1';
      (mockNode as any).isRunning = () => true;
      (mockNode as any).sendEnvelope = vi.fn().mockReturnValue(true);

      const bus = new MeshMessageBus(mockNode as any);

      bus.publish('dev-2', 'sync', 'update', { data: 'test' });

      expect((mockNode as any).sendEnvelope).toHaveBeenCalledWith('dev-2', {
        namespace: 'sync',
        type: 'update',
        payload: { data: 'test' },
      });
    });

    it('broadcasts via mesh node', () => {
      const mockNode = new EventEmitter();
      (mockNode as any).getDeviceId = () => 'dev-1';
      (mockNode as any).isRunning = () => true;
      (mockNode as any).broadcastEnvelope = vi.fn();

      const bus = new MeshMessageBus(mockNode as any);

      bus.broadcast('sync', 'request', { storeId: 'mystore' });

      expect((mockNode as any).broadcastEnvelope).toHaveBeenCalledWith({
        namespace: 'sync',
        type: 'request',
        payload: { storeId: 'mystore' },
      });
    });

    it('getSubscribedNamespaces returns current subscriptions', () => {
      const mockNode = new EventEmitter();
      (mockNode as any).getDeviceId = () => 'dev-1';
      (mockNode as any).isRunning = () => true;

      const bus = new MeshMessageBus(mockNode as any);

      bus.subscribe('sync', () => {});
      bus.subscribe('custom', () => {});

      expect(bus.getSubscribedNamespaces()).toContain('sync');
      expect(bus.getSubscribedNamespaces()).toContain('custom');
    });

    it('dispose cleans up', () => {
      const mockNode = new EventEmitter();
      (mockNode as any).getDeviceId = () => 'dev-1';
      (mockNode as any).isRunning = () => true;

      const bus = new MeshMessageBus(mockNode as any);

      const handler = vi.fn();
      bus.subscribe('sync', handler);

      bus.dispose();

      expect(bus.getSubscribedNamespaces()).toHaveLength(0);
    });
  });
});
