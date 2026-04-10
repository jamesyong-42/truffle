// Re-export classes (values) from the native Rust addon
export {
  NapiNode,
  NapiFileTransfer,
  NapiOfferResponder,
  NapiSyncedStore,
} from '@vibecook/truffle-native';

// Re-export interfaces/types from the native Rust addon
export type {
  NapiNodeConfig,
  NapiNodeIdentity,
  NapiPeer,
  NapiPingResult,
  NapiHealthInfo,
  NapiPeerEvent,
  NapiNamespacedMessage,
  NapiFileOffer,
  NapiTransferResult,
  NapiTransferProgress,
  NapiFileTransferEvent,
  NapiSlice,
  NapiStoreEvent,
} from '@vibecook/truffle-native';

// Sidecar binary resolution
export { resolveSidecarPath } from './sidecar.js';

// High-level API
export { createMeshNode, type CreateMeshNodeOptions } from './create-mesh-node.js';
