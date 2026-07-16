# MeshChat — RFC 024 example app

An iOS SwiftUI chat app over the Truffle mesh. Today it runs an
**in-process demo mesh**: your node plus two bot peers (Echo, Shout) on a
`LoopbackNetwork`. Everything above Layer 1 is the real Truffle stack —
hostname-based discovery, hello v2 handshakes with WhoIs verification,
namespaced JSON messages, generation-checked peers. Only the Tailscale layer
is simulated; when the Phase 0 `TailscaleKitBackend` lands, the only change
is the backend passed in `DemoWorld.startNode`.

## Run

Open `MeshChatDemo.xcodeproj` in Xcode 16+ and run on any iOS 17+ simulator
or device. Tap **Start demo mesh**, pick a bot, chat.

Headless/CI smoke run:

```bash
xcodebuild -project MeshChatDemo.xcodeproj -scheme MeshChatDemo \
  -destination 'generic/platform=iOS Simulator' build CODE_SIGNING_ALLOWED=NO
# Launch flags: --autostart (skip connect screen), --autochat (greet every
# peer once, driving the hello handshakes without UI interaction)
xcrun simctl launch booted com.vibecook.truffle.MeshChatDemo --autostart --autochat
```

## What to look at

| File | Shows |
|---|---|
| `DemoMesh.swift` | Starting a `MeshNode` over an explicit backend; bot peers via `onMessage` + `sendJSON` reply |
| `ChatStore.swift` | One `onMessage("chat")` subscription feeding per-peer threads, keyed by `tailscaleId` |
| `ContentView.swift` | `MeshModel` (TruffleSwiftUI) observation: phase, peer list, pre-hello "not yet confirmed" vs confirmed ULIDs |
| `ChatView.swift` | Sending with `sendJSON(to:namespace:payload:)` |

The peer rows demonstrate RFC 022 honest fields live: a peer appears from
its `truffle-{appId}-…` hostname first ("not yet confirmed", no ULID) and
gains its display name + device ULID the moment the first message drives a
hello handshake.
