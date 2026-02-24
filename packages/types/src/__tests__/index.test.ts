import { describe, it, expect } from 'vitest';
import {
  generateHostname,
  parseHostname,
  BaseDeviceSchema,
  MeshEnvelopeSchema,
  DeviceAnnouncePayloadSchema,
  DeviceListPayloadSchema,
  ElectionCandidatePayloadSchema,
  ElectionResultPayloadSchema,
  PingPayloadSchema,
  PongPayloadSchema,
  MeshErrorPayloadSchema,
  DeviceSliceSchema,
  SyncFullPayloadSchema,
  SyncUpdatePayloadSchema,
  SyncRequestPayloadSchema,
  SyncClearPayloadSchema,
  TailnetPeerSchema,
  createMeshMessage,
} from '../index.js';

describe('@vibecook/truffle-types', () => {
  // ─────────────────────────────────────────────────────────────────────
  // HOSTNAME UTILITIES
  // ─────────────────────────────────────────────────────────────────────

  describe('generateHostname', () => {
    it('generates hostname with prefix, type, and id', () => {
      expect(generateHostname('myapp', 'desktop', 'abc123')).toBe('myapp-desktop-abc123');
    });

    it('works with different prefixes', () => {
      expect(generateHostname('truffle', 'server', 'xyz')).toBe('truffle-server-xyz');
    });
  });

  describe('parseHostname', () => {
    it('parses valid hostname', () => {
      const result = parseHostname('myapp', 'myapp-desktop-abc123');
      expect(result).toEqual({ type: 'desktop', id: 'abc123' });
    });

    it('returns null for non-matching prefix', () => {
      const result = parseHostname('myapp', 'other-desktop-abc123');
      expect(result).toBeNull();
    });

    it('returns null for malformed hostname', () => {
      expect(parseHostname('myapp', 'myapp')).toBeNull();
      expect(parseHostname('myapp', 'myapp-desktop')).toBeNull();
      expect(parseHostname('myapp', 'random-hostname')).toBeNull();
    });

    it('handles IDs with hyphens', () => {
      const result = parseHostname('myapp', 'myapp-desktop-abc-123-def');
      expect(result).toEqual({ type: 'desktop', id: 'abc-123-def' });
    });
  });

  // ─────────────────────────────────────────────────────────────────────
  // ZOD SCHEMAS
  // ─────────────────────────────────────────────────────────────────────

  describe('BaseDeviceSchema', () => {
    it('validates a valid device', () => {
      const device = {
        id: 'dev-1',
        type: 'desktop',
        name: 'My PC',
        tailscaleHostname: 'myapp-desktop-dev-1',
        status: 'online' as const,
        capabilities: ['pty', 'push'],
      };
      const result = BaseDeviceSchema.safeParse(device);
      expect(result.success).toBe(true);
    });

    it('validates device with optional fields', () => {
      const device = {
        id: 'dev-1',
        type: 'server',
        name: 'My Server',
        tailscaleHostname: 'myapp-server-dev-1',
        tailscaleDNSName: 'myapp-server-dev-1.tailnet.ts.net',
        tailscaleIP: '100.64.0.1',
        role: 'primary' as const,
        status: 'online' as const,
        capabilities: [],
        metadata: { key: 'value' },
        lastSeen: Date.now(),
        startedAt: Date.now(),
        os: 'darwin',
        latencyMs: 10,
      };
      const result = BaseDeviceSchema.safeParse(device);
      expect(result.success).toBe(true);
    });

    it('rejects invalid status', () => {
      const device = {
        id: 'dev-1',
        type: 'desktop',
        name: 'My PC',
        tailscaleHostname: 'myapp-desktop-dev-1',
        status: 'invalid',
        capabilities: [],
      };
      const result = BaseDeviceSchema.safeParse(device);
      expect(result.success).toBe(false);
    });
  });

  describe('MeshEnvelopeSchema', () => {
    it('validates valid envelope', () => {
      const envelope = {
        namespace: 'sync',
        type: 'full',
        payload: { data: 'test' },
      };
      const result = MeshEnvelopeSchema.safeParse(envelope);
      expect(result.success).toBe(true);
    });

    it('validates envelope with timestamp', () => {
      const envelope = {
        namespace: 'mesh',
        type: 'message',
        payload: {},
        timestamp: Date.now(),
      };
      const result = MeshEnvelopeSchema.safeParse(envelope);
      expect(result.success).toBe(true);
    });
  });

  describe('Message payload schemas', () => {
    it('validates DeviceAnnouncePayload', () => {
      const payload = {
        device: {
          id: 'dev-1',
          type: 'desktop',
          name: 'My PC',
          tailscaleHostname: 'myapp-desktop-dev-1',
          status: 'online',
          capabilities: [],
        },
        protocolVersion: 2,
      };
      expect(DeviceAnnouncePayloadSchema.safeParse(payload).success).toBe(true);
    });

    it('validates DeviceListPayload', () => {
      const payload = {
        devices: [
          {
            id: 'dev-1',
            type: 'desktop',
            name: 'PC',
            tailscaleHostname: 'h',
            status: 'online',
            capabilities: [],
          },
        ],
        primaryId: 'dev-1',
      };
      expect(DeviceListPayloadSchema.safeParse(payload).success).toBe(true);
    });

    it('validates ElectionCandidatePayload', () => {
      const payload = {
        deviceId: 'dev-1',
        uptime: 60000,
        userDesignated: false,
      };
      expect(ElectionCandidatePayloadSchema.safeParse(payload).success).toBe(true);
    });

    it('validates ElectionResultPayload', () => {
      const payload = {
        newPrimaryId: 'dev-1',
        reason: 'election',
      };
      expect(ElectionResultPayloadSchema.safeParse(payload).success).toBe(true);
    });

    it('validates PingPayload', () => {
      expect(PingPayloadSchema.safeParse({ timestamp: Date.now() }).success).toBe(true);
    });

    it('validates PongPayload', () => {
      expect(
        PongPayloadSchema.safeParse({ timestamp: Date.now(), echoTimestamp: Date.now() }).success,
      ).toBe(true);
    });

    it('validates MeshErrorPayload', () => {
      expect(MeshErrorPayloadSchema.safeParse({ code: 'ERR', message: 'test' }).success).toBe(true);
    });
  });

  describe('TailnetPeerSchema', () => {
    it('validates valid peer', () => {
      const peer = {
        id: 'peer-1',
        hostname: 'myapp-desktop-peer-1',
        dnsName: 'myapp-desktop-peer-1.tailnet.ts.net',
        tailscaleIPs: ['100.64.0.1'],
        online: true,
      };
      expect(TailnetPeerSchema.safeParse(peer).success).toBe(true);
    });
  });

  // ─────────────────────────────────────────────────────────────────────
  // STORE SYNC SCHEMAS
  // ─────────────────────────────────────────────────────────────────────

  describe('Store sync schemas', () => {
    it('validates DeviceSlice', () => {
      const slice = {
        deviceId: 'dev-1',
        data: { key: 'value' },
        updatedAt: Date.now(),
        version: 1,
      };
      expect(DeviceSliceSchema.safeParse(slice).success).toBe(true);
    });

    it('validates SyncFullPayload', () => {
      const payload = {
        storeId: 'mystore',
        deviceId: 'dev-1',
        data: { items: [] },
        version: 1,
        updatedAt: Date.now(),
      };
      expect(SyncFullPayloadSchema.safeParse(payload).success).toBe(true);
    });

    it('validates SyncUpdatePayload', () => {
      const payload = {
        storeId: 'mystore',
        deviceId: 'dev-1',
        data: { items: [1, 2, 3] },
        version: 2,
        updatedAt: Date.now(),
      };
      expect(SyncUpdatePayloadSchema.safeParse(payload).success).toBe(true);
    });

    it('validates SyncRequestPayload', () => {
      expect(SyncRequestPayloadSchema.safeParse({ storeId: 'mystore' }).success).toBe(true);
      expect(
        SyncRequestPayloadSchema.safeParse({ storeId: 'mystore', fromDeviceId: 'dev-1' }).success,
      ).toBe(true);
    });

    it('validates SyncClearPayload', () => {
      const payload = {
        storeId: 'mystore',
        deviceId: 'dev-1',
        reason: 'offline',
      };
      expect(SyncClearPayloadSchema.safeParse(payload).success).toBe(true);
    });
  });

  // ─────────────────────────────────────────────────────────────────────
  // createMeshMessage
  // ─────────────────────────────────────────────────────────────────────

  describe('createMeshMessage', () => {
    it('creates a message with required fields', () => {
      const msg = createMeshMessage('device:announce', 'dev-1', { name: 'test' });
      expect(msg.type).toBe('device:announce');
      expect(msg.from).toBe('dev-1');
      expect(msg.payload).toEqual({ name: 'test' });
      expect(msg.timestamp).toBeTypeOf('number');
      expect(msg.to).toBeUndefined();
      expect(msg.correlationId).toBeUndefined();
    });

    it('creates a message with optional fields', () => {
      const msg = createMeshMessage('route:message', 'dev-1', {}, 'dev-2', 'corr-1');
      expect(msg.to).toBe('dev-2');
      expect(msg.correlationId).toBe('corr-1');
    });
  });
});
