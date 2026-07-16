import Foundation
import Observation
import Truffle

/// Chat state for the demo: one thread per peer, fed by a single
/// `onMessage("chat")` subscription on the user's node.
@MainActor
@Observable
final class ChatStore {
    struct Entry: Identifiable, Sendable {
        let id = UUID()
        let text: String
        let isMe: Bool
        let date: Date
    }

    /// Threads keyed by the peer's Tailscale stable ID (stable across
    /// hello confirmation — never key UI state on `deviceId`, it is nil
    /// pre-hello; RFC 022).
    private(set) var threads: [String: [Entry]] = [:]
    private(set) var lastError: String?

    private var subscription: MessageSubscription?

    func attach(to node: MeshNode) async {
        guard subscription == nil else { return }
        subscription = await node.onMessage(namespace: "chat") { [weak self] message in
            guard let payload = try? message.decodePayload(ChatText.self) else { return }
            let from = message.from.tailscaleId
            let date = message.timestamp ?? Date()
            await self?.append(
                Entry(text: payload.text, isMe: false, date: date), thread: from)
        }
    }

    func detach() async {
        await subscription?.cancel()
        subscription = nil
    }

    func send(_ text: String, to peer: Peer, via node: MeshNode) async {
        do {
            try await node.sendJSON(
                to: peer, namespace: "chat", payload: ChatText(text: text))
            append(Entry(text: text, isMe: true, date: Date()), thread: peer.tailscaleId)
            lastError = nil
        } catch {
            lastError = "send failed: \(error)"
        }
    }

    private func append(_ entry: Entry, thread: String) {
        threads[thread, default: []].append(entry)
    }
}
