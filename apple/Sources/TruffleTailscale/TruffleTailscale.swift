/// TruffleTailscale — Layer 0–1 of the Apple runtime (RFC 024 §5.1).
///
/// This target hosts the libtailscale / TailscaleKit glue: the embedded node
/// runtime, IPN bus supervisor, auth flow, and the production
/// `NetworkBackend` implementation (`TailscaleKitBackend`).
///
/// The TailscaleKit-dependent code compiles only when the pinned
/// `TailscaleKit.xcframework` is wired into the package
/// (`#if canImport(TailscaleKit)` — RFC 024 §5.4). Until then this target
/// carries the backend-independent pieces so the package builds everywhere.

@_exported import Truffle

#if canImport(TailscaleKit)
    import TailscaleKit

    // TailscaleKitBackend lands with the Phase 0 device spike (RFC 024 §12):
    // full-duplex stream adoption, listener supervision, LocalAPI WhoIs.
#endif
