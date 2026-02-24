export { FrameCodec, createLANCodec, createWANCodec } from './frame-codec.js';
export type {
  SerializationFormat,
  NamespacedMessage,
  DecodeResult,
  FrameCodecOptions,
} from './frame-codec.js';
export {
  HEADER_SIZE,
  FLAG_COMPRESSED,
  FLAG_FORMAT_MASK,
  FLAG_FORMAT_MSGPACK,
  FLAG_FORMAT_JSON,
  MAX_MESSAGE_SIZE,
} from './frame-codec.js';

export type {
  BusMessage,
  BusMessageHandler,
  IMessageBus,
  MessageBusEvents,
} from './messaging.js';
