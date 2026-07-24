# Releasing Truffle

Truffle uses Release Please as its only versioning and changelog system. Do
not run Changesets or manually publish an individual package: the JavaScript
packages, Rust crates, native addons, sidecars, lockfile, and checksum maps
form one release unit.

## Release contract

Every public artifact has the same version. `scripts/check-release-versions.mjs`
enforces that invariant across package manifests, Cargo manifests, and
`Cargo.lock`.

Sidecar binaries are fail-closed:

- A supported platform must have a SHA-256 checksum for the exact release
  version.
- A missing or mismatched checksum fails installation/build.
- PATH fallback is allowed only through the explicit
  `TRUFFLE_SIDECAR_SKIP_DOWNLOAD=1` development escape hatch or an explicit
  `TRUFFLE_SIDECAR_PATH`.

The source tag is immutable. Generated checksum maps are committed back to
the default branch only after every sidecar asset exists and is hashed.

## Normal release

1. Merge conventional commits to `main`.
2. Review the Release Please PR. It must update every version marker and
   `Cargo.lock`; CI validates the pre-tag graph with:

   ```bash
   node scripts/check-release-versions.mjs --allow-missing-checksums
   cargo metadata --locked --no-deps --format-version 1
   ```

   The checksum exception is intentional here: binaries for the new version
   cannot exist before its tag.
3. Merge the Release Please PR. Release Please creates
   `truffle-v<version>` and dispatches the independent CLI, sidecar, and NAPI
   builds.
4. The sidecar workflow builds all five targets, attaches them to the GitHub
   release, verifies their hashes, publishes the platform sidecar packages,
   and commits identical checksum maps for Rust and JavaScript consumers.
5. Only after step 4 succeeds, the sidecar workflow dispatches the Rust-crate
   and primary npm-package publishers. Both run strict validation:

   ```bash
   node scripts/check-release-versions.mjs
   ```

6. Confirm the GitHub release and all registries show the same version:
   crates.io (`truffle`, `truffle-core`, `truffle-sidecar`),
   npm (`@vibecook/truffle`, `@vibecook/truffle-react`,
   `@vibecook/truffle-native`, and all platform packages), and the CLI
   archives.

## Reruns and failures

Workflow reruns are safe. The crate publisher checks the exact version on
crates.io and skips only versions that are already present; any real
`cargo publish` error still fails the job. npm's provenance-enabled publish
steps likewise skip only an exact version already in the registry.

If a sidecar or checksum job fails, do not dispatch downstream publishers
manually. Fix and rerun the sidecar workflow for the existing tag so the
checksum gate remains the serialization point.

If a registry has only part of a release, rerun the relevant workflow at the
same tag. Never change a released artifact or reuse a version number.

## Pre-release verification

Before merging a release PR:

```bash
pnpm install --frozen-lockfile
pnpm run build
pnpm run test
cargo fmt --all -- --check
TRUFFLE_SIDECAR_SKIP_DOWNLOAD=1 cargo clippy --locked --workspace --all-targets \
  --exclude truffle-tauri-plugin --exclude truffle-napi -- -D warnings
cargo test --locked --workspace
```

The real-tailnet workflow and the Linux ignored transport suite must also be
green. See [TESTING.md](TESTING.md) for credentials and local commands.
