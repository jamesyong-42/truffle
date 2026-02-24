import { describe, it, expect } from 'vitest';
import {
  FrameCodec,
  createLANCodec,
  HEADER_SIZE,
  MAX_MESSAGE_SIZE,
  type NamespacedMessage,
} from '../index.js';

describe('@vibecook/truffle-protocol - FrameCodec', () => {
  describe('encode/decode roundtrip', () => {
    it('roundtrips with msgpack format', () => {
      const codec = new FrameCodec();
      const message: NamespacedMessage = {
        namespace: 'test',
        type: 'hello',
        payload: { data: 'world', count: 42 },
      };

      const frame = codec.encode(message);
      const result = codec.decodeOne(frame);

      expect(result).not.toBeNull();
      expect(result!.message.namespace).toBe('test');
      expect(result!.message.type).toBe('hello');
      expect(result!.message.payload).toEqual({ data: 'world', count: 42 });
      expect(result!.bytesConsumed).toBe(frame.length);
      expect(result!.wasCompressed).toBe(false);
    });

    it('roundtrips with JSON format', () => {
      const codec = new FrameCodec({ defaultFormat: 'json' });
      const message: NamespacedMessage = {
        namespace: 'sync',
        type: 'update',
        payload: { items: [1, 2, 3] },
      };

      const frame = codec.encode(message);
      const result = codec.decodeOne(frame);

      expect(result).not.toBeNull();
      expect(result!.message.namespace).toBe('sync');
      expect(result!.message.type).toBe('update');
      expect(result!.message.payload).toEqual({ items: [1, 2, 3] });
    });

    it('roundtrips with explicit format override', () => {
      const codec = new FrameCodec(); // default msgpack
      const message: NamespacedMessage = {
        namespace: 'test',
        type: 'json-override',
        payload: 'hello',
      };

      const frame = codec.encode(message, 'json');
      const result = codec.decodeOne(frame);

      expect(result).not.toBeNull();
      expect(result!.message.type).toBe('json-override');
    });
  });

  describe('multi-frame', () => {
    it('decodes multiple frames from single buffer', () => {
      const codec = new FrameCodec({ defaultFormat: 'json' });
      const msg1: NamespacedMessage = { namespace: 'a', type: 'one', payload: 1 };
      const msg2: NamespacedMessage = { namespace: 'b', type: 'two', payload: 2 };
      const msg3: NamespacedMessage = { namespace: 'c', type: 'three', payload: 3 };

      const combined = Buffer.concat([
        codec.encode(msg1),
        codec.encode(msg2),
        codec.encode(msg3),
      ]);

      const { messages, remaining } = codec.decodeAll(combined);

      expect(messages).toHaveLength(3);
      expect(messages[0].namespace).toBe('a');
      expect(messages[1].namespace).toBe('b');
      expect(messages[2].namespace).toBe('c');
      expect(remaining.length).toBe(0);
    });

    it('handles incomplete frame at end', () => {
      const codec = new FrameCodec({ defaultFormat: 'json' });
      const msg: NamespacedMessage = { namespace: 'test', type: 'complete' };
      const frame = codec.encode(msg);

      // Concat complete frame + partial frame (just 3 bytes)
      const partial = Buffer.concat([frame, Buffer.from([0, 0, 0])]);
      const { messages, remaining } = codec.decodeAll(partial);

      expect(messages).toHaveLength(1);
      expect(remaining.length).toBe(3);
    });
  });

  describe('error cases', () => {
    it('returns null for incomplete header', () => {
      const codec = new FrameCodec();
      const result = codec.decodeOne(Buffer.from([0, 0]));
      expect(result).toBeNull();
    });

    it('returns null for incomplete payload', () => {
      const codec = new FrameCodec();
      // Header says payload is 100 bytes, but we only provide header
      const buf = Buffer.alloc(HEADER_SIZE);
      buf.writeUInt32BE(100, 0);
      buf.writeUInt8(0, 4);

      const result = codec.decodeOne(buf);
      expect(result).toBeNull();
    });

    it('throws on message too large', () => {
      const codec = new FrameCodec();
      const buf = Buffer.alloc(HEADER_SIZE);
      buf.writeUInt32BE(MAX_MESSAGE_SIZE + 1, 0);
      buf.writeUInt8(0, 4);

      expect(() => codec.decodeOne(buf)).toThrow('Message too large');
    });

    it('throws on compressed frame with sync decode', () => {
      const codec = new FrameCodec();
      const buf = Buffer.alloc(HEADER_SIZE + 4);
      buf.writeUInt32BE(4, 0);
      buf.writeUInt8(0x01, 4); // compressed flag
      buf.write('test', HEADER_SIZE);

      expect(() => codec.decodeOne(buf)).toThrow('Compressed frames require decodeOneAsync');
    });

    it('throws on invalid format flag', () => {
      const codec = new FrameCodec();
      const jsonMsg = JSON.stringify({ namespace: 'test', type: 'x' });
      const payload = Buffer.from(jsonMsg);
      const buf = Buffer.alloc(HEADER_SIZE + payload.length);
      buf.writeUInt32BE(payload.length, 0);
      buf.writeUInt8(0x04, 4); // invalid format bits (10)
      payload.copy(buf, HEADER_SIZE);

      expect(() => codec.decodeOne(buf)).toThrow('Unknown format flag');
    });

    it('throws on invalid message format (missing namespace)', () => {
      const codec = new FrameCodec({ defaultFormat: 'json' });
      const payload = Buffer.from(JSON.stringify({ type: 'test' }));
      const buf = Buffer.alloc(HEADER_SIZE + payload.length);
      buf.writeUInt32BE(payload.length, 0);
      buf.writeUInt8(0x02, 4); // JSON format
      payload.copy(buf, HEADER_SIZE);

      expect(() => codec.decodeOne(buf)).toThrow('Invalid message format');
    });
  });

  describe('async encode/decode with compression', () => {
    it('roundtrips with async compression', async () => {
      // Simple "compression" that just reverses the buffer
      const compress = async (data: Buffer) => Buffer.from([...data].reverse());
      const decompress = async (data: Buffer) => Buffer.from([...data].reverse());

      const codec = new FrameCodec({
        compressionThreshold: 0, // Always compress
        compress,
        decompress,
        defaultFormat: 'json',
      });

      const message: NamespacedMessage = {
        namespace: 'test',
        type: 'compressed',
        payload: { big: 'data'.repeat(100) },
      };

      const frame = await codec.encodeAsync(message);
      const result = await codec.decodeOneAsync(frame);

      expect(result).not.toBeNull();
      expect(result!.message.namespace).toBe('test');
      expect(result!.message.type).toBe('compressed');
      expect(result!.wasCompressed).toBe(true);
    });

    it('skips compression below threshold', async () => {
      const compress = async (data: Buffer) => data;
      const decompress = async (data: Buffer) => data;

      const codec = new FrameCodec({
        compressionThreshold: 100000, // Very high threshold
        compress,
        decompress,
        defaultFormat: 'json',
      });

      const message: NamespacedMessage = {
        namespace: 'test',
        type: 'small',
        payload: 'x',
      };

      const frame = await codec.encodeAsync(message);
      const result = await codec.decodeOneAsync(frame);

      expect(result).not.toBeNull();
      expect(result!.wasCompressed).toBe(false);
    });
  });

  describe('factory functions', () => {
    it('createLANCodec creates codec without compression', () => {
      const codec = createLANCodec();
      expect(codec.isCompressionEnabled()).toBe(false);
      expect(codec.getCompressionThreshold()).toBe(Infinity);
    });
  });
});
