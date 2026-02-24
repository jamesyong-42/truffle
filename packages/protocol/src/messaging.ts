/**
 * Shared Message Bus Types and Interface
 *
 * Provides a unified interface for pub/sub messaging that can be implemented
 * by different transport layers (mesh WebSocket, local IPC, etc.)
 */

import type { EventEmitter } from 'events';

// ═══════════════════════════════════════════════════════════════════════════
// TYPES
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Message received from the bus.
 */
export interface BusMessage {
  /** Source identifier (deviceId for mesh, connectionId for local) */
  from: string | undefined;
  /** Message namespace */
  namespace: string;
  /** Message type within namespace */
  type: string;
  /** Message payload */
  payload: unknown;
}

export type BusMessageHandler = (message: BusMessage) => void;

export interface MessageBusEvents {
  subscribed: (namespace: string) => void;
  unsubscribed: (namespace: string) => void;
  error: (error: Error, namespace: string) => void;
}

// ═══════════════════════════════════════════════════════════════════════════
// INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

export interface IMessageBus extends EventEmitter {
  /**
   * Subscribe to messages in a namespace.
   * Returns unsubscribe function.
   */
  subscribe(namespace: string, handler: BusMessageHandler): () => void;

  /**
   * Send message to specific target.
   * Returns true if sent successfully.
   */
  publish(targetId: string, namespace: string, type: string, payload: unknown): boolean;

  /**
   * Broadcast message to all connected targets.
   */
  broadcast(namespace: string, type: string, payload: unknown): void;

  /**
   * Get local identifier.
   */
  readonly localId: string | undefined;

  /**
   * Check if the underlying transport is connected/running.
   */
  readonly isConnected: boolean;

  /**
   * Get list of subscribed namespaces.
   */
  getSubscribedNamespaces(): string[];

  /**
   * Dispose the message bus and clean up resources.
   */
  dispose(): void;

  // Events
  on<K extends keyof MessageBusEvents>(event: K, listener: MessageBusEvents[K]): this;
  emit<K extends keyof MessageBusEvents>(
    event: K,
    ...args: Parameters<MessageBusEvents[K]>
  ): boolean;
}
