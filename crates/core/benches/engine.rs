use criterion::{Criterion, criterion_group, criterion_main};

fn benchmark(c: &mut Criterion) {
    c.bench_function("parse 10000 Kanata operations", |b| {
        b.iter(|| futures_lite::future::block_on(xonata_core::benchmark_trace(10_000)))
    });
}
criterion_group!(benches, benchmark);
criterion_main!(benches);
