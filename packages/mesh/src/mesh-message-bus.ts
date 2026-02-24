/**
 * MeshMessageBus - Application-facing pub/sub layer for mesh messaging
 *
 * Layer 2 in the messaging architecture.
 * Implements IMessageBus for unified messaging.
 */

import { TypedEventEmitter } from '@vibecook/truffle-types';
import type { MeshEnvelope } from '@vibecook/truffle-types';
import type {
  BusMessage,
  BusMessageHandler,
  IMessageBus,
  MessageBusEvents,
} from '@vibecook/truffle-protocol';
import type { MeshNode, IncomingMeshMessage } from './mesh-node.js';

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

export class MeshMessageBus extends TypedEventEmitter<MessageBusEvents> implements IMessageBus {
  private meshNode: MeshNode;
  private handlers = new Map<string, Set<BusMessageHandler>>();
  private messageHandler: ((message: IncomingMeshMessage) => void) | null = null;

  constructor(meshNode: MeshNode) {
    super();
    this.meshNode = meshNode;

    this.messageHandler = this.dispatchMessage.bind(this);
    this.meshNode.on('message', this.messageHandler);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // SUBSCRIPTION
  // ─────────────────────────────────────────────────────────────────────────

  subscribe(namespace: string, handler: BusMessageHandler): () => void {
    let handlers = this.handlers.get(namespace);
    if (!handlers) {
      handlers = new Set();
      this.handlers.set(namespace, handlers);
      this.emit('subscribed', namespace);
    }
    handlers.add(handler);

    return () => {
      handlers?.delete(handler);
      if (handlers?.size === 0) {
        this.handlers.delete(namespace);
        this.emit('unsubscribed', namespace);
      }
    };
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PUBLISHING
  // ─────────────────────────────────────────────────────────────────────────

  publish(deviceId: string, namespace: string, type: string, payload: unknown): boolean {
    const envelope: MeshEnvelope = { namespace, type, payload };
    return this.meshNode.sendEnvelope(deviceId, envelope);
  }

  broadcast(namespace: string, type: string, payload: unknown): void {
    const envelope: MeshEnvelope = { namespace, type, payload };
    this.meshNode.broadcastEnvelope(envelope);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PROPERTIES
  // ─────────────────────────────────────────────────────────────────────────

  get localId(): string | undefined {
    return this.meshNode.getDeviceId();
  }

  get isConnected(): boolean {
    return this.meshNode.isRunning();
  }

  getSubscribedNamespaces(): string[] {
    return Array.from(this.handlers.keys());
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE
  // ─────────────────────────────────────────────────────────────────────────

  private dispatchMessage(message: IncomingMeshMessage): void {
    const handlers = this.handlers.get(message.namespace);
    if (!handlers || handlers.size === 0) return;

    const busMessage: BusMessage = {
      from: message.from,
      namespace: message.namespace,
      type: message.type,
      payload: message.payload,
    };

    for (const handler of handlers) {
      try {
        handler(busMessage);
      } catch (e) {
        this.emit('error', e instanceof Error ? e : new Error(String(e)), message.namespace);
      }
    }
  }

  // ─────────────────────────────────────────────────────────────────────────
  // LIFECYCLE
  // ─────────────────────────────────────────────────────────────────────────

  dispose(): void {
    if (this.messageHandler) {
      this.meshNode.off('message', this.messageHandler);
      this.messageHandler = null;
    }
    this.handlers.clear();
    this.removeAllListeners();
  }
}
