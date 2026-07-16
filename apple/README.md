# Truffle Swift — Apple-native mesh runtime

Swift implementation of the Truffle product (RFC 024): same mesh model as
`@vibecook/truffle` — Peers, `appId` isolation, namespaced messages — built
for in-process embedding on iOS/macOS via libtailscale/TailscaleKit instead
of the desktop sidecar.

## Package layout

| Target | Contents |
|---|---|
| `Truffle` | Product core: identity (AppId / ULID / hostname slug), wire codecs (hello v2, envelope, byte payloads), session handshake + close codes, generation-checked peer registry, `MeshNode` actor, `NetworkBackend` seam, loopback test backend |
| `TruffleTailscale` | Layer 0–1 (libtailscale / TailscaleKit glue). The production `TailscaleKitBackend` compiles only when `TailscaleKit.xcframework` is wired up (`#if canImport(TailscaleKit)`) |

## Build & test

```bash
cd apple
swift build
# Tests need swift-testing, which CommandLineTools does not bundle:
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift test
```

Stick to one toolchain per `.build` directory: CommandLineTools and Xcode may
ship different Swift versions, and mixing them yields "module compiled with
Swift X cannot be imported" — `rm -rf .build` and rebuild if you switch.

## Wire compatibility (RFC 024 §8)

The `Truffle` target implements the shipped `truffle-core` contracts
verbatim — hello v2 (`kind`/`version: 2`/`identity`, snake_case, desktop
field bounds, `device_id ≠ tailscale_id`), the JSON message envelope (no `v`,
no `from_device_id`, receiver-stamped attribution), close codes 4001/4002/4003,
fail-closed inbound WhoIs policy, and the `truffle-{appId}-{slug}` hostname
scheme with `is_app_peer` candidate filtering.

Cross-runtime semantic fixtures live in `Tests/TruffleTests/Fixtures/` and are
decoded by BOTH this suite (`WireTests.swift`) and the Rust suite
(`crates/truffle-core/tests/interop_fixtures.rs`).

Two intentional divergences from Rust internals (documented in `Slug.swift`;
safe because a node only derives its *own* hostname and remote nodes validate
only the prefix): transliteration uses `CFStringTransform` instead of
`deunicode`, and the slug fallback hash is SHA-256 instead of BLAKE3.

## Status vs RFC 024 phases

Done (this tree, tested on macOS via the loopback backend):

- Identity: AppId validation, device ULID (`device-id.txt`, desktop-compatible),
  DeviceName, 10-step hostname slug + `truffle-{appId}-{slug}` composition
- Wire: hello v2 + envelope codecs with all desktop bounds; base64 `"bytes"`
  schema with 11 MiB cap; 15 MiB envelope bound
- Session: role-ordered handshake, 4001/4002/4003 on the wire, ≤16 control
  frames, 5s/10s deadlines, fail-closed WhoIs with explicit test-only opt-out
- Mesh: `MeshNode` (start contract, listener supervision with backoff,
  events streams with bounded buffers), generation-checked `Peer`/`PeerRef`
  (peerGone on stale refs), desktop-parity peer query resolution, `send` /
  `sendBytes` / `sendJSON` / `onMessage` with bounded serial subscriptions

Pending (Phase 0 device work — needs TailscaleKit + hardware):

- Pinned, checksummed `TailscaleKit.xcframework` build (RFC 024 §5.4)
- Full-duplex stream adapter + RFC 6455 `/ws` adoption (§5.5) — sessions
  currently run over an in-memory frame transport in tests only; the
  `FrameTransport` seam is where the WebSocket adapter lands
- `TailscaleKitBackend`: IPN bus supervisor + `backendStatus` polling +
  LocalAPI WhoIs (extract from
  `project100/research/tailscale/ios-prototype`: `TailscaleSession`,
  `BusConsumer`)
- Interactive auth presentation (`SafariView` → `TruffleSwiftUI`)
- `urlSession()` over the embedded SOCKS proxy

## Reserved port

TCP **9417** is the session WebSocket port (`truffle-core` default);
`MeshNode.listen` refuses it, matching desktop.
