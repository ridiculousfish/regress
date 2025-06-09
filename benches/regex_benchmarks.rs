use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use regress::{Flags, Regex};

const SMALL_TEXT: &str = include_str!("../test_data/small_text.txt");
const MEDIUM_TEXT: &str = include_str!("../test_data/medium_text.txt");

// Common regex patterns to benchmark
const PATTERNS: &[(&str, &str)] = &[
    ("simple_literal", "Twain"),
    ("character_class", "[a-z]shing"),
    ("alternation", "Huck[a-zA-Z]+|Saw[a-zA-Z]+"),
    ("word_boundary", r"\b\w+nn\b"),
    ("complex_class", "[a-q][^u-z]{13}x"),
    ("multi_alternation", "Tom|Sawyer|Huckleberry|Finn"),
    ("complex_lookaround", ".{0,2}(Tom|Sawyer|Huckleberry|Finn)"),
    ("bounded_repeat", ".{2,4}(Tom|Sawyer|Huckleberry|Finn)"),
    ("word_suffix", "[a-zA-Z]+ing"),
    ("unicode_chars", "∞|✓"),
    ("digit_sequence", r"\d+"),
    (
        "email_pattern",
        r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}",
    ),
];

fn bench_regex_compile(c: &mut Criterion) {
    let mut group = c.benchmark_group("regex_compile");

    for (name, pattern) in PATTERNS {
        group.bench_with_input(BenchmarkId::new("compile", name), pattern, |b, pattern| {
            b.iter(|| Regex::new(pattern).unwrap())
        });
    }

    group.finish();
}

fn bench_regex_find_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("regex_find_all");

    // Benchmark with different text sizes
    let texts = [("small", SMALL_TEXT), ("medium", MEDIUM_TEXT)];

    for (text_name, text) in &texts {
        group.throughput(Throughput::Bytes(text.len() as u64));

        for (pattern_name, pattern) in PATTERNS {
            let regex = Regex::new(pattern).unwrap();
            let bench_name = format!("{}_on_{}", pattern_name, text_name);

            group.bench_with_input(BenchmarkId::new("find_all", bench_name), text, |b, text| {
                b.iter(|| regex.find_iter(text).count())
            });
        }
    }

    group.finish();
}

fn bench_regex_replace_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("regex_replace_all");

    let texts = [("small", SMALL_TEXT), ("medium", MEDIUM_TEXT)];

    // Test replacement patterns
    let replace_patterns = [
        ("simple_literal", "Twain", "Mark"),
        ("word_suffix", r"(\w+)ing", "${1}ed"),
        ("character_class", "[0-9]+", "NUM"),
    ];

    for (text_name, text) in &texts {
        group.throughput(Throughput::Bytes(text.len() as u64));

        for (pattern_name, pattern, replacement) in &replace_patterns {
            let regex = Regex::new(pattern).unwrap();
            let bench_name = format!("{}_on_{}", pattern_name, text_name);

            group.bench_with_input(
                BenchmarkId::new("replace_all", bench_name),
                &(text, replacement),
                |b, (text, replacement)| b.iter(|| regex.replace_all(text, replacement)),
            );
        }
    }

    group.finish();
}

fn bench_regex_find_matches(c: &mut Criterion) {
    let mut group = c.benchmark_group("regex_find_matches");

    let texts = [("small", SMALL_TEXT), ("medium", MEDIUM_TEXT)];

    let test_patterns = [
        ("whitespace", r"\s+"),
        ("punctuation", r"[,.!?;:]"),
        ("word_boundary", r"\b\w+\b"),
    ];

    for (text_name, text) in &texts {
        group.throughput(Throughput::Bytes(text.len() as u64));

        for (pattern_name, pattern) in &test_patterns {
            let regex = Regex::new(pattern).unwrap();
            let bench_name = format!("{}_on_{}", pattern_name, text_name);

            group.bench_with_input(
                BenchmarkId::new("find_matches", bench_name),
                text,
                |b, text| b.iter(|| regex.find_iter(text).count()),
            );
        }
    }

    group.finish();
}

fn bench_pathological_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("pathological_cases");

    // Test cases that might cause exponential behavior
    let pathological_patterns = [
        (
            "nested_quantifiers",
            r"(a+)+b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            "alternation_backtrack",
            r"(a|a)*b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            "deep_recursion",
            r"((a)*)*b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    ];

    for (name, pattern, input) in &pathological_patterns {
        let regex = Regex::new(pattern).unwrap();

        group.bench_with_input(BenchmarkId::new("pathological", name), input, |b, input| {
            b.iter(|| {
                // Use find() to check if there's a match
                regex.find(input).is_some()
            })
        });
    }

    group.finish();
}

fn bench_case_insensitive(c: &mut Criterion) {
    let mut group = c.benchmark_group("case_insensitive");

    let texts = [("small", SMALL_TEXT), ("medium", MEDIUM_TEXT)];

    // Test case-insensitive patterns using Flags
    let ci_patterns = [
        ("twain_ci", "twain"),
        ("tom_ci", "tom"),
        ("huckleberry_ci", "huckleberry"),
    ];

    for (text_name, text) in &texts {
        group.throughput(Throughput::Bytes(text.len() as u64));

        for (pattern_name, pattern) in &ci_patterns {
            let flags = Flags {
                icase: true,
                ..Default::default()
            };
            let regex = Regex::with_flags(pattern, flags).unwrap();
            let bench_name = format!("{}_on_{}", pattern_name, text_name);

            group.bench_with_input(
                BenchmarkId::new("case_insensitive", bench_name),
                text,
                |b, text| b.iter(|| regex.find_iter(text).count()),
            );
        }
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_regex_compile,
    bench_regex_find_all,
    bench_regex_replace_all,
    bench_regex_find_matches,
    bench_case_insensitive,
    bench_pathological_cases
);
criterion_main!(benches);
