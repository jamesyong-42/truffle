/**
 * Integration test: Two simulated mesh nodes communicating through
 * DeviceManager + PrimaryElection + MeshMessageBus with a mock transport.
 *
 * Verifies end-to-end: device discovery → primary election → message routing.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { EventEmitter } from 'events';
import { DeviceManager, type DeviceIdentity } from '../device-manager.js';
import { PrimaryElection } from '../primary-election.js';
import { MeshMessageBus } from '../mesh-message-bus.js';
import type { BaseDevice, TailnetPeer, DeviceAnnouncePayload } from '@vibecook/truffle-types';

const quietLogger = {
  info: () => {},
  warn: () => {},
  error: () => {},
  debug: () => {},
};

// ─────────────────────────────────────────────────────────────────────────────
// Mock transport: Two nodes connected via a simple "wire"
// ─────────────────────────────────────────────────────────────────────────────

interface MockMeshNode {
  id: string;
  dm: DeviceManager;
  election: PrimaryElection;
  node: EventEmitter; // simulates MeshNode for MeshMessageBus
  bus: MeshMessageBus;
}

function createMockNode(id: string, type: string, name: string, startedAt: number): MockMeshNode {
  const identity: DeviceIdentity = {
    id,
    type,
    name,
    tailscaleHostname: `app-${type}-${id}`,
  };

  const dm = new DeviceManager({
    identity,
    hostnamePrefix: 'app',
    capabilities: ['pty'],
    logger: quietLogger,
  });

  const election = new PrimaryElection(quietLogger);
  election.configure({ deviceId: id, startedAt, preferPrimary: false });

  // Simulate MeshNode EventEmitter with required methods
  const node = new EventEmitter();
  (node as any).getDeviceId = () => id;
  (node as any).isRunning = () => true;
  (node as any).sendEnvelope = vi.fn().mockReturnValue(true);
  (node as any).broadcastEnvelope = vi.fn();

  const bus = new MeshMessageBus(node as any);

  return { id, dm, election, node, bus };
}

/**
 * Connects two mock nodes: messages from node1.broadcastEnvelope
 * are delivered to node2's 'message' event, and vice versa.
 */
