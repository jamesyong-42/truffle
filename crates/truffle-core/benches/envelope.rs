//! Envelope codec micro-benchmarks (RFC 019 §10.1).
//!
//! Measures JsonCodec encode/decode throughput across payload sizes.
//! Regressions here would slow every message sent or received on the mesh.
//!
//! Run with: `cargo bench -p truffle-core --bench envelope`

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use serde_json::json;
use truffle_core::envelope::{Envelope, EnvelopeCodec, JsonCodec};

fn bench_encode(c: &mut Criterion) {
    let codec = JsonCodec;
    let mut group = c.benchmark_group("envelope/encode");

    for &size in &[64usize, 4 * 1024, 64 * 1024, 256 * 1024] {
        let payload = json!({ "blob": "x".repeat(size) });
        let envelope = Envelope::new("chat", "message", payload).with_timestamp();

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &envelope,
            |b, envelope| {
                b.iter(|| {
                    let bytes = codec.encode(black_box(envelope)).expect("encode");
                    black_box(bytes);
                });
            },
        );
    }
    group.finish();
}

fn bench_decode(c: &mut Criterion) {
    let codec = JsonCodec;
    let mut group = c.benchmark_group("envelope/decode");

    for &size in &[64usize, 4 * 1024, 64 * 1024, 256 * 1024] {
        let payload = json!({ "blob": "x".repeat(size) });
        let envelope = Envelope::new("chat", "message", payload).with_timestamp();
        let bytes = codec.encode(&envelope).expect("pre-encode");

        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &bytes, |b, bytes| {
            b.iter(|| {
                let env = codec.decode(black_box(bytes)).expect("decode");
                black_box(env);
            });
        });
    }
    group.finish();
}

fn bench_roundtrip(c: &mut Criterion) {
    let codec = JsonCodec;
    let payload = json!({
        "name": "alpha-01",
        "count": 42,
        "tags": ["a", "b", "c"],
        "nested": { "key": "value", "list": [1, 2, 3, 4, 5] },
    });
    let envelope = Envelope::new("bus", "ping", payload).with_timestamp();

    c.bench_function("envelope/roundtrip/structured", |b| {
        b.iter(|| {
            let bytes = codec.encode(black_box(&envelope)).expect("encode");
            let decoded = codec.decode(&bytes).expect("decode");
            black_box(decoded);
        });
    });
}

criterion_group!(benches, bench_encode, bench_decode, bench_roundtrip);
criterion_main!(benches);
