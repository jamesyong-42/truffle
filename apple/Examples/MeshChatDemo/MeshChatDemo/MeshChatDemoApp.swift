import SwiftUI

/// MeshChat — the RFC 024 example app.
///
/// Runs an in-process demo mesh today (LoopbackNetwork + bot peers) and is
/// structured so the Tailscale mode lights up when the Phase 0
/// TailscaleKitBackend lands: `DemoWorld.startNode` is the only place a
/// backend is chosen.
@main
struct MeshChatDemoApp: App {
    @State private var world = DemoWorld(deviceName: deviceLabel())

    var body: some Scene {
        WindowGroup {
            ContentView(world: world)
        }
    }

    private static func deviceLabel() -> String {
        #if os(iOS)
            return UIDevice.current.name
        #else
            return Host.current().localizedName ?? "My Mac"
        #endif
    }
}
