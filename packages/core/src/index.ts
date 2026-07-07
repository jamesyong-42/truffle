// Re-export classes (values) from the native Rust addon
export {
  NapiNode,
  NapiFileTransfer,
  NapiOfferResponder,
  NapiSyncedStore,
  NapiProxy,
  NapiCrdtDoc,
  NapiTcpSocket,
  NapiTcpListener,
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
  NapiProxyConfig,
  NapiProxyInfo,
  NapiProxyEvent,
  NapiCrdtDocEvent,
} from '@vibecook/truffle-native';

// Sidecar binary resolution
export { resolveSidecarPath } from './sidecar.js';

// node:net-shaped raw TCP API over the mesh (RFC 021)
export {
  TruffleSocket,
  TruffleServer,
  createNetNamespace,
  type TruffleNet,
  type NetConnectOptions,
  type ConnectionListener,
} from './net.js';

// node:http interop over the mesh (RFC 021)
export { MeshAgent, createHttpNamespace, type TruffleHttp } from './http.js';

// High-level API
export { createMeshNode, type CreateMeshNodeOptions, type MeshNode } from './create-mesh-node.js';
