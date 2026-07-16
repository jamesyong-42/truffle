import Foundation

/// A session-layer frame. Maps 1:1 onto WebSocket opcodes in production
/// (RFC 6455); test transports carry the same shapes over an in-memory pipe.
public enum SessionFrame: Sendable, Equatable {
    case text(String)
    case binary(Data)
    case ping(Data)
    case pong(Data)
    case close(code: UInt16, reason: String)
}

/// A full-duplex ordered frame stream between two session endpoints.
public protocol SessionFrames: Sendable {
    func send(_ frame: SessionFrame) async throws
    /// Next frame, or nil on clean end-of-stream.
    func receive() async throws -> SessionFrame?
    /// Best-effort close notification + stream teardown. Idempotent.
    func close(code: UInt16, reason: String) async
}

/// Adapts a raw `MeshConnection` byte stream into session frames.
///
/// Production is the RFC 6455 adapter from the Phase 0 device spike
/// (RFC 024 §5.5): `clientFrames` performs the WebSocket client upgrade on
/// `/ws`, `serverFrames` answers it. Tests use `LengthPrefixFrameTransport`.
/// WebSocket specifics (opcodes, upgrade, limits) are asserted by transcript
/// tests against the production adapter, not here (RFC 024 §8.4).
public protocol FrameTransport: Sendable {
    func clientFrames(over connection: any MeshConnection) async throws -> any SessionFrames
    func serverFrames(over connection: any MeshConnection) async throws -> any SessionFrames
}
