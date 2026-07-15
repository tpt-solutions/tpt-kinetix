//! Decode throughput benchmark stub (Phase 3 placeholder).
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_placeholder(_c: &mut Criterion) {
    // TODO(phase-3): implement H.264 decode throughput benchmark.
}

criterion_group!(benches, bench_placeholder);
criterion_main!(benches);
