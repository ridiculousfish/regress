use criterion::{Criterion, criterion_group, criterion_main};
use regress::Regex;
use std::hint::black_box;

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("replacement", |b| {
        b.iter(|| {
            let re = Regex::new(r"\d+").unwrap();
            let _result = re.replace(black_box("Price: $123"), black_box("[$0]"));
        })
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
