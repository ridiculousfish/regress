use criterion::{Criterion, criterion_group, criterion_main};
use regress::Regex;
use std::hint::black_box;

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("match", |b| {
        b.iter(|| {
            let re = Regex::new(r"\d+").unwrap();
            let _result = re.find(black_box("Price: $123"));
        })
    });

    c.bench_function("complex match", |b| {
        b.iter(|| {
            let re = Regex::new(r"(\d{1,2})/(\d{1,2})/(\d{4})").unwrap();
            let _result = re.find(black_box("Born on 12/25/1990 and graduated on 5/15/2012"));
        })
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);