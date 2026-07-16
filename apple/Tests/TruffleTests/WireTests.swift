import Foundation
import Testing

@testable import Truffle

func fixture(_ name: String) throws -> Data {
    let url = Bundle.module.url(
        forResource: name, withExtension: "json", subdirectory: "Fixtures")!
    return try Data(contentsOf: url)
}

// MARK: - Hello v2 (parity with session/hello.rs + websocket.rs::validate_hello)

@Suite struct HelloCodecTests {
    @Test func decodesRustFixture() throws {
        let hello = try HelloEnvelope.decoding(try fixture("hello_v2"))
        #expect(hello.kind == "hello")
        #expect(hello.version == 2)
        #expect(hello.identity.appId == "playground")
        #expect(hello.identity.deviceId == "01J4K9M2Z8AB3RNYQPW6H5TC0X")
        #expect(hello.identity.deviceName == "Alice's MacBook")
        #expect(hello.identity.os == "darwin")
        #expect(hello.identity.tailscaleId == "node-abc")
    }

    @Test func roundTripsWithRustJsonLayout() throws {
        let hello = HelloEnvelope(
            identity: PeerIdentity(
                appId: "chat", deviceId: "01HZZZZZZZZZZZZZZZZZZZZZZZ",
                deviceName: "laptop", os: "linux", tailscaleId: "tsnode-1"))
        let json = try JSONSerialization.jsonObject(with: hello.encoded()) as! [String: Any]
        #expect(json["kind"] as? String == "hello")
        #expect(json["version"] as? Int == 2)
        let identity = json["identity"] as! [String: Any]
        #expect(identity["app_id"] as? String == "chat")
        #expect(identity["device_id"] as? String == "01HZZZZZZZZZZZZZZZZZZZZZZZ")
        #expect(identity["device_name"] as? String == "laptop")
        #expect(identity["os"] as? String == "linux")
        #expect(identity["tailscale_id"] as? String == "tsnode-1")
    }

    @Test func missingTailscaleIdDefaultsToEmpty() throws {
        let raw = #"{"kind":"hello","version":2,"identity":{"app_id":"a-b","device_id":"x","device_name":"y","os":"darwin"}}"#
        let hello = try HelloEnvelope.decoding(Data(raw.utf8))
        #expect(hello.identity.tailscaleId == "")
    }

    private func makeHello(
        appId: String = "chat", deviceId: String = "01HZZZZZZZZZZZZZZZZZZZZZZZ",
        deviceName: String = "laptop", os: String = "linux", tailscaleId: String = "tsnode-1",
        kind: String = "hello", version: UInt32 = 2
    ) -> HelloEnvelope {
        var hello = HelloEnvelope(
            identity: PeerIdentity(
                appId: appId, deviceId: deviceId, deviceName: deviceName, os: os,
                tailscaleId: tailscaleId))
        hello.kind = kind
        hello.version = version
        return hello
    }

    @Test func validatesGoodHello() throws {
        let identity = try makeHello().validate(localAppId: "chat")
        #expect(identity.deviceId == "01HZZZZZZZZZZZZZZZZZZZZZZZ")
    }

    @Test func rejectsUnknownKind() {
        #expect(throws: HelloValidationError.malformed("unexpected hello kind: not_hello")) {
            try makeHello(kind: "not_hello").validate(localAppId: "chat")
        }
    }

    @Test func rejectsVersion1() {
        #expect(throws: HelloValidationError.self) {
            try makeHello(version: 1).validate(localAppId: "chat")
        }
    }

    @Test func rejectsOversizedFieldsBeforeAppIdCheck() {
        // Oversized device_name on a MISMATCHED app_id must classify as
        // malformed (4002), not app-mismatch (4001) — bounds run first.
        let oversized = makeHello(
            appId: "other", deviceName: String(repeating: "x", count: 513))
        do {
            _ = try oversized.validate(localAppId: "chat")
            Issue.record("expected throw")
        } catch let error as HelloValidationError {
            #expect(error.closeCode == SessionCloseCode.helloProtocol)
        } catch {
            Issue.record("unexpected error type: \(error)")
        }
    }

    @Test func rejectsDeviceIdEqualToTailscaleId() {
        #expect(throws: HelloValidationError.self) {
            try makeHello(deviceId: "tsnode-1", tailscaleId: "tsnode-1")
                .validate(localAppId: "chat")
        }
        #expect(throws: HelloValidationError.self) {
            try makeHello(deviceId: "").validate(localAppId: "chat")
        }
    }

    @Test func classifiesAppMismatchAs4001() {
        do {
            _ = try makeHello(appId: "other").validate(localAppId: "chat")
            Issue.record("expected throw")
        } catch let error as HelloValidationError {
            #expect(error == .appMismatch(local: "chat", remote: "other"))
            #expect(error.closeCode == SessionCloseCode.appMismatch)
        } catch {
            Issue.record("unexpected error type: \(error)")
        }
    }

    @Test func fieldBoundsMatchDesktopMaximums() throws {
        // Exactly-at-limit passes; one over fails. app_id uses the shared
        // 32-byte bound.
        let atLimit = makeHello(tailscaleId: String(repeating: "t", count: 256))
        _ = try atLimit.validate(localAppId: "chat")
        let over = makeHello(tailscaleId: String(repeating: "t", count: 257))
        #expect(throws: HelloValidationError.self) { try over.validate(localAppId: "chat") }
    }
}

