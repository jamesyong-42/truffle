import Foundation
import Truffle
import TruffleSwiftUI

/// The message payload for the demo's `"chat"` namespace — same shape a
/// Node/TS peer would send (RFC 024 §6.4 `sendJSON`).
struct ChatText: Codable, Sendable {
    var text: String
}

/// An in-process mesh: your node plus two bots on a `LoopbackNetwork`.
///
/// Everything above Layer 1 is the REAL Truffle stack — hello v2, envelopes,
/// generation-checked peers, heartbeats — only Layer 0 (Tailscale) is
/// simulated in memory. When the TailscaleKit backend lands (RFC 024
/// Phase 0), swapping `LoopbackBackend` for `TailscaleKitBackend` is the
/// only change.
@MainActor
final class DemoWorld {
    static let appId = "meshchat-demo"
    static let botCount = 2

    let network = LoopbackNetwork()
    let model: MeshModel
    private var bots: [DemoBot] = []

    init(deviceName: String) {
        let network = self.network
        model = MeshModel(starter: {
            try await Self.startNode(
                on: network, tailscaleId: "ts-me", deviceName: deviceName)
        })
    }

    func start() async {
        await model.start()
        guard bots.isEmpty else { return }
        do {
            bots.append(
                try await DemoBot.launch(
                    on: network, tailscaleId: "ts-echo", deviceName: "Echo Bot"
                ) { text in
                    "echo: \(text)"
                })
            bots.append(
                try await DemoBot.launch(
                    on: network, tailscaleId: "ts-shout", deviceName: "Shout Bot"
                ) { text in
                    text.uppercased() + "!!!"
                })
        } catch {
            // Bots are demo garnish; the user's node stays usable.
            print("demo bot failed to start: \(error)")
        }
    }

    func stop() async {
        for bot in bots {
            await bot.shutdown()
        }
        bots.removeAll()
        await model.stop()
    }

    static func startNode(
        on network: LoopbackNetwork, tailscaleId: String, deviceName: String
    ) async throws -> MeshNode {
        let hostname = Hostname.tailscaleHostname(
            appId: try AppId(parsing: appId), deviceName: DeviceName(deviceName))
        let backend = await network.join(tailscaleId: tailscaleId, hostname: hostname)
        let stateDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("meshchat-demo-\(tailscaleId)")
        return try await MeshNode.start(
            MeshConfiguration(
                appId: appId, deviceName: deviceName, stateDirectory: stateDir,
                auth: .existingState),
            backend: backend,
            frameTransport: LengthPrefixFrameTransport())
    }
}

/// A peer node that answers every `"chat"` message through `reply`.
actor DemoBot {
    private let node: MeshNode
    private var subscription: MessageSubscription?

    private init(node: MeshNode) {
        self.node = node
    }

    static func launch(
        on network: LoopbackNetwork,
        tailscaleId: String,
        deviceName: String,
        reply: @escaping @Sendable (String) -> String
    ) async throws -> DemoBot {
        let node = try await DemoWorld.startNode(
            on: network, tailscaleId: tailscaleId, deviceName: deviceName)
        let bot = DemoBot(node: node)
        let subscription = await node.onMessage(namespace: "chat") { message in
            guard let incoming = try? message.decodePayload(ChatText.self) else { return }
            try? await Task.sleep(for: .milliseconds(300))  // feel like a peer, not a mirror
            try? await node.sendJSON(
                to: message.from, namespace: "chat",
                payload: ChatText(text: reply(incoming.text)))
        }
        await bot.retain(subscription)
        return bot
    }

    private func retain(_ subscription: MessageSubscription) {
        self.subscription = subscription
    }

    func shutdown() async {
        await subscription?.cancel()
        await node.stop()
    }
}
