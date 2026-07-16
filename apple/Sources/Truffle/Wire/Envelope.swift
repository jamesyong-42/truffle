import Foundation

/// A namespace-routed message (Layer 6 — RFC 012), mirroring
/// `truffle-core::envelope::Envelope` field-for-field (RFC 024 §8.3):
///
/// - There is **no** `v` field and no `from_device_id` — protocol versioning
///   lives in the hello.
/// - `from` on the wire is untrusted; the receiving side attributes messages
///   from the connection's authenticated identity and ignores this field.
/// - Unknown fields are silently ignored on decode (forward compatibility).
public struct Envelope: Codable, Sendable, Equatable {
    /// Namespace owned by the application (e.g. `"ft"`, `"chat"`).
    public var namespace: String
    /// Application-defined message type within the namespace.
    public var msgType: String
    /// Opaque payload. Truffle never inspects it (except the `"bytes"` helper).
    public var payload: JSONValue
    /// Millisecond Unix timestamp, set by the sender.
    public var timestamp: UInt64?
    /// Sender peer ID — populated by the RECEIVING side, never trusted from
    /// the wire.
    public var from: String?

    enum CodingKeys: String, CodingKey {
        case namespace
        case msgType = "msg_type"
        case payload
        case timestamp
        case from
    }

    public init(
        namespace: String,
        msgType: String,
        payload: JSONValue,
        timestamp: UInt64? = nil,
        from: String? = nil
    ) {
        self.namespace = namespace
        self.msgType = msgType
        self.payload = payload
        self.timestamp = timestamp
        self.from = from
    }

    /// Millisecond Unix timestamp for "now" (matches Rust `with_timestamp`).
    public static func nowMillis() -> UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(namespace, forKey: .namespace)
        try c.encode(msgType, forKey: .msgType)
        try c.encode(payload, forKey: .payload)
        // skip_serializing_if = "Option::is_none" parity:
        try c.encodeIfPresent(timestamp, forKey: .timestamp)
        try c.encodeIfPresent(from, forKey: .from)
    }
}

// MARK: - Codec with bounds (RFC 024 §8.3)

public enum EnvelopeCodec {
    /// Encode an envelope, enforcing the local emission rules: namespace and
    /// msg_type non-empty and ≤ 1,024 UTF-8 bytes each, and the encoded
    /// envelope within `maxBytes` — the 15 MiB bound applies in BOTH
    /// directions (RFC 024 §8.3), not just on receive.
    public static func encode(
        _ envelope: Envelope, maxBytes: Int = SessionLimits.maxEnvelopeBytes
    ) throws -> Data {
        guard !envelope.namespace.isEmpty,
            envelope.namespace.utf8.count <= SessionLimits.maxNamespaceBytes
        else {
            throw MeshError.invalidPayload(
                "namespace must be non-empty and at most \(SessionLimits.maxNamespaceBytes) UTF-8 bytes"
            )
        }
        guard !envelope.msgType.isEmpty,
            envelope.msgType.utf8.count <= SessionLimits.maxMsgTypeBytes
        else {
            throw MeshError.invalidPayload(
                "msg_type must be non-empty and at most \(SessionLimits.maxMsgTypeBytes) UTF-8 bytes"
            )
        }
        let data = try JSONEncoder().encode(envelope)
        guard data.count <= maxBytes else {
            throw MeshError.payloadTooLarge(actual: data.count, limit: maxBytes)
        }
        return data
    }

    /// Decode an envelope, rejecting encoded data larger than `maxBytes`
    /// BEFORE JSON payload decoding (RFC 024 §8.3).
    public static func decode(
        _ data: Data, maxBytes: Int = SessionLimits.maxEnvelopeBytes
    ) throws -> Envelope {
        guard data.count <= maxBytes else {
            throw MeshError.payloadTooLarge(actual: data.count, limit: maxBytes)
        }
        do {
            return try JSONDecoder().decode(Envelope.self, from: data)
        } catch {
            throw MeshError.invalidPayload("envelope decode failed: \(error)")
        }
    }
}

// MARK: - Opaque bytes ("bytes" msg_type — RFC 024 §6.4 / §8.3)

extension Envelope {
    /// The msg_type used for opaque byte payloads (desktop `send_bytes`).
    public static let bytesMsgType = "bytes"

    /// Wrap raw bytes in the shipped explicit-byte representation:
    /// `{"encoding": "base64", "data": "…"}` with `msg_type: "bytes"`.
    /// Rejects input above `limit` (base64 expansion headroom — RFC 024 §6.4).
    public static func bytes(
        namespace: String,
        data: Data,
        limit: Int = SessionLimits.maxRawBytesPayload
    ) throws -> Envelope {
        guard data.count <= limit else {
            throw MeshError.payloadTooLarge(actual: data.count, limit: limit)
        }
        return Envelope(
            namespace: namespace,
            msgType: Self.bytesMsgType,
            payload: .object([
                "encoding": .string("base64"),
                "data": .string(data.base64EncodedString()),
            ]),
            timestamp: nowMillis()
        )
    }

    /// Decode the normative `msg_type == "bytes"` base64 wrapper; returns
    /// nil for any other msg_type. Throws on malformed wrappers, malformed
    /// base64, or a decoded payload above `limit`.
    public func decodedBytes(
        limit: Int = SessionLimits.maxRawBytesPayload
    ) throws -> Data? {
        guard msgType == Self.bytesMsgType else { return nil }
        guard self.payload["encoding"]?.stringValue == "base64",
            let encoded = self.payload["data"]?.stringValue
        else {
            throw MeshError.invalidPayload("bytes payload missing base64 wrapper")
        }
        guard let data = Data(base64Encoded: encoded) else {
            throw MeshError.invalidPayload("bytes payload contains malformed base64")
        }
        guard data.count <= limit else {
            throw MeshError.payloadTooLarge(actual: data.count, limit: limit)
        }
        return data
    }
}
