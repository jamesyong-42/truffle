import CryptoKit
import Foundation
import Security

/// The production Truffle session adapter. It adopts an already-connected
/// mesh byte stream and performs the narrow RFC 6455 subset used by the
/// desktop runtime: HTTP upgrade on `/ws`, text/binary messages, ping/pong,
/// close, masking, fragmentation, and the 16 MiB message bound.
public struct RFC6455FrameTransport: FrameTransport {
    public init() {}

    public func clientFrames(over connection: any MeshConnection) async throws -> any SessionFrames {
        let key = Self.clientKey()
        let request = [
            "GET /ws HTTP/1.1",
            "Host: truffle",
            "Upgrade: websocket",
            "Connection: Upgrade",
            "Sec-WebSocket-Key: \(key)",
            "Sec-WebSocket-Version: 13",
            "", "",
        ].joined(separator: "\r\n")
        try await connection.write(Data(request.utf8))
        let response = try await HTTPUpgrade.read(from: connection)
        guard response.startLine.hasPrefix("HTTP/1.1 101 ")
                || response.startLine == "HTTP/1.1 101"
        else {
            throw MeshError.transport("WebSocket upgrade rejected: \(response.startLine)")
        }
        try Self.validateCommonHeaders(response.headers)
        let expected = Self.accept(for: key)
        guard response.headers["sec-websocket-accept"] == expected else {
            throw MeshError.protocolViolation("invalid Sec-WebSocket-Accept")
        }
        return RFC6455Frames(
            connection: connection, role: .client, initialBuffer: response.remainder)
    }

    public func serverFrames(over connection: any MeshConnection) async throws -> any SessionFrames {
        let request = try await HTTPUpgrade.read(from: connection)
        let requestParts = request.startLine.split(separator: " ")
        guard requestParts.count == 3,
            requestParts[0] == "GET",
            requestParts[1] == "/ws",
            requestParts[2] == "HTTP/1.1"
        else {
            throw MeshError.protocolViolation("invalid WebSocket request target")
        }
        try Self.validateCommonHeaders(request.headers)
        guard request.headers["sec-websocket-version"] == "13",
            let key = request.headers["sec-websocket-key"],
            Data(base64Encoded: key)?.count == 16
        else {
            throw MeshError.protocolViolation("invalid WebSocket version or key")
        }
        let response = [
            "HTTP/1.1 101 Switching Protocols",
            "Upgrade: websocket",
            "Connection: Upgrade",
            "Sec-WebSocket-Accept: \(Self.accept(for: key))",
            "", "",
        ].joined(separator: "\r\n")
        try await connection.write(Data(response.utf8))
        return RFC6455Frames(
            connection: connection, role: .server, initialBuffer: request.remainder)
    }

    private static func validateCommonHeaders(_ headers: [String: String]) throws {
        guard headers["upgrade"]?.lowercased() == "websocket" else {
            throw MeshError.protocolViolation("missing WebSocket Upgrade header")
        }
        let connectionTokens = headers["connection"]?
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces).lowercased() } ?? []
        guard connectionTokens.contains("upgrade") else {
            throw MeshError.protocolViolation("missing Connection: Upgrade")
        }
    }

    private static func clientKey() -> String {
        var bytes = [UInt8](repeating: 0, count: 16)
        if SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) != errSecSuccess {
            bytes = Array(UUID().uuidString.utf8.prefix(16))
        }
        return Data(bytes).base64EncodedString()
    }

    private static func accept(for key: String) -> String {
        let magic = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
        let digest = Insecure.SHA1.hash(data: Data((key + magic).utf8))
        return Data(digest).base64EncodedString()
    }
}

private enum WebSocketRole: Sendable {
    case client
    case server

    var sendsMasked: Bool { self == .client }
    var expectsMasked: Bool { self == .server }
}

private struct HTTPUpgrade {
    static let maxHeaderBytes = 16 * 1024

    let startLine: String
    let headers: [String: String]
    let remainder: Data

    static func read(from connection: any MeshConnection) async throws -> HTTPUpgrade {
        let marker = Data("\r\n\r\n".utf8)
        var buffer = Data()
        while buffer.range(of: marker) == nil {
            guard buffer.count < maxHeaderBytes else {
                throw MeshError.protocolViolation("WebSocket HTTP headers too large")
            }
            guard let chunk = try await connection.read(4 * 1024), !chunk.isEmpty else {
                throw MeshError.transport("stream ended during WebSocket upgrade")
            }
            buffer.append(chunk)
        }
        guard let range = buffer.range(of: marker) else {
            throw MeshError.protocolViolation("incomplete WebSocket upgrade")
        }
        let headerData = buffer[..<range.lowerBound]
        guard let headerText = String(data: headerData, encoding: .utf8) else {
            throw MeshError.protocolViolation("non-UTF-8 WebSocket HTTP headers")
        }
        let lines = headerText.components(separatedBy: "\r\n")
        guard let startLine = lines.first, !startLine.isEmpty else {
            throw MeshError.protocolViolation("empty WebSocket HTTP request")
        }
        var headers: [String: String] = [:]
        for line in lines.dropFirst() {
            guard let colon = line.firstIndex(of: ":") else {
                throw MeshError.protocolViolation("malformed WebSocket HTTP header")
            }
            let name = line[..<colon].trimmingCharacters(in: .whitespaces).lowercased()
            let value = line[line.index(after: colon)...]
                .trimmingCharacters(in: .whitespaces)
            guard !name.isEmpty else {
                throw MeshError.protocolViolation("empty WebSocket HTTP header name")
            }
            if let previous = headers[name] {
                headers[name] = previous + "," + value
            } else {
                headers[name] = value
            }
        }
        return HTTPUpgrade(
            startLine: startLine,
            headers: headers,
            remainder: Data(buffer[range.upperBound...]))
    }
}

