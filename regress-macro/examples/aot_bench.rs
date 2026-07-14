//! AoT vs runtime-engine throughput over the Sherlock corpus.
//!
//! Each row is a pattern the `regex!` macro compiles ahead of time; columns
//! are MB/s for the AoT matcher, the classical backtracker (`regress::Regex`),
//! and the TDFA interpreter (`TdfaExecutor`) — the AoT backend's exact-
//! behavior sibling. Match counts are cross-checked across all three.
//!
//! Run with: `cargo run --release -p regress-macro --example aot_bench`

use regress::backends::{self, TdfaExecutor, TdfaProgram};
use regress_macro::regex;
use std::hint::black_box;
use std::time::{Duration, Instant};

const SHERLOCK: &str = include_str!("../../examples/data/sherlock.txt");

/// Median MB/s over 5 runs of 5 scans each.
fn throughput(input_len: usize, mut scan: impl FnMut() -> usize) -> f64 {
    const ITERS: usize = 5;
    const RUNS: usize = 5;
    let mut times: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            let mut n = 0;
            for _ in 0..ITERS {
                n += scan();
            }
            black_box(n);
            start.elapsed()
        })
        .collect();
    times.sort();
    (input_len * ITERS) as f64 / times[RUNS / 2].as_secs_f64() / 1e6
}

macro_rules! bench_case {
    ($name:literal, $pattern:literal, $flags:literal) => {{
        let aot = regex!($pattern, $flags);
        let mut flags = regress::Flags::from($flags);
        flags.unicode = true;
        let bt = regress::Regex::with_flags($pattern, flags).expect("backtracker build");
        let mut ire = backends::try_parse($pattern.chars().map(u32::from), flags).expect("parse");
        backends::optimize(&mut ire);
        let program = TdfaProgram::try_from_ir(&ire).expect("program build");

        let n_aot = aot.find_iter(SHERLOCK).count();
        let n_bt = bt.find_iter(SHERLOCK).count();
        let n_tdfa = backends::find::<TdfaExecutor>(&program, SHERLOCK, 0).count();
        assert_eq!(n_aot, n_bt, "{}: AoT vs backtracker count", $name);
        assert_eq!(n_aot, n_tdfa, "{}: AoT vs TDFA count", $name);

        let mb_aot = throughput(SHERLOCK.len(), || aot.find_iter(SHERLOCK).count());
        let mb_bt = throughput(SHERLOCK.len(), || bt.find_iter(SHERLOCK).count());
        let mb_tdfa = throughput(SHERLOCK.len(), || {
            backends::find::<TdfaExecutor>(&program, SHERLOCK, 0).count()
        });
        println!(
            "{:<28} {:>8} {:>10.0} {:>10.0} {:>10.0}",
            $name, n_aot, mb_aot, mb_bt, mb_tdfa
        );
    }};
}

fn main() {
    println!(
        "{:<28} {:>8} {:>10} {:>10} {:>10}",
        "pattern", "matches", "aot", "backtrack", "tdfa"
    );
    bench_case!("Holmes (literal)", "Holmes", "");
    bench_case!("alt literals", "Sherlock|Holmes|Watson", "");
    bench_case!("Sherlock/i (casefold)", "Sherlock", "i");
    bench_case!("alt prefixes", "Sher[a-z]+|Hol[a-z]+", "");
    bench_case!("prefix + \\w+", r"Sherlock\w+", "");
    bench_case!("prefix + group", r"Holmes (\w+)", "");
    bench_case!("interior literal", r"\w+\s+Holmes", "");
    bench_case!("scan + group", "([a-z]+)ing", "");
    bench_case!("digits window", "[0-9][0-9][0-9]-[0-9][0-9][0-9][0-9]", "");
}
