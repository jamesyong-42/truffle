import Foundation

/// Trivial frame transport for tests and the loopback backend:
/// `opcode(1) | length(4, BE) | payload`. Close frames carry
/// `code(2, BE) | reason(UTF-8)` as payload.
///
/// **Not a wire protocol.** Real sessions (Swift↔Swift included) run RFC 6455
/// on `/ws` port 9417 (RFC 024 §6.4/§8.1); this exists so the session and
/// mesh layers can be exercised over in-memory byte pipes before the Phase 0
/// WebSocket adapter lands. WebSocket-specific behavior is asserted by
/// transcript tests against that adapter, not against this transport.
public struct LengthPrefixFrameTransport: FrameTransport {
    public init() {}

    public func clientFrames(over connection: any MeshConnection) async throws -> any SessionFrames {
        LengthPrefixFrames(connection: connection)
    }

    public func serverFrames(over connection: any MeshConnection) async throws -> any SessionFrames {
        LengthPrefixFrames(connection: connection)
    }
}

actor LengthPrefixFrames: SessionFrames {
    private enum Opcode: UInt8 {
        case text = 1
        case binary = 2
        case close = 8
        case ping = 9
        case pong = 10
    }

    private let connection: any MeshConnection
    private var buffer = Data()
    private var closed = false

    init(connection: any MeshConnection) {
        self.connection = connection
    }

    func send(_ frame: SessionFrame) async throws {
        guard !closed else { throw MeshError.stopped }
        let (opcode, payload) = Self.encodePayload(frame)
        var out = Data()
        out.append(opcode.rawValue)
        out.append(contentsOf: Self.be32(UInt32(payload.count)))
        out.append(payload)
        try await connection.write(out)
        if case .close = frame {
            closed = true
            await connection.close()
        }
    }

    func receive() async throws -> SessionFrame? {
        guard let header = try await readExactly(5) else { return nil }
        let opcodeByte = header[header.startIndex]
        let length = Int(Self.readBE32(header.dropFirst()))
        guard length <= SessionLimits.maxMessageSize else {
            throw MeshError.payloadTooLarge(actual: length, limit: SessionLimits.maxMessageSize)
        }
        let payload: Data
        if length == 0 {
            payload = Data()
        } else {
            guard let body = try await readExactly(length) else {
                throw MeshError.transport("stream ended mid-frame")
            }
            payload = body
        }
        guard let opcode = Opcode(rawValue: opcodeByte) else {
            throw MeshError.protocolViolation("unknown frame opcode \(opcodeByte)")
        }
        switch opcode {
        case .text:
            return .text(String(decoding: payload, as: UTF8.self))
        case .binary:
            return .binary(payload)
        case .ping:
            return .ping(payload)
        case .pong:
            return .pong(payload)
        case .close:
            let code =
                payload.count >= 2
                ? UInt16(payload[payload.startIndex]) << 8
                    | UInt16(payload[payload.startIndex + 1])
                : SessionCloseCode.normal
            let reason = payload.count > 2 ? String(decoding: payload.dropFirst(2), as: UTF8.self) : ""
            return .close(code: code, reason: reason)
        }
    }

    func close(code: UInt16, reason: String) async {
        guard !closed else { return }
        closed = true
        var payload = Data(Self.be16(code))
        payload.append(contentsOf: Data(reason.utf8))
        var out = Data()
        out.append(Opcode.close.rawValue)
        out.append(contentsOf: Self.be32(UInt32(payload.count)))
        out.append(payload)
        try? await connection.write(out)
        await connection.close()
    }

    // MARK: helpers

    private func readExactly(_ n: Int) async throws -> Data? {
        while buffer.count < n {
            guard let chunk = try await connection.read(64 * 1024), !chunk.isEmpty else {
                if buffer.isEmpty { return nil }
                throw MeshError.transport("stream ended mid-frame")
            }
            buffer.append(chunk)
        }
        let out = buffer.prefix(n)
        buffer.removeFirst(n)
        return Data(out)
    }

    private static func encodePayload(_ frame: SessionFrame) -> (Opcode, Data) {
        switch frame {
        case .text(let s): return (.text, Data(s.utf8))
        case .binary(let d): return (.binary, d)
        case .ping(let d): return (.ping, d)
        case .pong(let d): return (.pong, d)
        case .close(let code, let reason):
            var payload = Data(be16(code))
            payload.append(contentsOf: Data(reason.utf8))
            return (.close, payload)
        }
    }

    private static func be16(_ v: UInt16) -> [UInt8] {
        [UInt8(v >> 8), UInt8(v & 0xFF)]
    }

    private static func be32(_ v: UInt32) -> [UInt8] {
        [UInt8(v >> 24 & 0xFF), UInt8(v >> 16 & 0xFF), UInt8(v >> 8 & 0xFF), UInt8(v & 0xFF)]
    }

    private static func readBE32(_ data: Data) -> UInt32 {
        var v: UInt32 = 0
        for byte in data.prefix(4) {
            v = (v << 8) | UInt32(byte)
        }
        return v
    }
}
