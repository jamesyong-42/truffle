// Re-export classes (values) from the native Rust addon
export {
  NapiNode,
  NapiFileTransfer,
  NapiOfferResponder,
  NapiSyncedStore,
  NapiProxy,
  NapiTcpSocket,
  NapiTcpListener,
  NapiUdpSocket,
  NapiQuicConnection,
  NapiQuicListener,
  NapiQuicStream,
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
  NapiDatagram,
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

// QUIC over the mesh (RFC 021)
export {
  TruffleQuicStream,
  TruffleQuicConnection,
  TruffleQuicServer,
  createQuicNamespace,
  type TruffleQuic,
} from './quic.js';

// node:dgram-shaped raw UDP API over the mesh (RFC 021)
export {
  TruffleDgramSocket,
  createDgramNamespace,
  type TruffleDgram,
  type TruffleRemoteInfo,
} from './dgram.js';

// WebSocket over the mesh via the `ws` package (RFC 021)
export {
  createWsNamespace,
  type TruffleWs,
  type TruffleWsServer,
  type TruffleWsServerOptions,
  type WsLoader,
} from './ws.js';

// High-level API
export {
  createMeshNode,
  type CreateMeshNodeOptions,
  type MeshNode,
  type MeshPeerEvent,
  type MeshNamespacedMessage,
} from './create-mesh-node.js';

// RFC 022 Peer handles
export {
  Peer,
  PeerRegistry,
  isPeer,
  peerLikeToQuery,
  type PeerLike,
  type PeerRef,
  type PeerSnapshot,
} from './peer.js';
