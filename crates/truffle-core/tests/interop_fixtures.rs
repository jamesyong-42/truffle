//! Cross-runtime semantic fixtures (RFC 024 §8.4).
//!
//! The JSON files under `apple/Tests/TruffleTests/Fixtures/` are decoded and
//! validated by BOTH this suite and the Swift test suite
//! (`WireTests.swift`), asserting the two implementations agree on the wire
//! schemas — hello v2 and message envelopes — before any live interop test
//! exists. Comparisons are structural (parsed values), never byte-for-byte.
//!
//! The fixtures live in the monorepo's `apple/` tree; when the crate is
//! built outside the monorepo (e.g. from a published .crate), these tests
//! skip rather than fail.

use std::path::PathBuf;

use truffle_core::envelope::Envelope;
use truffle_core::session::hello::{HelloEnvelope, HelloKind};

fn fixture(name: &str) -> Option<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apple/Tests/TruffleTests/Fixtures")
        .join(name);
    match std::fs::read(&path) {
        Ok(data) => Some(data),
        Err(_) => {
            eprintln!(
                "skipping: fixture {} not found (building outside the monorepo?)",
                path.display()
            );
            None
        }
    }
}

#[test]
fn hello_v2_fixture_decodes() {
    let Some(data) = fixture("hello_v2.json") else {
        return;
    };
    let hello: HelloEnvelope = serde_json::from_slice(&data).expect("hello fixture decodes");
    assert_eq!(hello.kind, HelloKind::Hello);
    assert_eq!(hello.version, HelloEnvelope::CURRENT_VERSION);
    assert!(hello.version >= HelloEnvelope::MIN_SUPPORTED_VERSION);
    assert_eq!(hello.identity.app_id, "playground");
    assert_eq!(hello.identity.device_id, "01J4K9M2Z8AB3RNYQPW6H5TC0X");
    assert_eq!(hello.identity.device_name, "Alice's MacBook");
    assert_eq!(hello.identity.os, "darwin");
    assert_eq!(hello.identity.tailscale_id, "node-abc");
    // RFC 022 I1 invariant encoded in the fixture:
    assert!(!hello.identity.device_id.is_empty());
    assert_ne!(hello.identity.device_id, hello.identity.tailscale_id);
}

#[test]
fn envelope_message_fixture_decodes() {
    let Some(data) = fixture("envelope_message.json") else {
        return;
    };
    let env = Envelope::deserialize(&data).expect("envelope fixture decodes");
    assert_eq!(env.namespace, "chat");
    assert_eq!(env.msg_type, "message");
    assert_eq!(env.timestamp, Some(1_752_675_000_000));
    assert_eq!(env.from, None);
    assert_eq!(env.payload["text"], "héllo wörld — 你好");
    assert_eq!(env.payload["count"], 42);
    assert_eq!(env.payload["big"], 1_752_675_000_000_u64);
    // Above i64::MAX — serde_json's u64 arm; Swift decodes this as .uint.
    assert_eq!(env.payload["huge"], u64::MAX);
    assert_eq!(env.payload["pi"], 3.5);
    assert_eq!(env.payload["ok"], true);
    assert!(env.payload["nothing"].is_null());
    assert_eq!(env.payload["nested"]["deep"][2], 3);
}

#[test]
fn envelope_bytes_fixture_decodes() {
    let Some(data) = fixture("envelope_bytes.json") else {
        return;
    };
    let env = Envelope::deserialize(&data).expect("bytes fixture decodes");
    assert_eq!(env.namespace, "ft");
    assert_eq!(env.msg_type, "bytes");
    assert_eq!(env.payload["encoding"], "base64");
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(env.payload["data"].as_str().expect("data is a string"))
        .expect("valid base64");
    assert_eq!(decoded, b"hello truffle");
}

#[test]
fn envelope_unknown_fields_are_ignored() {
    let Some(data) = fixture("envelope_unknown_fields.json") else {
        return;
    };
    // Forward compatibility: `v`, `from_device_id`, and future fields are
    // silently ignored — this must hold in both runtimes (RFC 024 §8.3).
    let env = Envelope::deserialize(&data).expect("unknown fields are ignored");
    assert_eq!(env.namespace, "chat");
    assert_eq!(env.payload["text"], "forward compat");
}
