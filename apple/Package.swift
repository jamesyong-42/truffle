// swift-tools-version: 6.0
// Truffle Swift — Apple-native mesh runtime (RFC 024).
//
// The `Truffle` target is the backend-agnostic product core (identity, wire
// codecs, session handshake, peer registry, MeshNode). It has no dependency
// on TailscaleKit and builds/tests on macOS with plain SwiftPM.
//
// The `TruffleTailscale` target hosts the L0–L1 runtime (libtailscale /
// TailscaleKit glue). The TailscaleKit-dependent backend only compiles when
// the TailscaleKit.xcframework is wired up (`#if canImport(TailscaleKit)`);
// until then the target carries the backend-independent supervisor logic.

import PackageDescription

let package = Package(
    name: "Truffle",
    platforms: [
        .macOS(.v14),
        .iOS(.v17),
    ],
    products: [
        .library(name: "Truffle", targets: ["Truffle"]),
        .library(name: "TruffleSwiftUI", targets: ["TruffleSwiftUI"]),
        .library(name: "TruffleTailscale", targets: ["TruffleTailscale"]),
    ],
    targets: [
        .target(
            name: "Truffle",
            path: "Sources/Truffle"
        ),
        .target(
            name: "TruffleSwiftUI",
            dependencies: ["Truffle"],
            path: "Sources/TruffleSwiftUI"
        ),
        .target(
            name: "TruffleTailscale",
            dependencies: ["Truffle"],
            path: "Sources/TruffleTailscale"
        ),
        .testTarget(
            name: "TruffleTests",
            dependencies: ["Truffle"],
            path: "Tests/TruffleTests",
            resources: [
                .copy("Fixtures")
            ]
        ),
    ]
)