function connectNodes(node1: MockMeshNode, node2: MockMeshNode): void {
  (node1.node as any).broadcastEnvelope = vi.fn((envelope: any) => {
    // Deliver to self
    node1.node.emit('message', {
      from: node1.id,
      connectionId: 'local',
      namespace: envelope.namespace,
      type: envelope.type,
      payload: envelope.payload,
    });
    // Deliver to peer
    node2.node.emit('message', {
      from: node1.id,
      connectionId: `conn-${node1.id}`,
      namespace: envelope.namespace,
      type: envelope.type,
      payload: envelope.payload,
    });
  });

  (node2.node as any).broadcastEnvelope = vi.fn((envelope: any) => {
    node2.node.emit('message', {
      from: node2.id,
      connectionId: 'local',
      namespace: envelope.namespace,
      type: envelope.type,
      payload: envelope.payload,
    });
    node1.node.emit('message', {
      from: node2.id,
      connectionId: `conn-${node2.id}`,
      namespace: envelope.namespace,
      type: envelope.type,
      payload: envelope.payload,
    });
  });

  (node1.node as any).sendEnvelope = vi.fn((deviceId: string, envelope: any) => {
    if (deviceId === node2.id) {
      node2.node.emit('message', {
        from: node1.id,
        connectionId: `conn-${node1.id}`,
        namespace: envelope.namespace,
        type: envelope.type,
        payload: envelope.payload,
      });
      return true;
    }
    return false;
  });

  (node2.node as any).sendEnvelope = vi.fn((deviceId: string, envelope: any) => {
    if (deviceId === node1.id) {
      node1.node.emit('message', {
        from: node2.id,
        connectionId: `conn-${node2.id}`,
        namespace: envelope.namespace,
        type: envelope.type,
        payload: envelope.payload,
      });
      return true;
    }
    return false;
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

describe('Integration: Two-node mesh', () => {
  let nodeA: MockMeshNode;
  let nodeB: MockMeshNode;

  beforeEach(() => {
    vi.useFakeTimers();

    // Node A started 120s ago (longer uptime → should win election)
    nodeA = createMockNode('dev-a', 'desktop', 'Desktop A', Date.now() - 120000);

    // Node B started 30s ago
    nodeB = createMockNode('dev-b', 'desktop', 'Desktop B', Date.now() - 30000);

    connectNodes(nodeA, nodeB);
  });

  afterEach(() => {
    nodeA.election.reset();
    nodeB.election.reset();
    nodeA.bus.dispose();
    nodeB.bus.dispose();
    vi.useRealTimers();
  });

  it('discovers devices via tailnet peers', () => {
    const peerB: TailnetPeer = {
      id: 'peer-b',
      hostname: 'app-desktop-dev-b',
      dnsName: 'app-desktop-dev-b.ts.net',
      tailscaleIPs: ['100.64.0.2'],
      online: true,
    };

    const discoveredHandler = vi.fn();
    nodeA.dm.on('deviceDiscovered', discoveredHandler);

    const device = nodeA.dm.addDiscoveredPeer(peerB);
    expect(device).not.toBeNull();
    expect(device!.id).toBe('dev-b');
    expect(device!.type).toBe('desktop');
    expect(discoveredHandler).toHaveBeenCalled();
  });

  it('exchanges device announce messages', () => {
    // Node A goes online
    nodeA.dm.setLocalOnline('100.64.0.1', Date.now() - 120000, 'app-desktop-dev-a.ts.net');

    // Node B goes online
    nodeB.dm.setLocalOnline('100.64.0.2', Date.now() - 30000, 'app-desktop-dev-b.ts.net');

    // Simulate device announce from A → B
    const announcePayload: DeviceAnnouncePayload = {
      device: nodeA.dm.getLocalDevice(),
      protocolVersion: 2,
    };
    nodeB.dm.handleDeviceAnnounce('dev-a', announcePayload);

    // Node B should now know about Node A
    const deviceA = nodeB.dm.getDeviceById('dev-a');
    expect(deviceA).toBeDefined();
    expect(deviceA!.name).toBe('Desktop A');
    expect(deviceA!.status).toBe('online');

    // Simulate device announce from B → A
    const announcePayloadB: DeviceAnnouncePayload = {
      device: nodeB.dm.getLocalDevice(),
      protocolVersion: 2,
    };
    nodeA.dm.handleDeviceAnnounce('dev-b', announcePayloadB);

    const deviceB = nodeA.dm.getDeviceById('dev-b');
    expect(deviceB).toBeDefined();
    expect(deviceB!.name).toBe('Desktop B');
  });

  it('elects primary by longest uptime', () => {
    const primaryElectedA = vi.fn();
    const primaryElectedB = vi.fn();
    nodeA.election.on('primaryElected', primaryElectedA);
    nodeB.election.on('primaryElected', primaryElectedB);

    // Both start election
    nodeA.election.handleNoPrimaryOnStartup();
    nodeB.election.handleNoPrimaryOnStartup();

    // Exchange candidates
    nodeA.election.handleElectionCandidate('dev-b', {
      deviceId: 'dev-b',
      uptime: 30000,
      userDesignated: false,
    });

    nodeB.election.handleElectionCandidate('dev-a', {
      deviceId: 'dev-a',
      uptime: 120000,
      userDesignated: false,
    });

    // Advance past election timeout
    vi.advanceTimersByTime(4000);

    // Node A should be primary (longer uptime)
    expect(nodeA.election.isPrimary()).toBe(true);
    expect(nodeA.election.getPrimaryId()).toBe('dev-a');

    // Node B should also know that dev-a won
    expect(nodeB.election.isPrimary()).toBe(false);
    expect(nodeB.election.getPrimaryId()).toBe('dev-a');
  });

  it('device list from primary syncs state', () => {
    // Set up A as primary
    nodeA.dm.setLocalOnline('100.64.0.1', Date.now() - 120000, 'app-desktop-dev-a.ts.net');
    nodeA.dm.setLocalRole('primary');

    // Add B as discovered on A
    const peerB: TailnetPeer = {
      id: 'peer-b',
      hostname: 'app-desktop-dev-b',
      dnsName: 'app-desktop-dev-b.ts.net',
      tailscaleIPs: ['100.64.0.2'],
      online: true,
    };
    nodeA.dm.addDiscoveredPeer(peerB);

    // Primary sends device list to Node B
    const devices: BaseDevice[] = [
      nodeA.dm.getLocalDevice(),
      ...nodeA.dm.getDevices(),
    ];

    const primaryChangedHandler = vi.fn();
    nodeB.dm.on('primaryChanged', primaryChangedHandler);

    nodeB.dm.handleDeviceList('dev-a', {
      devices,
      primaryId: 'dev-a',
    });

    expect(nodeB.dm.getPrimaryId()).toBe('dev-a');
    expect(primaryChangedHandler).toHaveBeenCalledWith('dev-a');
  });

  it('message bus delivers messages between nodes', () => {
    const syncHandler = vi.fn();
    nodeB.bus.subscribe('custom', syncHandler);

    // Node A sends a message to Node B
    nodeA.bus.publish('dev-b', 'custom', 'hello', { greeting: 'world' });

    expect((nodeA.node as any).sendEnvelope).toHaveBeenCalledWith('dev-b', {
      namespace: 'custom',
      type: 'hello',
      payload: { greeting: 'world' },
    });

    // Verify Node B received it
    expect(syncHandler).toHaveBeenCalledWith(
      expect.objectContaining({
        from: 'dev-a',
        namespace: 'custom',
        type: 'hello',
        payload: { greeting: 'world' },
      })
    );
  });

  it('message bus broadcasts to all nodes', () => {
    const handlerA = vi.fn();
    const handlerB = vi.fn();

    nodeA.bus.subscribe('events', handlerA);
    nodeB.bus.subscribe('events', handlerB);

    // Node A broadcasts
    nodeA.bus.broadcast('events', 'update', { version: 2 });

    expect((nodeA.node as any).broadcastEnvelope).toHaveBeenCalledWith({
      namespace: 'events',
      type: 'update',
      payload: { version: 2 },
    });

    // Both should receive
    expect(handlerA).toHaveBeenCalledWith(
      expect.objectContaining({ namespace: 'events', type: 'update' })
    );
    expect(handlerB).toHaveBeenCalledWith(
      expect.objectContaining({ namespace: 'events', type: 'update' })
    );
  });

  it('full lifecycle: discover → elect → communicate', () => {
    // 1. Both nodes come online
    nodeA.dm.setLocalOnline('100.64.0.1', Date.now() - 120000, 'app-desktop-dev-a.ts.net');
    nodeB.dm.setLocalOnline('100.64.0.2', Date.now() - 30000, 'app-desktop-dev-b.ts.net');

    // 2. Discover each other via tailnet peers
    const peerB: TailnetPeer = {
      id: 'peer-b',
      hostname: 'app-desktop-dev-b',
      dnsName: 'app-desktop-dev-b.ts.net',
      tailscaleIPs: ['100.64.0.2'],
      online: true,
    };
    const peerA: TailnetPeer = {
      id: 'peer-a',
      hostname: 'app-desktop-dev-a',
      dnsName: 'app-desktop-dev-a.ts.net',
      tailscaleIPs: ['100.64.0.1'],
      online: true,
    };

    nodeA.dm.addDiscoveredPeer(peerB);
    nodeB.dm.addDiscoveredPeer(peerA);

    // 3. Exchange device announces
    nodeB.dm.handleDeviceAnnounce('dev-a', {
      device: nodeA.dm.getLocalDevice(),
      protocolVersion: 2,
    });
    nodeA.dm.handleDeviceAnnounce('dev-b', {
      device: nodeB.dm.getLocalDevice(),
      protocolVersion: 2,
    });

    // 4. Run election
    nodeA.election.handleNoPrimaryOnStartup();
    nodeB.election.handleNoPrimaryOnStartup();

    nodeA.election.handleElectionCandidate('dev-b', {
      deviceId: 'dev-b', uptime: 30000, userDesignated: false,
    });
    nodeB.election.handleElectionCandidate('dev-a', {
      deviceId: 'dev-a', uptime: 120000, userDesignated: false,
    });

    vi.advanceTimersByTime(4000);

    // Node A is primary
    expect(nodeA.election.isPrimary()).toBe(true);
    expect(nodeB.election.isPrimary()).toBe(false);

    // 5. Set roles in device managers
    nodeA.dm.setLocalRole('primary');
    nodeB.dm.setDeviceRole('dev-a', 'primary');

    // 6. Communicate via message bus
    const messagesReceived: any[] = [];
    nodeB.bus.subscribe('app', (msg) => messagesReceived.push(msg));

    nodeA.bus.publish('dev-b', 'app', 'task:created', { taskId: 't1', title: 'Test Task' });

    expect(messagesReceived).toHaveLength(1);
    expect(messagesReceived[0].type).toBe('task:created');
    expect(messagesReceived[0].payload).toEqual({ taskId: 't1', title: 'Test Task' });

    // 7. Node B sends back
    const responsesReceived: any[] = [];
    nodeA.bus.subscribe('app', (msg) => responsesReceived.push(msg));

    nodeB.bus.publish('dev-a', 'app', 'task:ack', { taskId: 't1' });

    expect(responsesReceived).toHaveLength(1);
    expect(responsesReceived[0].type).toBe('task:ack');

    // 8. Verify final state
    expect(nodeA.dm.getDeviceById('dev-b')).toBeDefined();
    expect(nodeB.dm.getDeviceById('dev-a')).toBeDefined();
    expect(nodeA.dm.getLocalDevice().role).toBe('primary');
  });
});
