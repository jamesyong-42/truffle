import SwiftUI
import Truffle
import TruffleSwiftUI

struct ContentView: View {
    let world: DemoWorld
    @State private var chat = ChatStore()

    var body: some View {
        NavigationStack {
            Group {
                switch world.model.phase {
                case .stopped, .failed:
                    ConnectView(world: world)
                case .starting, .needsLogin, .needsMachineAuth, .stopping:
                    ProgressView("Connecting…")
                case .running:
                    PeerListView(world: world, chat: chat)
                }
            }
            .navigationTitle("MeshChat")
        }
        .task(id: world.model.phase == .running) {
            if world.model.phase == .running, let node = world.model.node {
                await chat.attach(to: node)
                await autoChatIfRequested(node: node)
            }
        }
        .task {
            // `--autostart` skips the connect screen (demos, screenshots,
            // headless smoke tests).
            if CommandLine.arguments.contains("--autostart"),
                world.model.phase == .stopped
            {
                await world.start()
            }
        }
    }

    /// `--autochat`: greet every peer once — drives the hello handshakes so
    /// peers show confirmed ULIDs without UI interaction. Waits briefly for
    /// discovery since the bots join after the local node is running.
    private func autoChatIfRequested(node: MeshNode) async {
        guard CommandLine.arguments.contains("--autochat") else { return }
        var peers: [Peer] = []
        for _ in 0..<30 {
            peers = await node.peers()
            if peers.count >= DemoWorld.botCount { break }
            try? await Task.sleep(for: .milliseconds(100))
        }
        for peer in peers {
            await chat.send("hello from \(DemoWorld.appId)", to: peer, via: node)
        }
    }
}

struct ConnectView: View {
    let world: DemoWorld

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 56))
                .foregroundStyle(.tint)
            Text("Truffle mesh, in-process demo")
                .font(.headline)
            Text(
                """
                Starts your node plus two bot peers on an in-memory mesh. \
                Everything above the network layer is the real Truffle \
                stack — peers, hello handshake, namespaced messages. The \
                Tailscale backend arrives with RFC 024 Phase 0.
                """
            )
            .font(.footnote)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.center)
            .padding(.horizontal)

            Button {
                Task { await world.start() }
            } label: {
                Label("Start demo mesh", systemImage: "play.fill")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)

            Button {} label: {
                Label("Join a tailnet (Phase 0 pending)", systemImage: "lock")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .disabled(true)

            if let error = world.model.lastError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
        .padding()
    }
}

struct PeerListView: View {
    let world: DemoWorld
    let chat: ChatStore

    var body: some View {
        List {
            Section {
                ForEach(world.model.peers) { peer in
                    NavigationLink(value: peer) {
                        PeerRow(peer: peer)
                    }
                }
            } header: {
                Text("Peers on '\(DemoWorld.appId)'")
            } footer: {
                Text(
                    "A peer shows its device ULID only after the hello "
                        + "handshake confirms it (RFC 022 honest fields).")
            }

            Section("Me") {
                if let node = world.model.node {
                    LocalPeerRow(node: node)
                }
            }
        }
        .navigationDestination(for: Peer.self) { peer in
            ChatView(peer: peer, world: world, chat: chat)
        }
        .toolbar {
            ToolbarItem(placement: .destructiveAction) {
                Button("Stop") {
                    Task { await world.stop() }
                }
            }
        }
    }
}

struct PeerRow: View {
    let peer: Peer

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Circle()
                    .fill(peer.online ? .green : .gray)
                    .frame(width: 8, height: 8)
                Text(peer.displayName)
                    .font(.body)
            }
            Text(peer.deviceId.map { "ULID \($0.prefix(10))…" } ?? "not yet confirmed")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }
}

struct LocalPeerRow: View {
    let node: MeshNode
    @State private var local: Peer?

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(local?.displayName ?? "…")
            Text(local.map { "\($0.hostname) · ULID \($0.deviceId?.prefix(10) ?? "")…" } ?? "")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .task {
            local = await node.localPeer
        }
    }
}
