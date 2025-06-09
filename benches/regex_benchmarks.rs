use criterion::{Criterion, criterion_group, criterion_main};
use regress::Regex;

// 非常简单的测试文本
const TEXT: &str = "Tom caught 123 fish. Great! ∞";

fn bench_simple_compile(c: &mut Criterion) {
    c.bench_function("compile_literal", |b| b.iter(|| Regex::new("Tom").unwrap()));

    c.bench_function("compile_digit", |b| b.iter(|| Regex::new(r"\d+").unwrap()));
}

fn bench_simple_match(c: &mut Criterion) {
    let regex = Regex::new("Tom").unwrap();

    c.bench_function("find_literal", |b| b.iter(|| regex.find(TEXT).is_some()));

    let digit_regex = Regex::new(r"\d+").unwrap();
    c.bench_function("find_digit", |b| {
        b.iter(|| digit_regex.find(TEXT).is_some())
    });
}

fn bench_simple_replace(c: &mut Criterion) {
    let regex = Regex::new("Tom").unwrap();

    c.bench_function("replace_literal", |b| {
        b.iter(|| regex.replace(TEXT, "Jerry"))
    });
}

criterion_group!(
    benches,
    bench_simple_compile,
    bench_simple_match,
    bench_simple_replace
);
criterion_main!(benches);
