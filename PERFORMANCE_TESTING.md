# Performance Testing for Regress

This document describes the performance testing setup for the regress crate, which allows you to track performance metrics across commits and detect regressions.

## Quick Start

### Running Benchmarks Locally

```bash
# Run all benchmarks
cargo bench

# Or use the convenient script
./scripts/run_benchmarks.sh

# Save current results as a baseline
./scripts/run_benchmarks.sh --baseline main

# Compare with a saved baseline
./scripts/run_benchmarks.sh --compare main
```

### Viewing Results

After running benchmarks, you can view detailed results:
- **HTML Report**: Open `target/criterion/report/index.html` in your browser
- **Terminal Output**: Timing information is displayed during the benchmark run
- **Summary Report**: A timestamped summary file is generated

## Benchmark Categories

The performance test suite includes several categories of benchmarks:

### 1. Regex Compilation (`regex_compile`)
Tests the time taken to compile regex patterns from strings.
- Various pattern complexities
- Unicode support
- Error cases

### 2. Pattern Matching (`regex_find_all`)
Tests finding all matches in text of different sizes.
- Simple literals (e.g., "Twain")
- Character classes (e.g., "[a-z]shing")
- Complex patterns with lookahead/lookbehind
- Unicode patterns

### 3. Text Replacement (`regex_replace_all`)
Tests replacing all occurrences of patterns.
- Simple replacements
- Capture group substitutions
- Complex replacement patterns

### 4. Text Splitting (`regex_split`)
Tests splitting text on regex boundaries.
- Whitespace splitting
- Punctuation splitting
- Word boundary splitting

### 5. Pathological Cases (`pathological_cases`)
Tests patterns that might cause exponential behavior.
- Nested quantifiers
- Alternation backtracking
- Deep recursion patterns

## Test Data

Benchmarks use two test files:
- `test_data/small_text.txt` - Small text sample for quick tests
- `test_data/medium_text.txt` - Medium text sample for throughput tests

Both files contain:
- Common English text
- Various patterns to match
- Unicode characters
- Numbers and punctuation

## CI/CD Integration

### GitHub Actions

The project includes a GitHub Actions workflow (`.github/workflows/performance.yml`) that:

1. **On every push/PR**: Runs benchmarks and compares with the main branch baseline
2. **Daily scheduled runs**: Compares current main with previous releases
3. **Artifact storage**: Saves benchmark results for historical tracking
4. **PR comments**: Posts performance summaries on pull requests

### Setting Up Baselines

When you make significant changes to the codebase:

```bash
# Before your changes - establish baseline
git checkout main
./scripts/run_benchmarks.sh --baseline before_changes

# After your changes - compare
git checkout your_feature_branch
./scripts/run_benchmarks.sh --compare before_changes
```

## Interpreting Results

### Criterion Output

Criterion provides several metrics:
- **Time**: Mean execution time with confidence intervals
- **Throughput**: For operations on text (bytes/second, elements/second)
- **Change**: Percentage change compared to baseline

### What to Look For

🟢 **Good Signs**:
- Stable or improved performance
- Low variance in timing
- Throughput improvements

🔴 **Warning Signs**:
- >10% performance degradation
- High variance (indicates inconsistent performance)
- Timeouts on pathological cases

### Example Output

```
regex_compile/simple_literal    time:   [125.3 ns 126.8 ns 128.5 ns]
                                change: [-2.1% -0.8% +0.6%] (no significant change)

regex_find_all/character_class  time:   [45.2 μs 45.8 μs 46.5 μs]
                                thrpt:  [344.2 MB/s 349.1 MB/s 353.8 MB/s]
                                change: [+2.3% +4.1% +5.9%] (performance regression)
```

## Configuration

### Benchmark Settings

Key settings in `Cargo.toml`:
```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "regex_benchmarks"
harness = false
```

### Build Optimization

Settings in `.cargo/config.toml`:
```toml
[profile.bench]
opt-level = 3      # Maximum optimization
lto = true         # Link-time optimization
codegen-units = 1  # Better optimization
panic = "abort"    # Smaller binaries
```

## Adding New Benchmarks

To add a new benchmark:

1. **Add test pattern** to the `PATTERNS` array in `benches/regex_benchmarks.rs`
2. **Create new benchmark function** following the existing pattern:
   ```rust
   fn bench_my_feature(c: &mut Criterion) {
       let mut group = c.benchmark_group("my_feature");
       // ... benchmark code
       group.finish();
   }
   ```
3. **Add to criterion_group!** macro at the bottom of the file
4. **Update this documentation** with the new benchmark category

## Troubleshooting

### Common Issues

**Benchmarks take too long**:
- Reduce sample size in criterion configuration
- Use smaller test data
- Focus on specific benchmark groups

**Inconsistent results**:
- Close other applications
- Run on dedicated hardware
- Increase measurement time
- Check for thermal throttling

**CI failures**:
- Check if baseline exists
- Verify test data files are committed
- Ensure sufficient CI runner resources

### Debug Mode

Run with verbose output:
```bash
./scripts/run_benchmarks.sh --verbose
RUST_LOG=debug cargo bench
```

## Best Practices

### For Developers

1. **Run benchmarks before committing** performance-critical changes
2. **Establish baselines** before starting optimization work
3. **Document performance goals** in commit messages
4. **Profile before optimizing** to identify actual bottlenecks

### For CI/CD

1. **Use consistent hardware** for reliable comparisons
2. **Store historical data** for trend analysis
3. **Set up alerts** for significant regressions
4. **Archive results** for long-term tracking

### For Analysis

1. **Look at trends**, not just single measurements
2. **Consider statistical significance** of changes
3. **Test on representative data** that matches real usage
4. **Measure what matters** to end users

## Further Reading

- [Criterion.rs Documentation](https://docs.rs/criterion/)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)
- [Benchmarking Best Practices](https://github.com/bheisler/criterion.rs/blob/master/book/src/user_guide/advanced_configuration.md) 