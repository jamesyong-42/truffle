import Foundation
import Testing

@testable import Truffle

@Suite struct RFC6455FrameTransportTests {
    @Test func upgradesOnWSAndExchangesAllFrameKinds() async throws {
        let (clientConnection, serverConnection) = LoopbackConnection.makePair()
        let transport = RFC6455FrameTransport()

        async let pendingServer = transport.serverFrames(over: serverConnection)
        let client = try await transport.clientFrames(over: clientConnection)
        let server = try await pendingServer

        try await client.send(.text("hello"))
        #expect(try await server.receive() == .text("hello"))

        let bytes = Data([0, 1, 2, 0xff])
        try await server.send(.binary(bytes))
        #expect(try await client.receive() == .binary(bytes))

        try await client.send(.ping(Data("p".utf8)))
        #expect(try await server.receive() == .ping(Data("p".utf8)))
        try await server.send(.pong(Data("p".utf8)))
        #expect(try await client.receive() == .pong(Data("p".utf8)))

        try await client.send(.close(code: 1000, reason: "done"))
        #expect(try await server.receive() == .close(code: 1000, reason: "done"))
    }

    @Test func serverRejectsUnmaskedClientFrames() async throws {
        let (clientConnection, serverConnection) = LoopbackConnection.makePair()
        let request = [
            "GET /ws HTTP/1.1",
            "Host: truffle",
            "Upgrade: websocket",
            "Connection: Upgrade",
            "Sec-WebSocket-Key: AAECAwQFBgcICQoLDA0ODw==",
            "Sec-WebSocket-Version: 13",
            "", "",
        ].joined(separator: "\r\n")

        try await clientConnection.write(Data(request.utf8))
        let server = try await RFC6455FrameTransport().serverFrames(over: serverConnection)
        _ = try await clientConnection.read(4 * 1024)

        // FIN + binary, one-byte payload, deliberately not masked.
        try await clientConnection.write(Data([0x82, 0x01, 0x2a]))
        await #expect(throws: MeshError.self) {
            _ = try await server.receive()
        }
        await clientConnection.close()
        await serverConnection.close()
    }
}
