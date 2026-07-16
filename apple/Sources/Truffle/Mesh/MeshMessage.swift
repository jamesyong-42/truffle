import Foundation

/// An application message received from a peer (RFC 024 §6.4).
public struct MeshMessage: Sendable {
    /// Snapshot of the sender, attributed from the connection's
    /// hello-authenticated identity — never from the wire `from` field.
    public let from: Peer
    public let namespace: String
    public let msgType: String
    /// Raw JSON bytes of the envelope's `payload` value (not the whole
    /// envelope).
    public let payloadJSON: Data
    public let timestamp: Date?

    /// The parsed payload (kept so byte decoding is structural, not
    /// re-parsed).
    let payloadValue: JSONValue

    init(
        from: Peer, namespace: String, msgType: String, payloadValue: JSONValue,
        timestamp: Date?
    ) throws {
        self.from = from
        self.namespace = namespace
        self.msgType = msgType
        self.payloadValue = payloadValue
        self.payloadJSON = try payloadValue.encodedData()
        self.timestamp = timestamp
    }

    /// Decode the JSON payload into a `Decodable` type.
    public func decodePayload<T: Decodable>(_ type: T.Type) throws -> T {
        do {
            return try JSONDecoder().decode(type, from: payloadJSON)
        } catch {
            throw MeshError.invalidPayload("payload decode failed: \(error)")
        }
    }

    /// Decodes the normative `msg_type == "bytes"` base64 wrapper
    /// (RFC 024 §6.4); returns nil for any other msg_type.
    public func payloadBytes() throws -> Data? {
        let envelope = Envelope(
            namespace: namespace, msgType: msgType, payload: payloadValue)
        return try envelope.decodedBytes()
    }
}

/// Handle for an `onMessage` registration (RFC 024 §6.4).
///
/// Invokes its handler serially, in receive order, with a bounded queue of
/// 256 messages. Overflow cancels the subscription (the node emits a
/// `.health` event). Delivery is at-most-once; no replay.
public actor MessageSubscription {
    static let maxQueue = 256

    private let handler: @Sendable (MeshMessage) async -> Void
    private var queue: [MeshMessage] = []
    private var draining = false
    private var cancelled = false
    /// Node-side cleanup; safe to call from deinit (spawns no actor hops
    /// synchronously).
    private let onCancel: @Sendable () -> Void

    init(
        handler: @escaping @Sendable (MeshMessage) async -> Void,
        onCancel: @escaping @Sendable () -> Void
    ) {
        self.handler = handler
        self.onCancel = onCancel
    }

    /// Idempotent. Also called automatically when the subscription is
    /// released.
    public func cancel() {
        guard !cancelled else { return }
        cancelled = true
        queue.removeAll()
        onCancel()
    }

    deinit {
        onCancel()
    }

    /// Returns false when the bounded queue overflowed (the subscription
    /// self-cancels); the node then emits a health event.
    func enqueue(_ message: MeshMessage) -> Bool {
        guard !cancelled else { return true }
        guard queue.count < Self.maxQueue else {
            cancelled = true
            queue.removeAll()
            onCancel()
            return false
        }
        queue.append(message)
        if !draining {
            draining = true
            Task { await self.drain() }
        }
        return true
    }

    var isCancelled: Bool { cancelled }

    private func drain() async {
        while !cancelled, !queue.isEmpty {
            let message = queue.removeFirst()
            await handler(message)
        }
        draining = false
    }
}
