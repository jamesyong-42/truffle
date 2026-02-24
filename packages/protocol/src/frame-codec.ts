/**
 * FrameCodec - Unified wire format for message transport
 *
 * Provides length-prefixed framing with MessagePack/JSON serialization.
 *
 * Wire Format:
 * ┌────────────────┬───────┬─────────────────────────────────┐
 * │ Length (4 bytes│ Flags │        Payload (N bytes)        │
 * │  big-endian)   │(1 byte│   (MessagePack, JSON, or raw)   │
 * └────────────────┴───────┴─────────────────────────────────┘
 *
 * Flags byte:
 *   bit 0: compressed (0 = no, 1 = yes)
 *   bit 1-2: serialization format
 *            00 = MessagePack (default)
 *            01 = JSON (debug mode)
 *            10 = reserved
 *            11 = reserved
 *   bit 3-7: reserved for future use
 */

import { encode, decode } from '@msgpack/msgpack';

// ═══════════════════════════════════════════════════════════════════════════
// CONSTANTS
// ═══════════════════════════════════════════════════════════════════════════

/** Header size: 4 bytes length + 1 byte flags */
export const HEADER_SIZE = 5;

/** Flag bits */
export const FLAG_COMPRESSED = 0x01;
export const FLAG_FORMAT_MASK = 0x06;
export const FLAG_FORMAT_MSGPACK = 0x00;
export const FLAG_FORMAT_JSON = 0x02;

/** Maximum message size (16 MB) */
export const MAX_MESSAGE_SIZE = 16 * 1024 * 1024;

// ═══════════════════════════════════════════════════════════════════════════
// TYPES
// ═══════════════════════════════════════════════════════════════════════════

export type SerializationFormat = 'msgpack' | 'json';

export interface NamespacedMessage<T = unknown> {
  namespace: string;
  type: string;
  payload?: T;
  id?: string;
  timestamp?: number;
}

export interface DecodeResult<T = unknown> {
  message: NamespacedMessage<T>;
  bytesConsumed: number;
  wasCompressed: boolean;
}

export interface FrameCodecOptions {
  compressionThreshold?: number;
  compress?: (data: Buffer) => Buffer | Promise<Buffer>;
  decompress?: (data: Buffer) => Buffer | Promise<Buffer>;
  defaultFormat?: SerializationFormat;
}

// ═══════════════════════════════════════════════════════════════════════════
// FRAME CODEC CLASS
// ═══════════════════════════════════════════════════════════════════════════

export class FrameCodec {
  private readonly compressionThreshold: number;
  private readonly compress?: (data: Buffer) => Buffer | Promise<Buffer>;
  private readonly decompress?: (data: Buffer) => Buffer | Promise<Buffer>;
  private readonly defaultFormat: SerializationFormat;

  constructor(options?: FrameCodecOptions) {
    this.compressionThreshold = options?.compressionThreshold ?? Infinity;
    this.compress = options?.compress;
    this.decompress = options?.decompress;
    this.defaultFormat = options?.defaultFormat ?? 'msgpack';
  }

  // ─────────────────────────────────────────────────────────────────────────
  // ENCODING
  // ─────────────────────────────────────────────────────────────────────────

  encode<T>(message: NamespacedMessage<T>, format?: SerializationFormat): Buffer {
    const useFormat = format ?? this.defaultFormat;
    let payload: Buffer;
    let flags: number;

    if (useFormat === 'msgpack') {
      payload = Buffer.from(encode(message));
      flags = FLAG_FORMAT_MSGPACK;
    } else {
      payload = Buffer.from(JSON.stringify(message), 'utf-8');
      flags = FLAG_FORMAT_JSON;
    }

    return this.createFrame(payload, flags);
  }

  async encodeAsync<T>(
    message: NamespacedMessage<T>,
    format?: SerializationFormat,
  ): Promise<Buffer> {
    const useFormat = format ?? this.defaultFormat;
    let payload: Buffer;
    let flags: number;

    if (useFormat === 'msgpack') {
      payload = Buffer.from(encode(message));
      flags = FLAG_FORMAT_MSGPACK;
    } else {
      payload = Buffer.from(JSON.stringify(message), 'utf-8');
      flags = FLAG_FORMAT_JSON;
    }

    if (this.compress && payload.length > this.compressionThreshold) {
      payload = await this.compress(payload);
      flags |= FLAG_COMPRESSED;
    }

    return this.createFrame(payload, flags);
  }

