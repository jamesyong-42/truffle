import Foundation

/// The Truffle mesh node (RFC 024 §6.2) — Apple runtime, product core.
///
/// Generic over a `NetworkBackend` (Layer 0–1) and a `FrameTransport`
/// (session framing). Production uses `TailscaleKitBackend` + the RFC 6455
/// adapter (Phase 0 device spike); tests use `LoopbackBackend` +
/// `LengthPrefixFrameTransport`.
public actor MeshNode {
    // MARK: configuration / collaborators

    private let config: MeshConfiguration
    private let backend: any NetworkBackend
    private let transport: any FrameTransport
    private let identityPolicy: Handshake.IdentityPolicy

    private let appId: AppId
    private let deviceName: DeviceName
    private let deviceId: DeviceId

    // MARK: state

    private struct RegistryEntry {
        var tailscaleId: String
        var generation: UInt64
        var hostname: String
        var tailnetIPs: [String]
        var online: Bool
        /// Confirmed identity after a completed hello; nil for candidates.
        var identity: PeerIdentity?
        /// Created from an inbound hello that raced ahead of the netmap
        /// (RFC 024 §7.2); Layer 3 metadata merges into the same generation.
        var provisional: Bool
    }

    private struct SessionState {
        let frames: any SessionFrames
        let identity: PeerIdentity
        var pump: Task<Void, Never>?
    }

    private var phaseValue: MeshPhase = .starting
    private var entries: [String: RegistryEntry] = [:]
    private var generationCounter: UInt64 = 0
    private var sessions: [String: SessionState] = [:]
    private var subscriptions: [String: [(id: UUID, sub: MessageSubscription)]] = [:]
    private var eventSubs: [UUID: AsyncStream<MeshEvent>.Continuation] = [:]
    private var supervisorTasks: [Task<Void, Never>] = []
    private var listener: (any MeshListener)?
    private var lastAuthURL: URL?
    private var isStopped = false

    private var localTailscaleId = ""
    private var localDnsName: String?
    private var localIPs: [String] = []

    // MARK: - Lifecycle

    /// Production entry point (RFC 024 §6.2). Requires the TailscaleKit
    /// backend from the Phase 0 device spike; until it is wired up this
    /// throws. Tests and advanced callers use
    /// `start(_:backend:frameTransport:identityPolicy:)`.
    public static func start(_ config: MeshConfiguration) async throws -> MeshNode {
        throw MeshError.transport(
            "TailscaleKitBackend is not wired up yet (RFC 024 Phase 0); "
                + "use start(_:backend:frameTransport:) with an explicit backend")
    }

    /// Start a node over an explicit backend and frame transport.
    public static func start(
        _ config: MeshConfiguration,
        backend: any NetworkBackend,
        frameTransport: any FrameTransport,
        identityPolicy: Handshake.IdentityPolicy = .failClosed
    ) async throws -> MeshNode {
        let node = try MeshNode(
            config: config, backend: backend, transport: frameTransport,
            identityPolicy: identityPolicy)
        try await node.bootstrap()
        return node
    }

    private init(
        config: MeshConfiguration,
        backend: any NetworkBackend,
        transport: any FrameTransport,
        identityPolicy: Handshake.IdentityPolicy
    ) throws {
        self.config = config
        self.backend = backend
        self.transport = transport
        self.identityPolicy = identityPolicy
        self.appId = try AppId(parsing: config.appId)
        self.deviceName = DeviceName(config.deviceName)
        let stateDir =
            config.stateDirectory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("truffle", isDirectory: true)
                .appendingPathComponent(config.appId, isDirectory: true)
        self.deviceId = try DeviceId.loadOrCreate(stateDirectory: stateDir)
    }

    /// Start contract steps (RFC 024 §6.2): backend up, initial status,
    /// event pump, listener supervision. Returns once watchers are live —
    /// does NOT wait for `.running` (use `waitUntilRunning`).
    private func bootstrap() async throws {
        try await backend.start()

        let status = try await backend.status()
        apply(status: status)

        if case .existingState = config.auth {
            // §6.2: fail fast instead of returning an unusable node.
            if status.needsLogin { throw MeshError.needsLogin }
            if status.needsMachineAuth { throw MeshError.needsMachineAuth }
        }

        let backendEvents = await backend.events()
        supervisorTasks.append(
            Task { [weak self] in
                for await event in backendEvents {
                    guard let self else { return }
                    await self.handle(backendEvent: event)
                }
            })

        supervisorTasks.append(
            Task { [weak self] in
                guard let self else { return }
                await self.superviseListener()
            })
    }

    /// Idempotent shutdown (RFC 024 §6.2 step 9): stop accepting, cancel
    /// handshakes and subscriptions, close sessions, stop supervisors,
    /// then close the backend.
    public func stop() async {
        guard !isStopped else { return }
        isStopped = true
        setPhase(.stopping)

        await listener?.close()
        listener = nil

        for task in supervisorTasks { task.cancel() }
        supervisorTasks.removeAll()

        for (_, session) in sessions {
            session.pump?.cancel()
            await session.frames.close(code: SessionCloseCode.normal, reason: "node stopping")
        }
        sessions.removeAll()

        for (_, subs) in subscriptions {
            for (_, sub) in subs { await sub.cancel() }
        }
        subscriptions.removeAll()

        await backend.stop()
        setPhase(.stopped)
        for (_, continuation) in eventSubs { continuation.finish() }
        eventSubs.removeAll()
    }

    /// Re-poll backend status (e.g. after an interactive login sheet is
    /// dismissed). Clears auth-URL deduplication for a new attempt.
    public func refresh() async throws {
        guard !isStopped else { throw MeshError.stopped }
        lastAuthURL = nil
        let status = try await backend.status()
        apply(status: status)
    }

    /// Block until the node reaches `.running` (RFC 024 §6.2 step 7).
    public func waitUntilRunning(timeout: Duration) async throws {
        try await withDeadline(timeout, label: "waitUntilRunning") { [weak self] in
            while true {
                guard let self else { throw MeshError.stopped }
                if await self.phase == .running { return }
                if await self.isStoppedNow { throw MeshError.stopped }
                try await Task.sleep(for: .milliseconds(25))
            }
        }
    }

    // MARK: - Introspection

    public var phase: MeshPhase { phaseValue }
    private var isStoppedNow: Bool { isStopped }

    public var dnsName: String? { localDnsName }
    public var tailnetIPs: [String] { localIPs }

    public var localPeer: Peer {
        Peer(
            ref: PeerRef(tailscaleId: localTailscaleId, generation: 0),
            deviceId: deviceId.value,
            tailscaleId: localTailscaleId,
            generation: 0,
            displayName: deviceName.value,
            hostname: Hostname.tailscaleHostname(appId: appId, deviceName: deviceName),
            tailnetIPs: localIPs,
            online: phaseValue == .running,
            appId: appId.value,
            isLocal: true)
    }

    /// Each access mints a NEW independently-buffered stream (RFC 024 §6.2):
    /// bounded newest-value buffer of 256; the current phase is emitted
    /// immediately; no history replay.
    public var events: AsyncStream<MeshEvent> {
        let id = UUID()
        let (stream, continuation) = AsyncStream.makeStream(
            of: MeshEvent.self, bufferingPolicy: .bufferingNewest(256))
        continuation.onTermination = { [weak self] _ in
            Task { await self?.removeEventSub(id) }
        }
        eventSubs[id] = continuation
        continuation.yield(.phase(phaseValue))
        return stream
    }

    private func removeEventSub(_ id: UUID) {
        eventSubs[id] = nil
    }

    private func emit(_ event: MeshEvent) {
        for (_, continuation) in eventSubs {
            if case .dropped = continuation.yield(event) {
                // RFC 024 §6.2: one lag notice; consumers re-read level APIs.
                continuation.yield(.health("event stream lagged; resync"))
            }
        }
    }

    private func setPhase(_ phase: MeshPhase) {
        guard phaseValue != phase else { return }
        phaseValue = phase
        emit(.phase(phase))
    }

    // MARK: - Peers (RFC 024 §6.3)

    public func peers() -> [Peer] {
        entries.values.map(makePeer(from:))
    }

    /// Resolve a peer query (RFC 024 §6.3 resolution order). Not found or
    /// wait timeout → nil; ambiguity, stale refs, and shutdown throw.
    public func peer(_ query: String, waitMs: Int = 0) async throws -> Peer? {
        let deadline = ContinuousClock.now.advanced(by: .milliseconds(waitMs))
        while true {
            if isStopped { throw MeshError.stopped }
            if let found = try resolveQuery(query) { return found }
            if ContinuousClock.now >= deadline { break }
            try await Task.sleep(for: .milliseconds(50))
        }
        // Well-formed ref that resolved nothing → departed generation
        // (live refs exact-matched above). RFC 022 I5: fail loudly.
        if PeerRef.parse(query) != nil, entries[query] == nil {
            throw MeshError.peerGone(query)
        }
        return nil
    }

    private func resolveQuery(_ query: String) throws -> Peer? {
        // 1. Exact process-local PeerRef.
        if let hit = entries.values.first(where: {
            PeerRef(tailscaleId: $0.tailscaleId, generation: $0.generation).description == query
        }) {
            return makePeer(from: hit)
        }
        // 2. Exact deviceId (confirmed identities only).
        if let hit = entries.values.first(where: { $0.identity?.deviceId == query }) {
            return makePeer(from: hit)
        }
        // 3. Exact tailscaleId.
        if let hit = entries[query] {
            return makePeer(from: hit)
        }
        // 4. Exact hostname (case-sensitive) or device name
        //    (case-insensitive) — may be ambiguous.
        let nameHits = entries.values.filter { entry in
            entry.hostname == query
                || entry.identity?.deviceName.lowercased() == query.lowercased()
        }
        if nameHits.count > 1 {
            throw MeshError.peerAmbiguous(
                query: query,
                candidates: nameHits.map { $0.identity?.deviceId ?? $0.tailscaleId })
        }
        if let hit = nameHits.first {
            return makePeer(from: hit)
        }
        // 5. Unique deviceId prefix (≥4 chars).
        if query.count >= 4 {
            let prefixHits = entries.values.filter {
                $0.identity?.deviceId.hasPrefix(query) == true
            }
            if prefixHits.count > 1 {
                throw MeshError.peerAmbiguous(
                    query: query,
                    candidates: prefixHits.compactMap { $0.identity?.deviceId })
            }
            if let hit = prefixHits.first {
                return makePeer(from: hit)
            }
        }
        // 6. Tailnet IP.
        if let hit = entries.values.first(where: { $0.tailnetIPs.contains(query) }) {
            return makePeer(from: hit)
        }
        return nil
    }

    private func makePeer(from entry: RegistryEntry) -> Peer {
        Peer(
            ref: PeerRef(tailscaleId: entry.tailscaleId, generation: entry.generation),
            deviceId: entry.identity?.deviceId,
            tailscaleId: entry.tailscaleId,
            generation: entry.generation,
            displayName: entry.identity?.deviceName ?? entry.hostname,
            hostname: entry.hostname,
            tailnetIPs: entry.tailnetIPs,
            online: entry.online,
            appId: entry.identity != nil ? appId.value : nil,
            isLocal: false)
    }

    /// Generation-checked live lookup for peer-taking calls (RFC 024 §6.3).
    private func resolveLive(_ peer: Peer) throws -> RegistryEntry {
        guard !isStopped else { throw MeshError.stopped }
        guard let entry = entries[peer.tailscaleId], entry.generation == peer.generation
        else {
            throw MeshError.peerGone(peer.ref.description)
        }
        return entry
    }

    // MARK: - Messaging (RFC 024 §6.4)

    /// Convenience alias for `sendBytes`; never content-sniffs the data.
    public func send(to peer: Peer, namespace: String, data: Data) async throws {
        try await sendBytes(to: peer, namespace: namespace, data: data)
    }

    public func sendBytes(to peer: Peer, namespace: String, data: Data) async throws {
        let envelope = try Envelope.bytes(namespace: namespace, data: data)
        try await sendEnvelope(envelope, to: peer)
    }

    public func sendJSON<T: Encodable>(
        to peer: Peer, namespace: String, msgType: String = "message", payload: T
    ) async throws {
        let envelope = Envelope(
            namespace: namespace, msgType: msgType,
            payload: try JSONValue.encoding(payload),
            timestamp: Envelope.nowMillis())
        try await sendEnvelope(envelope, to: peer)
    }

    private func sendEnvelope(_ envelope: Envelope, to peer: Peer) async throws {
        let entry = try resolveLive(peer)
        let data = try EnvelopeCodec.encode(envelope)
        let session = try await session(for: entry)
        // Envelopes travel as Binary frames (RFC 024 §8.1).
        try await session.frames.send(.binary(data))
    }

    public func onMessage(
        namespace: String,
        handler: @escaping @Sendable (MeshMessage) async -> Void
    ) -> MessageSubscription {
        let id = UUID()
        let subscription = MessageSubscription(handler: handler) { [weak self] in
            Task { await self?.removeSubscription(id: id, namespace: namespace) }
        }
        subscriptions[namespace, default: []].append((id: id, sub: subscription))
        return subscription
    }

    private func removeSubscription(id: UUID, namespace: String) {
        subscriptions[namespace]?.removeAll { $0.id == id }
    }

    // MARK: - Raw transport (RFC 024 §6.6 — Phase 1b surface)

    public func dial(to peer: Peer, port: UInt16) async throws -> any MeshConnection {
        let entry = try resolveLive(peer)
        return try await backend.dial(host: dialHost(for: entry), port: port)
    }

    public func listen(port: UInt16) async throws -> any MeshListener {
        guard !isStopped else { throw MeshError.stopped }
        guard port != SessionLimits.sessionPort else {
            throw MeshError.listenFailed(
                "port \(SessionLimits.sessionPort) is reserved (the session WebSocket port)")
        }
        return try await backend.listen(port: port)
    }

    // MARK: - HTTP (RFC 024 §6.5)

    public func urlSession(
        configuration: URLSessionConfiguration = .default
    ) async throws -> URLSession {
        guard !isStopped else { throw MeshError.stopped }
        return try await backend.makeURLSession()
    }

    public func httpGet(_ url: URL) async throws -> (Data, URLResponse) {
        let session = try await urlSession()
        return try await session.data(from: url)
    }

    // MARK: - Backend event handling

    private func handle(backendEvent event: BackendEvent) async {
        guard !isStopped else { return }
        switch event {
        case .status(let status):
            apply(status: status)
        case .peerUpsert(let peer):
            upsertFromLayer3(peer)
        case .peerLeft(let tailscaleId):
            removeEntry(tailscaleId: tailscaleId)
        case .authRequired(let url):
            await handleAuthRequired(url)
        case .health(let message):
            emit(.health(message))
        }
    }

    private func apply(status: BackendStatus) {
        localTailscaleId = status.tailscaleId
        localDnsName = status.dnsName
        localIPs = status.tailnetIPs

        if status.running {
            setPhase(.running)
        } else if status.needsMachineAuth {
            setPhase(.needsMachineAuth)
        } else if status.needsLogin {
            setPhase(.needsLogin)
        }

        var seen = Set<String>()
        for peer in status.peers {
            seen.insert(peer.tailscaleId)
            upsertFromLayer3(peer)
        }
        // Entries absent from a full status snapshot have left Layer 3.
        for tailscaleId in entries.keys where !seen.contains(tailscaleId) {
            removeEntry(tailscaleId: tailscaleId)
        }
    }

    /// Candidate filtering (RFC 024 §7.2): only hostnames matching
    /// `truffle-{appId}-{slug}` enter the registry — except provisional
    /// entries created by a raced inbound hello, which merge Layer 3
    /// metadata into the same generation.
    private func upsertFromLayer3(_ peer: BackendPeer) {
        if var existing = entries[peer.tailscaleId] {
            existing.hostname = peer.hostname
            existing.tailnetIPs = peer.tailnetIPs
            existing.online = peer.online
            existing.provisional = false
            entries[peer.tailscaleId] = existing
            emit(.peerUpsert(makePeer(from: existing)))
            return
        }
        guard Hostname.isAppPeer(hostname: peer.hostname, appId: appId.value) else {
            return
        }
        generationCounter += 1
        let entry = RegistryEntry(
            tailscaleId: peer.tailscaleId,
            generation: generationCounter,
            hostname: peer.hostname,
            tailnetIPs: peer.tailnetIPs,
            online: peer.online,
            identity: nil,
            provisional: false)
        entries[peer.tailscaleId] = entry
        emit(.peerUpsert(makePeer(from: entry)))
    }

    private func removeEntry(tailscaleId: String) {
        guard let entry = entries.removeValue(forKey: tailscaleId) else { return }
        if let session = sessions.removeValue(forKey: tailscaleId) {
            session.pump?.cancel()
            Task { await session.frames.close(code: SessionCloseCode.normal, reason: "peer left") }
        }
        emit(.peerLeft(makePeer(from: entry)))
    }

    private func handleAuthRequired(_ url: URL) async {
        setPhase(.needsLogin)
        // Deduplicate the same URL until it changes or refresh() begins a
        // new attempt (RFC 024 §6.2 step 5). Emit the event first, then
        // invoke the interactive callback on the main actor.
        guard url != lastAuthURL else { return }
        lastAuthURL = url
        emit(.authRequired(url))
        if case .interactive(let onAuthRequired) = config.auth {
            await onAuthRequired(url)
        }
    }

    // MARK: - Session management (dial side)

    private func dialHost(for entry: RegistryEntry) -> String {
        entry.tailnetIPs.first ?? entry.hostname
    }

    private func session(for entry: RegistryEntry) async throws -> SessionState {
        if let existing = sessions[entry.tailscaleId] { return existing }

        let connection = try await backend.dial(
            host: dialHost(for: entry), port: SessionLimits.sessionPort)
        let frames = try await transport.clientFrames(over: connection)
        let identity: PeerIdentity
        do {
            identity = try await withDeadline(
                SessionLimits.handshakeTimeout, label: "session handshake"
            ) { [localHello] in
                try await Handshake.client(
                    frames: frames,
                    localHello: localHello,
                    expectedTailscaleId: entry.tailscaleId)
            }
        } catch {
            await frames.close(code: SessionCloseCode.helloProtocol, reason: "handshake failed")
            throw error
        }

        confirm(identity: identity, tailscaleId: entry.tailscaleId)
        return register(frames: frames, identity: identity, tailscaleId: entry.tailscaleId)
    }

    private var localHello: HelloEnvelope {
        HelloEnvelope(
            identity: PeerIdentity(
                appId: appId.value,
                deviceId: deviceId.value,
                deviceName: deviceName.value,
                os: Self.currentOS,
                tailscaleId: localTailscaleId))
    }

    static var currentOS: String {
        #if os(iOS)
            return "ios"
        #else
            return "darwin"
        #endif
    }

    /// Record a completed hello: confirm an existing candidate, or create a
    /// provisional entry when the hello raced ahead of the netmap
    /// (RFC 024 §7.2).
    private func confirm(identity: PeerIdentity, tailscaleId: String) {
        if var entry = entries[tailscaleId] {
            entry.identity = identity
            entries[tailscaleId] = entry
            emit(.peerUpsert(makePeer(from: entry)))
        } else {
            generationCounter += 1
            let entry = RegistryEntry(
                tailscaleId: tailscaleId,
                generation: generationCounter,
                hostname: "",
                tailnetIPs: [],
                online: true,
                identity: identity,
                provisional: true)
            entries[tailscaleId] = entry
            emit(.peerUpsert(makePeer(from: entry)))
        }
    }

    private func register(
        frames: any SessionFrames, identity: PeerIdentity, tailscaleId: String
    ) -> SessionState {
        var state = SessionState(frames: frames, identity: identity, pump: nil)
        let pump = Task { [weak self] in
            guard let self else { return }
            await self.pumpSession(tailscaleId: tailscaleId, frames: frames)
        }
        state.pump = pump
        sessions[tailscaleId] = state
        return state
    }

    // MARK: - Session management (accept side)

    private func superviseListener() async {
        var backoffMs = 100
        while !Task.isCancelled, !isStopped {
            do {
                let listener = try await backend.listen(port: SessionLimits.sessionPort)
                self.listener = listener
                backoffMs = 100
                await acceptLoop(listener)
            } catch {
                emit(.health("listener error: \(error); retrying"))
            }
            guard !Task.isCancelled, !isStopped else { return }
            // RFC 024 §6.2 step 8: recreate with bounded backoff; never
            // silently end inbound messaging.
            try? await Task.sleep(for: .milliseconds(backoffMs))
            backoffMs = min(backoffMs * 2, 5_000)
        }
    }

    private func acceptLoop(_ listener: any MeshListener) async {
        while !Task.isCancelled, !isStopped {
            do {
                let accepted = try await listener.accept()
                Task { [weak self] in
                    await self?.handleInbound(accepted)
                }
            } catch {
                if !isStopped {
                    emit(.health("accept error: \(error); recreating listener"))
                }
                return
            }
        }
    }

    private func handleInbound(_ accepted: MeshAcceptedConnection) async {
        do {
            let frames = try await transport.serverFrames(over: accepted.connection)
            let authenticated = try? await backend.whoIs(
                remoteEndpoint: accepted.remoteEndpoint)
            let identity = try await withDeadline(
                SessionLimits.handshakeTimeout, label: "inbound handshake"
            ) { [localHello, identityPolicy] in
                try await Handshake.server(
                    frames: frames,
                    localHello: localHello,
                    authenticated: authenticated,
                    policy: identityPolicy)
            }
            confirm(identity: identity, tailscaleId: identity.tailscaleId)
            if let previous = sessions[identity.tailscaleId] {
                previous.pump?.cancel()
                await previous.frames.close(
                    code: SessionCloseCode.normal, reason: "superseded")
            }
            _ = register(
                frames: frames, identity: identity, tailscaleId: identity.tailscaleId)
        } catch {
            emit(.health("inbound handshake rejected: \(error)"))
            await accepted.connection.close()
        }
    }

    // MARK: - Message pump

    private func pumpSession(tailscaleId: String, frames: any SessionFrames) async {
        defer { Task { [weak self] in await self?.sessionEnded(tailscaleId: tailscaleId) } }
        while !Task.isCancelled {
            let frame: SessionFrame?
            do {
                frame = try await frames.receive()
            } catch {
                emit(.health("session receive error (\(tailscaleId)): \(error)"))
                return
            }
            guard let frame else { return }
            switch frame {
            case .binary(let data):
                await deliver(data: data, from: tailscaleId)
            case .text(let text):
                await deliver(data: Data(text.utf8), from: tailscaleId)
            case .ping(let payload):
                try? await frames.send(.pong(payload))
            case .pong:
                continue
            case .close:
                return
            }
        }
    }

    private func sessionEnded(tailscaleId: String) {
        sessions[tailscaleId] = nil
    }

    private func deliver(data: Data, from tailscaleId: String) async {
        guard let entry = entries[tailscaleId] else { return }
        let envelope: Envelope
        do {
            envelope = try EnvelopeCodec.decode(data)
        } catch {
            emit(.health("dropped malformed envelope from \(tailscaleId): \(error)"))
            return
        }
        let sender = makePeer(from: entry)
        let message: MeshMessage
        do {
            // Attribution comes from the authenticated session, never from
            // the wire `from` field (RFC 024 §8.3).
            message = try MeshMessage(
                from: sender,
                namespace: envelope.namespace,
                msgType: envelope.msgType,
                payloadValue: envelope.payload,
                timestamp: envelope.timestamp.map {
                    Date(timeIntervalSince1970: Double($0) / 1000)
                })
        } catch {
            emit(.health("dropped undecodable payload from \(tailscaleId): \(error)"))
            return
        }

        for (_, sub) in subscriptions[envelope.namespace] ?? [] {
            let accepted = await sub.enqueue(message)
            if !accepted {
                emit(
                    .health(
                        "subscription on '\(envelope.namespace)' overflowed (\(MessageSubscription.maxQueue) queued); cancelled"
                    ))
            }
        }
        emit(.message(message))
    }
}
