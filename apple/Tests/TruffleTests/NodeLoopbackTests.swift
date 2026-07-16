import Foundation
import Testing

@testable import Truffle

/// End-to-end tests: two `MeshNode`s over a shared `LoopbackNetwork`
/// (RFC 024 §13 — mock-provider integration level).
@Suite struct NodeLoopbackTests {
    struct ChatPayload: Codable, Equatable {
        var text: String
    }

    private func tempDir() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("truffle-node-test-\(UUID().uuidString)")
    }

    /// Start a node named `deviceName` for `appId` on `network`, advertising
    /// the RFC 017 hostname derived from its own identity — unless
    /// `advertisedHostname` overrides it (lying-hostname tests).
    private func startNode(
        network: LoopbackNetwork,
        tailscaleId: String,
        appId: String,
        deviceName: String,
        advertisedHostname: String? = nil,
        identityPolicy: Handshake.IdentityPolicy = .failClosed
    ) async throws -> (MeshNode, URL) {
        let derived = Hostname.tailscaleHostname(
            appId: try AppId(parsing: appId), deviceName: DeviceName(deviceName))
        let hostname = advertisedHostname ?? derived
        let backend = await network.join(tailscaleId: tailscaleId, hostname: hostname)
        let dir = tempDir()
        let node = try await MeshNode.start(
            MeshConfiguration(
                appId: appId, deviceName: deviceName, stateDirectory: dir,
                auth: .existingState),
            backend: backend,
            frameTransport: LengthPrefixFrameTransport(),
            identityPolicy: identityPolicy)
        return (node, dir)
    }

    @Test func exchangesJSONBothDirectionsWithAttribution() async throws {
        let network = LoopbackNetwork()
        let (alice, dirA) = try await startNode(
            network: network, tailscaleId: "ts-a", appId: "demo", deviceName: "Alice")
        let (bob, dirB) = try await startNode(
            network: network, tailscaleId: "ts-b", appId: "demo", deviceName: "Bob")
        defer {
            try? FileManager.default.removeItem(at: dirA)
            try? FileManager.default.removeItem(at: dirB)
        }

        try await alice.waitUntilRunning(timeout: .seconds(2))
        try await bob.waitUntilRunning(timeout: .seconds(2))

        let bobInbox = Mailbox<MeshMessage>()
        let aliceInbox = Mailbox<MeshMessage>()
        let subBob = await bob.onMessage(namespace: "chat") { message in
            await bobInbox.put(message)
        }
        let subAlice = await alice.onMessage(namespace: "chat") { message in
            await aliceInbox.put(message)
        }

        // Alice discovers Bob as a pre-hello candidate: deviceId nil.
        guard let bobPeer = try await alice.peer("ts-b", waitMs: 2_000) else {
            Issue.record("alice never discovered bob")
            return
        }
        #expect(bobPeer.deviceId == nil)

        // Alice → Bob (dial direction 1).
        try await alice.sendJSON(
            to: bobPeer, namespace: "chat", payload: ChatPayload(text: "hi bob"))
        guard let atBob = await bobInbox.take() else {
            Issue.record("bob received nothing")
            return
        }
        #expect(try atBob.decodePayload(ChatPayload.self) == ChatPayload(text: "hi bob"))
        #expect(atBob.namespace == "chat")
        #expect(atBob.msgType == "message")
        // Attribution from the authenticated session, incl. Alice's ULID.
        #expect(atBob.from.tailscaleId == "ts-a")
        #expect(atBob.from.deviceId != nil)
        #expect(atBob.timestamp != nil)

        // Bob → Alice (reuses the established session; direction 2).
        guard let alicePeer = try await bob.peer("ts-a", waitMs: 2_000) else {
            Issue.record("bob never discovered alice")
            return
        }
        try await bob.sendJSON(
            to: alicePeer, namespace: "chat", payload: ChatPayload(text: "hi alice"))
        guard let atAlice = await aliceInbox.take() else {
            Issue.record("alice received nothing")
            return
        }
        #expect(try atAlice.decodePayload(ChatPayload.self) == ChatPayload(text: "hi alice"))
        #expect(atAlice.from.tailscaleId == "ts-b")

        // After hello, Bob's snapshot for Alice carries her real ULID.
        let confirmed = try await alice.peer("ts-b")
        #expect(confirmed?.deviceId != nil)
        #expect(confirmed?.appId == "demo")

        await subBob.cancel()
        await subAlice.cancel()
        await alice.stop()
        await bob.stop()
    }

    @Test func exchangesOpaqueBytes() async throws {
        let network = LoopbackNetwork()
        let (alice, dirA) = try await startNode(
            network: network, tailscaleId: "ts-a", appId: "demo", deviceName: "Alice")
        let (bob, dirB) = try await startNode(
            network: network, tailscaleId: "ts-b", appId: "demo", deviceName: "Bob")
        defer {
            try? FileManager.default.removeItem(at: dirA)
            try? FileManager.default.removeItem(at: dirB)
        }

        let inbox = Mailbox<MeshMessage>()
        let sub = await bob.onMessage(namespace: "ft") { await inbox.put($0) }

        guard let bobPeer = try await alice.peer("ts-b", waitMs: 2_000) else {
            Issue.record("no peer")
            return
        }
        let blob = Data((0..<1024).map { UInt8($0 % 251) })
        try await alice.send(to: bobPeer, namespace: "ft", data: blob)

        guard let received = await inbox.take() else {
            Issue.record("no message")
            return
        }
        #expect(received.msgType == "bytes")
        #expect(try received.payloadBytes() == blob)

        await sub.cancel()
        await alice.stop()
        await bob.stop()
    }

    @Test func appIdMismatchNeverConfirms() async throws {
        let network = LoopbackNetwork()
        let (alice, dirA) = try await startNode(
            network: network, tailscaleId: "ts-a", appId: "app-one", deviceName: "Alice")
        // Mallory advertises app-one's hostname prefix but actually runs
        // app-two: a candidate that must never survive the hello.
        let (mallory, dirB) = try await startNode(
            network: network, tailscaleId: "ts-m", appId: "app-two", deviceName: "Mallory",
            advertisedHostname: "truffle-app-one-mallory")
        defer {
            try? FileManager.default.removeItem(at: dirA)
            try? FileManager.default.removeItem(at: dirB)
        }

        guard let candidate = try await alice.peer("ts-m", waitMs: 2_000) else {
            Issue.record("candidate not visible")
            return
        }
        #expect(candidate.deviceId == nil)

        // The hello closes with 4001; the send fails; Mallory never confirms.
        await #expect(throws: MeshError.self) {
            try await alice.sendJSON(
                to: candidate, namespace: "chat", payload: ChatPayload(text: "?"))
        }
        let after = try await alice.peer("ts-m")
        #expect(after?.deviceId == nil)

        await alice.stop()
        await mallory.stop()
    }

    @Test func staleRefThrowsPeerGoneEvenAfterRejoin() async throws {
        let network = LoopbackNetwork()
        let (alice, dirA) = try await startNode(
            network: network, tailscaleId: "ts-a", appId: "demo", deviceName: "Alice")
        let (bob, dirB) = try await startNode(
            network: network, tailscaleId: "ts-b", appId: "demo", deviceName: "Bob")
        defer {
            try? FileManager.default.removeItem(at: dirA)
            try? FileManager.default.removeItem(at: dirB)
        }

        guard let staleBob = try await alice.peer("ts-b", waitMs: 2_000) else {
            Issue.record("no peer")
            return
        }

        // Bob leaves...
        await bob.stop()
        while try await alice.peer("ts-b") != nil {
            try await Task.sleep(for: .milliseconds(20))
        }

        // ...and rejoins with the SAME tailscale id (new generation).
        let backendB2 = await network.join(
            tailscaleId: "ts-b",
            hostname: Hostname.tailscaleHostname(
                appId: try AppId(parsing: "demo"), deviceName: DeviceName("Bob")))
        let bob2 = try await MeshNode.start(
            MeshConfiguration(
                appId: "demo", deviceName: "Bob", stateDirectory: dirB,
                auth: .existingState),
            backend: backendB2,
            frameTransport: LengthPrefixFrameTransport())
        guard let freshBob = try await alice.peer("ts-b", waitMs: 2_000) else {
            Issue.record("bob did not rejoin")
            return
        }
        #expect(freshBob.generation != staleBob.generation)

        // The stale snapshot fails loudly — never routes to the new node.
        await #expect(throws: MeshError.peerGone(staleBob.ref.description)) {
            try await alice.sendJSON(
                to: staleBob, namespace: "chat", payload: ChatPayload(text: "stale"))
        }
        // A stale ref *query* also classifies as peerGone, not not-found.
        await #expect(throws: MeshError.peerGone(staleBob.ref.description)) {
            _ = try await alice.peer(staleBob.ref.description)
        }

        await alice.stop()
        await bob2.stop()
    }

    @Test func resolvesQueriesAndDetectsAmbiguity() async throws {
        let network = LoopbackNetwork()
        let (alice, dirA) = try await startNode(
            network: network, tailscaleId: "ts-a", appId: "demo", deviceName: "Alice")
        let (bob, dirB) = try await startNode(
            network: network, tailscaleId: "ts-b", appId: "demo", deviceName: "Laptop")
        let (carol, dirC) = try await startNode(
            network: network, tailscaleId: "ts-c", appId: "demo", deviceName: "Laptop")
        defer {
            try? FileManager.default.removeItem(at: dirA)
            try? FileManager.default.removeItem(at: dirB)
            try? FileManager.default.removeItem(at: dirC)
        }

        guard let bobPeer = try await alice.peer("ts-b", waitMs: 2_000) else {
            Issue.record("no peer")
            return
        }
        // Confirm both via hello so device names/ULIDs are known.
        try await alice.sendJSON(to: bobPeer, namespace: "x", payload: ChatPayload(text: "."))
        guard let carolPeer = try await alice.peer("ts-c", waitMs: 2_000) else {
            Issue.record("no peer")
            return
        }
        try await alice.sendJSON(to: carolPeer, namespace: "x", payload: ChatPayload(text: "."))

        // Exact tailscaleId / ref / IP / deviceId / prefix.
        let byRef = try await alice.peer(bobPeer.ref.description)
        #expect(byRef == bobPeer)
        guard let confirmedBob = try await alice.peer("ts-b"),
            let bobUlid = confirmedBob.deviceId
        else {
            Issue.record("bob not confirmed")
            return
        }
        let byUlid = try await alice.peer(bobUlid)
        #expect(byUlid?.tailscaleId == "ts-b")
        // ULIDs minted in the same millisecond share their 10-char timestamp
        // prefix, so use a prefix deep into the 80 random bits.
        let byPrefix = try await alice.peer(String(bobUlid.prefix(20)))
        #expect(byPrefix?.tailscaleId == "ts-b")
        let byIP = try await alice.peer(confirmedBob.tailnetIPs[0])
        #expect(byIP?.tailscaleId == "ts-b")

        // Shared display name "Laptop" → ambiguous (throws, never guesses).
        await #expect(throws: MeshError.self) {
            _ = try await alice.peer("laptop")
        }

        // Unknown → nil (never throws not-found from peer()).
        let missing = try await alice.peer("no-such-peer")
        #expect(missing == nil)

        await alice.stop()
        await bob.stop()
        await carol.stop()
    }

    @Test func failClosedIdentityRejectsWhenWhoIsUnavailable() async throws {
        let network = LoopbackNetwork()
        let (alice, dirA) = try await startNode(
            network: network, tailscaleId: "ts-a", appId: "demo", deviceName: "Alice")
        let (bob, dirB) = try await startNode(
            network: network, tailscaleId: "ts-b", appId: "demo", deviceName: "Bob",
            identityPolicy: .failClosed)
        defer {
            try? FileManager.default.removeItem(at: dirA)
            try? FileManager.default.removeItem(at: dirB)
        }

        guard let bobPeer = try await alice.peer("ts-b", waitMs: 2_000) else {
            Issue.record("no peer")
            return
        }

        // WhoIs stops answering: Bob (server, fail-closed) must reject with
        // 4003 and Alice's send must fail. Nobody confirms.
        await network.setWithholdWhoIs(true)
        await #expect(throws: MeshError.self) {
            try await alice.sendJSON(
                to: bobPeer, namespace: "chat", payload: ChatPayload(text: "hi"))
        }
        let unconfirmed = try await alice.peer("ts-b")
        #expect(unconfirmed?.deviceId == nil)

        // WhoIs recovers → the same peers can now confirm.
        await network.setWithholdWhoIs(false)
        try await alice.sendJSON(
            to: bobPeer, namespace: "chat", payload: ChatPayload(text: "hi again"))
        let confirmed = try await alice.peer("ts-b")
        #expect(confirmed?.deviceId != nil)

        await alice.stop()
        await bob.stop()
    }

    @Test func eventsStreamEmitsPhaseImmediately() async throws {
        let network = LoopbackNetwork()
        let (alice, dirA) = try await startNode(
            network: network, tailscaleId: "ts-a", appId: "demo", deviceName: "Alice")
        defer { try? FileManager.default.removeItem(at: dirA) }

        var iterator = await alice.events.makeAsyncIterator()
        let first = await iterator.next()
        guard case .phase(let phase) = first else {
            Issue.record("expected immediate phase event, got \(String(describing: first))")
            return
        }
        #expect(phase == .running)

        #expect(await alice.localPeer.isLocal)
        #expect(await alice.localPeer.deviceId != nil)
        #expect(await alice.dnsName != nil)

        await alice.stop()
    }
}
