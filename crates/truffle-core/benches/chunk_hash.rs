//! SHA-256 throughput micro-benchmarks (RFC 019 §10.1).
//!
//! File transfers hash every byte end-to-end. If sha2 slows down (platform
//! change, library regression), transfer throughput tanks. This bench
//! isolates the hashing cost across typical buffer sizes.
//!
//! Run with: `cargo bench -p truffle-core --bench chunk_hash`

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use sha2::{Digest, Sha256};

fn make_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| ((i * 13 + 7) % 256) as u8).collect()
}

fn bench_sha256_full_buffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("chunk_hash/sha256");

    for &size in &[
        4 * 1024usize,
        64 * 1024,
        256 * 1024,
        1024 * 1024,
        4 * 1024 * 1024,
    ] {
        let buf = make_payload(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &buf, |b, buf| {
            b.iter(|| {
                let mut hasher = Sha256::new();
                hasher.update(black_box(buf.as_slice()));
                let out = hasher.finalize();
                black_box(out);
            });
        });
    }
    group.finish();
}

fn bench_sha256_streamed_chunks(c: &mut Criterion) {
    // Emulate file-transfer's streaming path: hash a 4 MB "file" in
    // fixed-size chunks via `update()` then finalize once.
    let total: usize = 4 * 1024 * 1024;
    let buf = make_payload(total);

    let mut group = c.benchmark_group("chunk_hash/sha256_streamed");
    group.throughput(Throughput::Bytes(total as u64));

    for &chunk_size in &[4 * 1024usize, 16 * 1024, 64 * 1024, 256 * 1024] {
        group.bench_with_input(
            BenchmarkId::from_parameter(chunk_size),
            &chunk_size,
            |b, &chunk_size| {
                b.iter(|| {
                    let mut hasher = Sha256::new();
                    for chunk in buf.chunks(chunk_size) {
                        hasher.update(black_box(chunk));
                    }
                    let out = hasher.finalize();
                    black_box(out);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_sha256_full_buffer,
    bench_sha256_streamed_chunks
);
criterion_main!(benches);