private actor RFC6455Frames: SessionFrames {
    private enum Opcode: UInt8 {
        case continuation = 0x0
        case text = 0x1
        case binary = 0x2
        case close = 0x8
        case ping = 0x9
        case pong = 0xA

        var isControl: Bool { rawValue >= 0x8 }
    }

    private struct WireFrame {
        let final: Bool
        let opcode: Opcode
        let payload: Data
    }

    private let connection: any MeshConnection
    private let role: WebSocketRole
    private var buffer: Data
    private var closed = false
    private var fragmentedOpcode: Opcode?
    private var fragmentedPayload = Data()

    init(connection: any MeshConnection, role: WebSocketRole, initialBuffer: Data) {
        self.connection = connection
        self.role = role
        self.buffer = initialBuffer
    }

    func send(_ frame: SessionFrame) async throws {
        guard !closed else { throw MeshError.stopped }
        let (opcode, payload) = Self.encode(frame)
        if opcode.isControl, payload.count > 125 {
            throw MeshError.protocolViolation("WebSocket control payload exceeds 125 bytes")
        }
        guard payload.count <= SessionLimits.maxMessageSize else {
            throw MeshError.payloadTooLarge(
                actual: payload.count, limit: SessionLimits.maxMessageSize)
        }
        try await connection.write(Self.wire(opcode, payload: payload, masked: role.sendsMasked))
        if opcode == .close {
            closed = true
            await connection.close()
        }
    }

    func receive() async throws -> SessionFrame? {
        while !closed {
            guard let wire = try await readWireFrame() else { return nil }
            if wire.opcode.isControl {
                guard wire.final, wire.payload.count <= 125 else {
                    throw MeshError.protocolViolation("fragmented or oversized control frame")
                }
                switch wire.opcode {
                case .ping: return .ping(wire.payload)
                case .pong: return .pong(wire.payload)
                case .close:
                    closed = true
                    let close = try Self.decodeClose(wire.payload)
                    return close
                default:
                    throw MeshError.protocolViolation("invalid control opcode")
                }
            }

            switch wire.opcode {
            case .text, .binary:
                guard fragmentedOpcode == nil else {
                    throw MeshError.protocolViolation("new message during fragmented message")
                }
                if wire.final {
                    return try Self.message(opcode: wire.opcode, payload: wire.payload)
                }
                fragmentedOpcode = wire.opcode
                fragmentedPayload = wire.payload
            case .continuation:
                guard let opcode = fragmentedOpcode else {
                    throw MeshError.protocolViolation("unexpected continuation frame")
                }
                guard fragmentedPayload.count <= SessionLimits.maxMessageSize - wire.payload.count else {
                    throw MeshError.payloadTooLarge(
                        actual: fragmentedPayload.count + wire.payload.count,
                        limit: SessionLimits.maxMessageSize)
                }
                fragmentedPayload.append(wire.payload)
                if wire.final {
                    let payload = fragmentedPayload
                    fragmentedOpcode = nil
                    fragmentedPayload.removeAll(keepingCapacity: false)
                    return try Self.message(opcode: opcode, payload: payload)
                }
            default:
                throw MeshError.protocolViolation("unknown WebSocket data opcode")
            }
        }
        return nil
    }

    func close(code: UInt16, reason: String) async {
        guard !closed else { return }
        closed = true
        let clipped = Data(reason.utf8).prefix(123)
        var payload = Data([UInt8(code >> 8), UInt8(code & 0xff)])
        payload.append(clipped)
        try? await connection.write(Self.wire(.close, payload: payload, masked: role.sendsMasked))
        await connection.close()
    }

    private func readWireFrame() async throws -> WireFrame? {
        guard let head = try await readExactly(2) else { return nil }
        let byte0 = head[head.startIndex]
        let byte1 = head[head.startIndex + 1]
        guard byte0 & 0x70 == 0 else {
            throw MeshError.protocolViolation("WebSocket RSV bits are unsupported")
        }
        guard let opcode = Opcode(rawValue: byte0 & 0x0f) else {
            throw MeshError.protocolViolation("unknown WebSocket opcode")
        }
        let masked = byte1 & 0x80 != 0
        guard masked == role.expectsMasked else {
            throw MeshError.protocolViolation("incorrect WebSocket masking for role")
        }
        let shortLength = Int(byte1 & 0x7f)
        let length: Int
        switch shortLength {
        case 126:
            guard let extended = try await readExactly(2) else {
                throw MeshError.transport("stream ended in WebSocket length")
            }
            length = Int(Self.readUInt(extended))
        case 127:
            guard let extended = try await readExactly(8) else {
                throw MeshError.transport("stream ended in WebSocket length")
            }
            let value = Self.readUInt(extended)
            guard value <= UInt64(Int.max) else {
                throw MeshError.payloadTooLarge(actual: Int.max, limit: SessionLimits.maxMessageSize)
            }
            length = Int(value)
        default:
            length = shortLength
        }
        guard length <= SessionLimits.maxMessageSize else {
            throw MeshError.payloadTooLarge(actual: length, limit: SessionLimits.maxMessageSize)
        }
        let mask: Data
        if masked {
            guard let bytes = try await readExactly(4) else {
                throw MeshError.transport("stream ended in WebSocket mask")
            }
            mask = bytes
        } else {
            mask = Data()
        }
        let payload: Data
        if length == 0 {
            payload = Data()
        } else {
            guard var bytes = try await readExactly(length) else {
                throw MeshError.transport("stream ended in WebSocket payload")
            }
            if masked {
                for index in bytes.indices {
                    bytes[index] ^= mask[(index - bytes.startIndex) % 4]
                }
            }
            payload = bytes
        }
        return WireFrame(final: byte0 & 0x80 != 0, opcode: opcode, payload: payload)
    }

    private func readExactly(_ count: Int) async throws -> Data? {
        while buffer.count < count {
            guard let chunk = try await connection.read(min(64 * 1024, count - buffer.count)),
                !chunk.isEmpty
            else {
                if buffer.isEmpty { return nil }
                throw MeshError.transport("stream ended mid-WebSocket frame")
            }
            buffer.append(chunk)
        }
        let result = Data(buffer.prefix(count))
        buffer.removeFirst(count)
        return result
    }

    private static func message(opcode: Opcode, payload: Data) throws -> SessionFrame {
        if opcode == .binary { return .binary(payload) }
        guard opcode == .text, let text = String(data: payload, encoding: .utf8) else {
            throw MeshError.protocolViolation("invalid UTF-8 WebSocket text message")
        }
        return .text(text)
    }

    private static func decodeClose(_ payload: Data) throws -> SessionFrame {
        if payload.isEmpty { return .close(code: SessionCloseCode.normal, reason: "") }
        guard payload.count >= 2 else {
            throw MeshError.protocolViolation("one-byte WebSocket close payload")
        }
        let code = UInt16(payload[payload.startIndex]) << 8
            | UInt16(payload[payload.startIndex + 1])
        let reasonData = payload.dropFirst(2)
        guard let reason = String(data: reasonData, encoding: .utf8) else {
            throw MeshError.protocolViolation("invalid UTF-8 WebSocket close reason")
        }
        return .close(code: code, reason: reason)
    }

    private static func encode(_ frame: SessionFrame) -> (Opcode, Data) {
        switch frame {
        case .text(let text): return (.text, Data(text.utf8))
        case .binary(let data): return (.binary, data)
        case .ping(let data): return (.ping, data)
        case .pong(let data): return (.pong, data)
        case .close(let code, let reason):
            var payload = Data([UInt8(code >> 8), UInt8(code & 0xff)])
            payload.append(Data(reason.utf8).prefix(123))
            return (.close, payload)
        }
    }

    private static func wire(_ opcode: Opcode, payload: Data, masked: Bool) -> Data {
        var output = Data([0x80 | opcode.rawValue])
        let maskBit: UInt8 = masked ? 0x80 : 0
        if payload.count < 126 {
            output.append(maskBit | UInt8(payload.count))
        } else if payload.count <= Int(UInt16.max) {
            output.append(maskBit | 126)
            output.append(contentsOf: [UInt8(payload.count >> 8), UInt8(payload.count & 0xff)])
        } else {
            output.append(maskBit | 127)
            let length = UInt64(payload.count)
            for shift in stride(from: 56, through: 0, by: -8) {
                output.append(UInt8((length >> UInt64(shift)) & 0xff))
            }
        }
        if masked {
            var mask = [UInt8](repeating: 0, count: 4)
            if SecRandomCopyBytes(kSecRandomDefault, mask.count, &mask) != errSecSuccess {
                mask = [0x12, 0x34, 0x56, 0x78]
            }
            output.append(contentsOf: mask)
            for (index, byte) in payload.enumerated() {
                output.append(byte ^ mask[index % 4])
            }
        } else {
            output.append(payload)
        }
        return output
    }

    private static func readUInt(_ bytes: Data) -> UInt64 {
        bytes.reduce(0) { ($0 << 8) | UInt64($1) }
    }
}
