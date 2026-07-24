# API Stability

Truffle follows Semantic Versioning for every public artifact in the unified
release.

## Before 1.0

The current 0.x line is suitable for evaluation and controlled production
use, but minor releases may still contain breaking API changes. Breaking
changes must be called out in the changelog and migration notes.

## From 1.0

The following are stable public surfaces:

- `@vibecook/truffle` and `@vibecook/truffle-react` exports
- The documented `truffle` CLI commands and machine-readable JSONL shapes
- Public Rust APIs in `truffle`, `truffle-core`, and `truffle-sidecar`
- Documented wire protocols used for communication between supported Truffle
  versions

Breaking changes to those surfaces require a new major version. Compatible
additions may ship in minor versions, and fixes in patch versions.

Deprecated APIs remain available for at least one minor release after a
replacement is documented, except when continued availability would create
a security or data-integrity risk.

The following are implementation details and are not direct compatibility
surfaces: `@vibecook/truffle-native`, architecture-specific native/sidecar
packages, undocumented NAPI helpers, internal Rust modules, and repository
scripts. Their versions remain synchronized with the public release so the
package resolver cannot assemble an incompatible installation.

Supported release lines and security reporting are documented in
[SECURITY.md](../SECURITY.md).
