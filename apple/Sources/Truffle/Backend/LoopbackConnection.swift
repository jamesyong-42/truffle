import Foundation

/// An in-memory, ordered byte stream implementing `MeshConnection` for tests
/// and the loopback backend. Queued chunks are always delivered before EOF.
public actor LoopbackConnection: MeshConnection {
    private let incoming: Mailbox<Data>
    private let outgoing: Mailbox<Data>
    private var residual = Data()
    private var readEOF = false
    private var writeClosed = false

    private init(incoming: Mailbox<Data>, outgoing: Mailbox<Data>) {
        self.incoming = incoming
        self.outgoing = outgoing
    }

    /// Create a connected pair: bytes written on one side are read (in
    /// order, arbitrarily re-chunked) on the other.
    public static func makePair() -> (LoopbackConnection, LoopbackConnection) {
        let aToB = Mailbox<Data>()
        let bToA = Mailbox<Data>()
        return (
            LoopbackConnection(incoming: bToA, outgoing: aToB),
            LoopbackConnection(incoming: aToB, outgoing: bToA)
        )
    }

    public func read(_ max: Int) async throws -> Data? {
        guard max > 0 else { return Data() }
        if residual.isEmpty {
            if readEOF { return nil }
            guard let chunk = await incoming.take() else {
                readEOF = true
                return nil
            }
            residual = chunk
        }
        let out = residual.prefix(max)
        residual.removeFirst(out.count)
        return Data(out)
    }

    public func write(_ data: Data) async throws {
        guard !writeClosed else {
            throw MeshError.transport("write on closed connection")
        }
        guard !data.isEmpty else { return }
        guard await outgoing.put(data) else {
            throw MeshError.transport("write on closed connection")
        }
    }

    public func close() async {
        guard !writeClosed else { return }
        writeClosed = true
        await outgoing.finish()
    }
}
