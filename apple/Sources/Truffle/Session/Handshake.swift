import Foundation

/// Run `operation` with a deadline; throws `MeshError.timeout(label)` if it
/// does not complete in time.
func withDeadline<T: Sendable>(
    _ limit: Duration,
    label: String,
    operation: @escaping @Sendable () async throws -> T
) async throws -> T {
    try await withThrowingTaskGroup(of: T.self) { group in
        group.addTask { try await operation() }
        group.addTask {
            try await Task.sleep(for: limit)
            throw MeshError.timeout(label)
        }
        guard let first = try await group.next() else {
            throw MeshError.timeout(label)
        }
        group.cancelAll()
        return first
    }
}

/// The hello exchange (RFC 017 §8 / RFC 024 §8.1), transport-agnostic over
/// `SessionFrames`. Role ordering mirrors desktop `websocket.rs`:
///
/// - client: send hello → read + validate server hello
/// - server: read + validate client hello → verify authenticated identity →
///   send own hello (never before verification — RFC 024 §14.4)
public enum Handshake {
    /// Inbound identity policy (RFC 024 §8.1.1).
    public enum IdentityPolicy: Sendable {
        /// Production: absence of a concrete stable node ID fails closed (4003).
        case failClosed
        /// Explicit opt-in for loopback/mock tests only.
        case allowUnverified
    }

    // MARK: receive

    /// Receive and parse the first hello frame, tolerating up to
    /// `SessionLimits.maxControlFramesBeforeHello` Ping/Pong frames (answering
    /// pings), accepting Text or Binary JSON. Does not apply the timeout —
    /// callers wrap with `withDeadline`.
    static func receiveHello(_ frames: any SessionFrames) async throws -> HelloEnvelope {
        var controlFrames = 0
        while true {
            guard let frame = try await frames.receive() else {
                throw MeshError.protocolViolation("peer closed connection before hello")
            }
            switch frame {
            case .text(let text):
                do {
                    return try HelloEnvelope.decoding(Data(text.utf8))
                } catch {
                    throw MeshError.protocolViolation("parse hello: \(error)")
                }
            case .binary(let data):
                do {
                    return try HelloEnvelope.decoding(data)
                } catch {
                    throw MeshError.protocolViolation("parse hello: \(error)")
                }
            case .ping(let payload):
                controlFrames += 1
                if controlFrames > SessionLimits.maxControlFramesBeforeHello {
                    throw MeshError.protocolViolation("too many control frames before hello")
                }
                try? await frames.send(.pong(payload))
            case .pong:
                controlFrames += 1
                if controlFrames > SessionLimits.maxControlFramesBeforeHello {
                    throw MeshError.protocolViolation("too many control frames before hello")
                }
            case .close(let code, let reason):
                throw MeshError.protocolViolation(
                    "peer closed connection before hello (code \(code): \(reason))")
            }
        }
    }

    // MARK: client role

    /// Dialing side: send our hello as a Text frame, then read and validate
    /// the server hello. `expectedTailscaleId` enforces the outbound identity
    /// policy (RFC 024 §8.1.1): the server hello must identify the exact
    /// Layer 3 peer that was dialed.
    public static func client(
        frames: any SessionFrames,
        localHello: HelloEnvelope,
        expectedTailscaleId: String?
    ) async throws -> PeerIdentity {
        let payload = String(decoding: try localHello.encoded(), as: UTF8.self)
        try await frames.send(.text(payload))

        let remote: HelloEnvelope
        do {
            remote = try await withDeadline(SessionLimits.helloTimeout, label: "hello read") {
                try await receiveHello(frames)
            }
        } catch {
            // Malformed / missing hello → 4002, mirroring desktop. Without
            // this close the remote side would wait out its own timeout.
            await frames.close(
                code: SessionCloseCode.helloProtocol, reason: "hello not received")
            throw error
        }

        let identity: PeerIdentity
        do {
            identity = try remote.validate(localAppId: localHello.identity.appId)
        } catch let error as HelloValidationError {
            await frames.close(code: error.closeCode, reason: "\(error)")
            throw map(error)
        }

        if let expected = expectedTailscaleId, identity.tailscaleId != expected {
            await frames.close(
                code: SessionCloseCode.identityMismatch,
                reason: "server hello tailscale_id does not match dialed peer")
            throw MeshError.identityMismatch(
                claimed: identity.tailscaleId, authenticated: expected)
        }
        return identity
    }

    // MARK: server role

    /// Accepting side: read and validate the client hello, verify the claimed
    /// `tailscale_id` against the WhoIs-authenticated identity, then — and
    /// only then — send our own hello.
    ///
    /// `authenticated` is the backend's WhoIs answer for the accepted
    /// connection (nil when the lookup itself failed). Under `.failClosed`,
    /// a missing or empty stable node ID rejects with 4003; under
    /// `.allowUnverified` (tests only) the claim is accepted unverified —
    /// mirroring, explicitly, what desktop currently does implicitly.
    public static func server(
        frames: any SessionFrames,
        localHello: HelloEnvelope,
        authenticated: AuthenticatedPeer?,
        policy: IdentityPolicy
    ) async throws -> PeerIdentity {
        let remote: HelloEnvelope
        do {
            remote = try await withDeadline(SessionLimits.helloTimeout, label: "hello read") {
                try await receiveHello(frames)
            }
        } catch {
            // Malformed / missing hello → 4002, mirroring desktop. Without
            // this close the remote side would wait out its own timeout.
            await frames.close(
                code: SessionCloseCode.helloProtocol, reason: "hello not received")
            throw error
        }

        let identity: PeerIdentity
        do {
            identity = try remote.validate(localAppId: localHello.identity.appId)
        } catch let error as HelloValidationError {
            await frames.close(code: error.closeCode, reason: "\(error)")
            throw map(error)
        }

        let authenticatedId = authenticated?.tailscaleId ?? ""
        if authenticatedId.isEmpty {
            switch policy {
            case .failClosed:
                await frames.close(
                    code: SessionCloseCode.identityMismatch, reason: "identity unavailable")
                throw MeshError.identityUnavailable(
                    "no authenticated identity for incoming connection")
            case .allowUnverified:
                break
            }
        } else if authenticatedId != identity.tailscaleId {
            await frames.close(
                code: SessionCloseCode.identityMismatch,
                reason: "claimed tailscale_id contradicts authenticated identity")
            throw MeshError.identityMismatch(
                claimed: identity.tailscaleId, authenticated: authenticatedId)
        }

        let payload = String(decoding: try localHello.encoded(), as: UTF8.self)
        try await frames.send(.text(payload))
        return identity
    }

    // MARK: helpers

    static func map(_ error: HelloValidationError) -> MeshError {
        switch error {
        case .malformed(let msg):
            return .protocolViolation("hello rejected: \(msg)")
        case .appMismatch(let local, let remote):
            return .protocolViolation("app_id mismatch: local '\(local)', remote '\(remote)'")
        }
    }
}
