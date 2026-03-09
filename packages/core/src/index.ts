// Re-export classes (values) from the native Rust addon
export {
  NapiMeshNode,
  NapiFileTransferAdapter,
  NapiMessageBus,
  NapiStoreSyncAdapter,
} from '@vibecook/truffle-native'

// Re-export interfaces/types from the native Rust addon
export type {
  NapiMeshNodeConfig,
  NapiMeshTimingConfig,
  NapiBaseDevice,
  NapiIncomingMessage,
  NapiTailnetPeer,
  NapiMeshEvent,
  NapiFileTransferAdapterConfig,
  NapiFileTransferOffer,
  NapiAdapterTransferInfo,
  NapiBusMessage,
  NapiFileTransferEvent,
  NapiStoreSyncConfig,
  NapiOutgoingSyncMessage,
  NapiDeviceSlice,
} from '@vibecook/truffle-native'

// Sidecar binary resolution
export { resolveSidecarPath } from './sidecar.js'
