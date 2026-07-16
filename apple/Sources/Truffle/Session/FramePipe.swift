import Foundation

/// An in-memory, ordered, bidirectional `SessionFrames` pair for tests and
/// the loopback backend. Not a wire transport: production sessions run over
/// the RFC 6455 adapter (RFC 024 §5.5).
public struct FramePipe: SessionFrames {
    private let incoming: Mailbox<SessionFrame>
    private let outgoing: Mailbox<SessionFrame>

    private init(incoming: Mailbox<SessionFrame>, outgoing: Mailbox<SessionFrame>) {
        self.incoming = incoming
        self.outgoing = outgoing
    }

    /// Create a connected pair: frames sent on one endpoint arrive on the
    /// other, in order.
    public static func makePair() -> (FramePipe, FramePipe) {
        let aToB = Mailbox<SessionFrame>()
        let bToA = Mailbox<SessionFrame>()
        return (
            FramePipe(incoming: bToA, outgoing: aToB),
            FramePipe(incoming: aToB, outgoing: bToA)
        )
    }

    public func send(_ frame: SessionFrame) async throws {
        guard await outgoing.put(frame) else { throw MeshError.stopped }
        if case .close = frame {
            await outgoing.finish()
        }
    }

    public func receive() async throws -> SessionFrame? {
        await incoming.take()
    }

    public func close(code: UInt16, reason: String) async {
        await outgoing.put(.close(code: code, reason: reason))
        await outgoing.finish()
    }
}