  private createFrame(payload: Buffer, flags: number): Buffer {
    const frame = Buffer.alloc(HEADER_SIZE + payload.length);
    frame.writeUInt32BE(payload.length, 0);
    frame.writeUInt8(flags, 4);
    payload.copy(frame, HEADER_SIZE);
    return frame;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // DECODING
  // ─────────────────────────────────────────────────────────────────────────

  decodeOne<T = unknown>(buffer: Buffer): DecodeResult<T> | null {
    if (buffer.length < HEADER_SIZE) {
      return null;
    }

    const payloadLength = buffer.readUInt32BE(0);
    const flags = buffer.readUInt8(4);

    if (payloadLength > MAX_MESSAGE_SIZE) {
      throw new Error(`Message too large: ${payloadLength} bytes (max: ${MAX_MESSAGE_SIZE})`);
    }

    const totalLength = HEADER_SIZE + payloadLength;
    if (buffer.length < totalLength) {
      return null;
    }

    const payload = buffer.subarray(HEADER_SIZE, totalLength);
    const isCompressed = (flags & FLAG_COMPRESSED) === FLAG_COMPRESSED;
    const format = flags & FLAG_FORMAT_MASK;

    if (isCompressed) {
      throw new Error('Compressed frames require decodeOneAsync()');
    }

    const message = this.deserialize<T>(payload, format);

    return {
      message,
      bytesConsumed: totalLength,
      wasCompressed: false,
    };
  }

  async decodeOneAsync<T = unknown>(buffer: Buffer): Promise<DecodeResult<T> | null> {
    if (buffer.length < HEADER_SIZE) {
      return null;
    }

    const payloadLength = buffer.readUInt32BE(0);
    const flags = buffer.readUInt8(4);

    if (payloadLength > MAX_MESSAGE_SIZE) {
      throw new Error(`Message too large: ${payloadLength} bytes (max: ${MAX_MESSAGE_SIZE})`);
    }

    const totalLength = HEADER_SIZE + payloadLength;
    if (buffer.length < totalLength) {
      return null;
    }

    let payload = buffer.subarray(HEADER_SIZE, totalLength);
    const isCompressed = (flags & FLAG_COMPRESSED) === FLAG_COMPRESSED;
    const format = flags & FLAG_FORMAT_MASK;

    if (isCompressed) {
      if (!this.decompress) {
        throw new Error('Received compressed frame but no decompress function configured');
      }
      payload = await this.decompress(payload);
    }

    const message = this.deserialize<T>(payload, format);

    return {
      message,
      bytesConsumed: totalLength,
      wasCompressed: isCompressed,
    };
  }

  decodeAll<T = unknown>(
    buffer: Buffer,
  ): {
    messages: NamespacedMessage<T>[];
    remaining: Buffer;
  } {
    const messages: NamespacedMessage<T>[] = [];
    let offset = 0;

    while (offset < buffer.length) {
      const slice = buffer.subarray(offset);
      const result = this.decodeOne<T>(slice);
      if (!result) {
        break;
      }
      messages.push(result.message);
      offset += result.bytesConsumed;
    }

    return {
      messages,
      remaining: buffer.subarray(offset),
    };
  }

  private deserialize<T>(payload: Buffer, format: number): NamespacedMessage<T> {
    let parsed: unknown;

    if (format === FLAG_FORMAT_MSGPACK) {
      parsed = decode(payload);
    } else if (format === FLAG_FORMAT_JSON) {
      parsed = JSON.parse(payload.toString('utf-8'));
    } else {
      throw new Error(`Unknown format flag: ${format}`);
    }

    if (
      typeof parsed !== 'object' ||
      parsed === null ||
      typeof (parsed as Record<string, unknown>).namespace !== 'string' ||
      typeof (parsed as Record<string, unknown>).type !== 'string'
    ) {
      throw new Error('Invalid message format: missing namespace or type');
    }

    return parsed as NamespacedMessage<T>;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // UTILITIES
  // ─────────────────────────────────────────────────────────────────────────

  isCompressionEnabled(): boolean {
    return this.compress !== undefined && this.compressionThreshold !== Infinity;
  }

  getCompressionThreshold(): number {
    return this.compressionThreshold;
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// FACTORY FUNCTIONS
// ═══════════════════════════════════════════════════════════════════════════

export function createLANCodec(): FrameCodec {
  return new FrameCodec({
    compressionThreshold: Infinity,
  });
}

export function createWANCodec(options: {
  compressionThreshold?: number;
  compress: (data: Buffer) => Buffer | Promise<Buffer>;
  decompress: (data: Buffer) => Buffer | Promise<Buffer>;
}): FrameCodec {
  return new FrameCodec({
    compressionThreshold: options.compressionThreshold ?? 1024,
    compress: options.compress,
    decompress: options.decompress,
  });
}
