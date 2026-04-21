//! Loro CRDT merge micro-benchmarks (RFC 019 §10.1).
//!
//! Benchmarks the raw merge path used by `truffle_core::crdt_doc`: two
//! LoroDocs independently accumulate operations, then exchange update
//! bytes. This isolates the CRDT math from the network layer.
//!
//! Scenarios approximate the dmonad/crdt-benchmarks B1/B2 shapes — the
//! full macro versions live in the `truffle-bench` crate.
//!
//! Run with: `cargo bench -p truffle-core --bench crdt_merge`

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use loro::{LoroDoc, LoroMap};

/// Insert `n` key/value pairs into a map container.
fn fill_map(doc: &LoroDoc, container_name: &str, n: usize) {
    let map: LoroMap = doc.get_map(container_name);
    for i in 0..n {
        let key = format!("k{i}");
        let val: i64 = i as i64;
        map.insert(&key, val).expect("insert");
    }
    doc.commit();
}

fn bench_apply_updates(c: &mut Criterion) {
    let mut group = c.benchmark_group("crdt/apply_updates");

    for &ops in &[100usize, 1_000, 10_000] {
        // Prepare an update blob from a source doc.
        let source = LoroDoc::new();
        fill_map(&source, "m", ops);
        let update = source
            .export(loro::ExportMode::all_updates())
            .expect("export");
        let bytes = update.len();

        group.throughput(criterion::Throughput::Bytes(bytes as u64));
        group.bench_with_input(BenchmarkId::from_parameter(ops), &update, |b, update| {
            b.iter(|| {
                let target = LoroDoc::new();
                target.import(black_box(update)).expect("import");
                black_box(target);
            });
        });
    }
    group.finish();
}

fn bench_concurrent_merge(c: &mut Criterion) {
    // Simulates B2: two parties each make N writes independently, then
    // merge each other's updates. Measures the round-trip merge path.
    let mut group = c.benchmark_group("crdt/concurrent_merge");

    for &ops in &[100usize, 1_000, 5_000] {
        group.throughput(criterion::Throughput::Elements(ops as u64));
        group.bench_with_input(BenchmarkId::from_parameter(ops), &ops, |b, &ops| {
            b.iter(|| {
                let a = LoroDoc::new();
                let b_doc = LoroDoc::new();

                fill_map(&a, "m", ops);
                fill_map(&b_doc, "n", ops);

                let a_update = a.export(loro::ExportMode::all_updates()).expect("a export");
                let b_update = b_doc
                    .export(loro::ExportMode::all_updates())
                    .expect("b export");

                a.import(&b_update).expect("a import b");
                b_doc.import(&a_update).expect("b import a");

                black_box((a, b_doc));
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_apply_updates, bench_concurrent_merge);
criterion_main!(benches);
