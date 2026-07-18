# TailscaleKit provenance

`TruffleTailscale` consumes an unmodified TailscaleKit XCFramework built from:

- upstream: `https://github.com/tailscale/libtailscale.git`
- revision: `5e89501def80a6579ca5d0f9a02f336be62b8f2e`
- patch set: none
- license: BSD-3-Clause; see `TAILSCALE-LICENSE`
- recorded build environment: Xcode 26.1 (17B55), Apple Swift 6.2.1,
  Go 1.25.6, Apple silicon macOS

Run `scripts/materialize-tailscalekit.sh`. The resulting 71 MiB framework is
ignored by Git; `Package.swift` enables its binary target when it is present.
The known research build used during the initial integration has these payload
checksums:

| Payload | SHA-256 |
|---|---|
| device framework binary | `e00e8239c576df7ccb88fde16263c19090c9c77aa07bb1b77596d0bbf3f627de` |
| simulator framework binary | `388d316956946b4bfa8e4965c3508496087e800fbd1977073d022e806374c451` |
| XCFramework Info.plist | `f0bdfcc3c0fd0a64cb2952540469da3f5a36ef85c022b7b3070915ec8e661810` |

Build metadata may change the binary digest under a different toolchain. The
source revision and clean-tree checks in the script are mandatory; update this
record deliberately when the toolchain or pinned revision changes.
