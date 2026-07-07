// Pure integrity helpers shared between `build.rs` and the crate's tests.
//
// This file has NO I/O and NO side effects: it computes and compares SHA-256
// digests and looks entries up out of the committed `sidecar-checksums.json`
// trust anchor. Keeping the logic here (rather than inline in `build.rs`) lets
// the exact same code be unit-tested.
//
// It is consumed two ways:
//   - `build.rs` pulls it in with `include!("src/integrity.rs")`, compiling it
//     into the build script against `[build-dependencies]`.
//   - `lib.rs` compiles it as `#[cfg(test)] mod integrity;`, so the tests run
//     under `cargo test` against `[dev-dependencies]` — this keeps `sha2` and
//     `serde_json` out of the published library's regular dependencies.
//
// The `#[cfg(test)]` test module at the bottom is stripped from the build
// script compilation (build scripts never see `cfg(test)`), so no `//!` inner
// doc comments are used here — they would be illegal once this file is spliced
// into `build.rs` after its items via `include!`.

/// Hex-encode the SHA-256 of `bytes` as a 64-char lowercase string.
///
/// Uses a manual `{:02x}` loop rather than the `hex` crate so this module adds
/// no dependency beyond `sha2`, which both the build script and the tests
/// already pull in.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Writing a byte to a String is infallible.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Verify that `bytes` hash to `expected_hex`.
///
/// The comparison ignores ASCII case and trims surrounding whitespace on
/// `expected_hex`. On mismatch the `Err` names both the expected and the
/// actual digest so callers can surface a useful message.
pub(crate) fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<(), String> {
    let expected = expected_hex.trim();
    let actual = sha256_hex(bytes);
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!("expected sha256 {expected}, got {actual}"))
    }
}

/// Look up the expected SHA-256 for `version`/`asset` in a checksums JSON map.
///
/// The map is shaped `{ "<version>": { "<asset>": "<sha256-hex>" } }`. Both a
/// bare `"X.Y.Z"` and a `"vX.Y.Z"` version key are accepted, mirroring
/// `scripts/postinstall.cjs`'s `loadExpectedChecksum`.
///
/// - `Err(_)` — the JSON is malformed; the build script treats this as fatal.
/// - `Ok(None)` — the JSON parsed but has no entry for this version/asset. The
///   build script warns (integrity not verified) rather than failing, so a
///   freshly tagged release whose hash is not committed yet still builds.
/// - `Ok(Some(hex))` — the pinned checksum for this asset.
pub(crate) fn lookup_checksum(
    json: &str,
    version: &str,
    asset: &str,
) -> Result<Option<String>, String> {
    let map: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("malformed JSON: {e}"))?;

    let by_version = map.get(version).or_else(|| map.get(format!("v{version}")));

    let checksum = by_version
        .and_then(|entry| entry.get(asset))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    Ok(checksum)
}

#[cfg(test)]
mod tests {
    use super::{lookup_checksum, sha256_hex, verify_sha256};

    /// SHA-256 of the ASCII string "hello".
    const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn sha256_hex_known_vector() {
        assert_eq!(sha256_hex(b"hello"), HELLO_SHA256);
    }

    #[test]
    fn verify_accepts_matching_bytes() {
        assert!(verify_sha256(b"hello", HELLO_SHA256).is_ok());
        // The expected hex is compared case-insensitively.
        assert!(verify_sha256(b"hello", &HELLO_SHA256.to_uppercase()).is_ok());
    }

    #[test]
    fn verify_rejects_tampered_bytes() {
        // Regression: before this fix there was NO code path that could reject
        // tampered bytes — build.rs wrote whatever it downloaded.
        let err = verify_sha256(b"hello tampered", HELLO_SHA256)
            .expect_err("tampered bytes must not verify against the hello digest");
        // The message must name both the expected and the actual digest.
        assert!(err.contains(HELLO_SHA256), "missing expected hex: {err}");
        assert!(
            err.contains(&sha256_hex(b"hello tampered")),
            "missing actual hex: {err}"
        );
    }

    #[test]
    fn lookup_finds_entry() {
        let got = lookup_checksum(r#"{"0.4.8": {"a": "x"}}"#, "0.4.8", "a").unwrap();
        assert_eq!(got.as_deref(), Some("x"));
    }

    #[test]
    fn lookup_accepts_v_prefixed_version_key() {
        // A "vX.Y.Z" key resolves for a bare "X.Y.Z" lookup, mirroring
        // postinstall.cjs's `map[version] || map[`v${version}`]`.
        let got = lookup_checksum(r#"{"v0.4.8": {"a": "x"}}"#, "0.4.8", "a").unwrap();
        assert_eq!(got.as_deref(), Some("x"));
    }

    #[test]
    fn lookup_missing_version_or_asset_is_none() {
        let json = r#"{"0.4.8": {"a": "x"}}"#;
        // Unknown version -> None (warn, don't fail).
        assert_eq!(lookup_checksum(json, "9.9.9", "a").unwrap(), None);
        // Known version, unknown asset -> None.
        assert_eq!(lookup_checksum(json, "0.4.8", "b").unwrap(), None);
    }

    #[test]
    fn lookup_malformed_json_is_err() {
        assert!(lookup_checksum("not json", "0.4.8", "a").is_err());
    }

    #[test]
    fn committed_checksums_file_is_well_formed() {
        // Guards against release-time typos in the committed trust anchor:
        // every real (non-"_"-prefixed) version entry must map its assets to
        // 64-char lowercase hex digests.
        let raw = include_str!("../sidecar-checksums.json");
        let map: serde_json::Value = serde_json::from_str(raw).expect("valid JSON");
        let obj = map.as_object().expect("top level is an object");
        for (version, assets) in obj {
            if version.starts_with('_') {
                continue; // documentation keys such as "_comment"
            }
            let assets = assets
                .as_object()
                .unwrap_or_else(|| panic!("version {version} must map to an object"));
            for (asset, digest) in assets {
                let digest = digest
                    .as_str()
                    .unwrap_or_else(|| panic!("{version}/{asset} must be a string"));
                assert_eq!(
                    digest.len(),
                    64,
                    "{version}/{asset} must be 64 hex chars, got {digest:?}"
                );
                assert!(
                    digest.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                    "{version}/{asset} must be lowercase hex, got {digest:?}"
                );
            }
        }
    }
}