// MARK: - Envelope (parity with envelope/mod.rs + RFC 024 §8.3)

@Suite struct EnvelopeCodecTests {
    @Test func decodesMessageFixture() throws {
        let env = try EnvelopeCodec.decode(try fixture("envelope_message"))
        #expect(env.namespace == "chat")
        #expect(env.msgType == "message")
        #expect(env.timestamp == 1_752_675_000_000)
        #expect(env.from == nil)
        #expect(env.payload["text"]?.stringValue == "héllo wörld — 你好")
        #expect(env.payload["count"] == .int(42))
        #expect(env.payload["big"] == .int(1_752_675_000_000))
        #expect(env.payload["pi"] == .double(3.5))
        #expect(env.payload["ok"] == .bool(true))
        #expect(env.payload["nothing"] == JSONValue.null)
        #expect(env.payload["nested"]?["deep"] == .array([.int(1), .int(2), .int(3)]))
    }

    @Test func ignoresUnknownFields() throws {
        // Forward compatibility: `v`, `from_device_id`, and future fields are
        // silently ignored — matches Rust serde behavior.
        let env = try EnvelopeCodec.decode(try fixture("envelope_unknown_fields"))
        #expect(env.namespace == "chat")
        #expect(env.payload["text"]?.stringValue == "forward compat")
    }

    @Test func omitsNilOptionalsOnEncode() throws {
        let env = Envelope(namespace: "chat", msgType: "message", payload: .object([:]))
        let json = try JSONSerialization.jsonObject(with: EnvelopeCodec.encode(env)) as! [String: Any]
        #expect(json["timestamp"] == nil)
        #expect(json["from"] == nil)
        #expect(json["msg_type"] as? String == "message")
    }

    @Test func roundTripsStructurally() throws {
        let env = Envelope(
            namespace: "chat", msgType: "message",
            payload: .object(["a": .array([.int(1), .string("x"), .bool(false), .null])]),
            timestamp: 123)
        let decoded = try EnvelopeCodec.decode(try EnvelopeCodec.encode(env))
        #expect(decoded == env)
    }

    @Test func rejectsOversizedEnvelopeBeforeDecoding() {
        let big = Data(repeating: 0x20, count: 64)
        #expect(throws: MeshError.payloadTooLarge(actual: 64, limit: 32)) {
            try EnvelopeCodec.decode(big, maxBytes: 32)
        }
    }

    @Test func enforcesLocalEmissionBounds() {
        let emptyNs = Envelope(namespace: "", msgType: "message", payload: .null)
        #expect(throws: MeshError.self) { try EnvelopeCodec.encode(emptyNs) }

        let hugeNs = Envelope(
            namespace: String(repeating: "n", count: 1025), msgType: "message", payload: .null)
        #expect(throws: MeshError.self) { try EnvelopeCodec.encode(hugeNs) }

        let emptyType = Envelope(namespace: "chat", msgType: "", payload: .null)
        #expect(throws: MeshError.self) { try EnvelopeCodec.encode(emptyType) }
    }
}

// MARK: - Opaque bytes wrapper (parity with node.rs::send_bytes)

@Suite struct BytesPayloadTests {
    @Test func decodesRustBytesFixture() throws {
        let env = try EnvelopeCodec.decode(try fixture("envelope_bytes"))
        let data = try env.decodedBytes()
        #expect(data == Data("hello truffle".utf8))
    }

    @Test func wrapsWithDesktopSchema() throws {
        let env = try Envelope.bytes(namespace: "ft", data: Data("hello".utf8))
        #expect(env.msgType == "bytes")
        #expect(env.payload["encoding"]?.stringValue == "base64")
        #expect(env.payload["data"]?.stringValue == "aGVsbG8=")
        #expect(try env.decodedBytes() == Data("hello".utf8))
    }

    @Test func nonBytesMsgTypeReturnsNil() throws {
        let env = Envelope(namespace: "chat", msgType: "message", payload: .object([:]))
        #expect(try env.decodedBytes() == nil)
    }

    @Test func rejectsMalformedBase64() throws {
        let env = Envelope(
            namespace: "ft", msgType: "bytes",
            payload: .object(["encoding": .string("base64"), "data": .string("!!not-base64!!")]))
        #expect(throws: MeshError.self) { try env.decodedBytes() }
    }

    @Test func rejectsMissingWrapper() throws {
        let env = Envelope(namespace: "ft", msgType: "bytes", payload: .object([:]))
        #expect(throws: MeshError.self) { try env.decodedBytes() }
    }

    @Test func capsRawInputSize() {
        #expect(throws: MeshError.payloadTooLarge(actual: 8, limit: 4)) {
            try Envelope.bytes(namespace: "ft", data: Data(repeating: 0, count: 8), limit: 4)
        }
    }

    @Test func capsDecodedSize() throws {
        let env = try Envelope.bytes(namespace: "ft", data: Data(repeating: 7, count: 64))
        #expect(throws: MeshError.self) { try env.decodedBytes(limit: 32) }
    }
}
