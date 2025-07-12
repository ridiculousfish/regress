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

    c.bench_function("complex replacement", |b| {
        b.iter(|| {
            let re = Regex::new(r"(\d{1,2})/(\d{1,2})/(\d{4})").unwrap();
            let _result = re.replace_all(
                black_box("Born on 12/25/1990 and graduated on 5/15/2012"),
                black_box("$3-$1-$2"),
            );
        })
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
