// ═══════════════════════════════════════════════════════════════════════════
// @vibecook/truffle — Unified entry point for Truffle mesh networking
// ═══════════════════════════════════════════════════════════════════════════

// Mesh node (primary API)
export { MeshNode, createMeshNode } from '@vibecook/truffle-mesh';
export type { MeshNodeConfig, MeshNodeEvents, MeshTimingConfig, IncomingMeshMessage } from '@vibecook/truffle-mesh';
export { DeviceManager } from '@vibecook/truffle-mesh';
export type { DeviceManagerEvents, DeviceIdentity } from '@vibecook/truffle-mesh';
export { PrimaryElection } from '@vibecook/truffle-mesh';
export type { ElectionConfig, ElectionState, PrimaryElectionEvents } from '@vibecook/truffle-mesh';
export { MeshMessageBus } from '@vibecook/truffle-mesh';

// Store sync
export { StoreSyncAdapter } from '@vibecook/truffle-store-sync';
export type { ISyncableStore, StoreSyncAdapterConfig } from '@vibecook/truffle-store-sync';

// Types & utilities
export { createLogger, TypedEventEmitter } from '@vibecook/truffle-types';
export type {
  BaseDevice,
  DeviceRole,
  DeviceStatus,
  MeshMessage,
  MeshMessageType,
  MeshEnvelope,
  Logger,
  DeviceSlice,
  SidecarConfig,
  SidecarStatus,
  TailnetPeer,
} from '@vibecook/truffle-types';

// Protocol
export type { IMessageBus, BusMessage, BusMessageHandler } from '@vibecook/truffle-protocol';
