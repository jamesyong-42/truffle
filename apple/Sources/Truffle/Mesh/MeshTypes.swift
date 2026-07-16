import Foundation

/// Lifecycle phase of a `MeshNode` (RFC 024 §6.2).
public enum MeshPhase: String, Sendable {
    case starting
    case needsLogin
    case needsMachineAuth
    case running
    case stopping
    case stopped
    case failed
}

/// Events emitted on `MeshNode.events` (RFC 024 §6.2).
public enum MeshEvent: Sendable {
    case phase(MeshPhase)
    case authRequired(URL)
    case peerUpsert(Peer)
    case peerLeft(Peer)
    case message(MeshMessage)
    /// Non-fatal conditions: bus restarts, listener recreation, lagged
    /// event streams, cancelled slow subscriptions.
    case health(String)
}

public enum MeshLogLevel: Sendable {
    case debug, info, warning, error
}

public protocol MeshLogger: Sendable {
    func log(level: MeshLogLevel, message: String)
}

/// Authentication modes (RFC 024 §6.1/§9).
public enum MeshAuth: Sendable {
    /// Interactive Tailscale login; the host presents `url`
    /// (in-app Safari recommended). Called on the main actor, deduplicated
    /// per URL until it changes or `refresh()` begins a new attempt.
    case interactive(onAuthRequired: @MainActor @Sendable (URL) async -> Void)

    /// Pre-minted or callback-minted auth key (B2C / automated).
    case authKey(String)
    case authKeyProvider(@Sendable () async throws -> String)

    /// Reuse existing state dir credentials only; `start()` throws
    /// `MeshError.needsLogin` if login would be required.
    case existingState
}

/// Configuration for `MeshNode.start` (RFC 024 §6.1).
public struct MeshConfiguration: Sendable {
    /// Required. RFC 017: `^[a-z][a-z0-9-]{1,31}$` (no trailing hyphen).
    public var appId: String

    /// Human-readable device label. The Tailscale hostname is derived as
    /// `truffle-{appId}-{slug(deviceName)}` (RFC 024 §7.2).
    public var deviceName: String

    /// Where node + Tailscale state live. Defaults to
    /// `Application Support/truffle/{appId}`; exclude from backup.
    public var stateDirectory: URL?

    public var controlURL: URL?
    /// Tailscale node state only — never rotates the device ULID (§7.1).
    public var ephemeral: Bool
    public var auth: MeshAuth

    public var logger: (any MeshLogger)?

    public init(
        appId: String,
        deviceName: String,
        stateDirectory: URL? = nil,
        controlURL: URL? = nil,
        ephemeral: Bool = false,
        auth: MeshAuth = .existingState,
        logger: (any MeshLogger)? = nil
    ) {
        self.appId = appId
        self.deviceName = deviceName
        self.stateDirectory = stateDirectory
        self.controlURL = controlURL
        self.ephemeral = ephemeral
        self.auth = auth
        self.logger = logger
    }
}
