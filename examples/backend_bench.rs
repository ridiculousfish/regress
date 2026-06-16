//! Cross-backend matching-throughput benchmark.
//!
//! Runs the same stress suite through all four backends — classical
//! backtracking, PikeVM, the Thompson NFA simulation, and the tagged DFA —
//! and reports MB/s for each, so they can be compared directly. Also checks
//! that every available backend agrees on the match count (a cheap sanity
//! guard); a mismatch is flagged with `!`.
//!
//! All backends are built from the same *unoptimized* IR (the bytecode
//! optimizer isn't applied — PikeVM doesn't support its `Loop1CharBody`
//! form), and the non-ASCII `find` path is used for all four, so this is an
//! apples-to-apples comparison rather than each backend's absolute best.
//!
//! Run with: `cargo run --release --example backend_bench`

use regress::backends::{
    self, BacktrackExecutor, CompiledRegex, Nfa, NfaExecutor, PikeVMExecutor, Tdfa, TdfaExecutor,
};
use std::hint::black_box;
use std::time::{Duration, Instant};

/// Repeat `base` until at least `target` bytes long.
fn repeat_to(base: &str, target: usize) -> String {
    let reps = target / base.len().max(1) + 1;
    base.repeat(reps)
}

/// Median MB/s over `RUNS` runs of `ITERS` scans each. `scan` runs one
/// `find(...).count()` and returns the count (kept alive via `black_box`).
fn throughput(input_len: usize, mut scan: impl FnMut() -> usize) -> f64 {
    const ITERS: usize = 10;
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
    let dt = times[RUNS / 2];
    (input_len * ITERS) as f64 / dt.as_secs_f64() / 1e6
}

fn cell(mbps: Option<f64>) -> String {
    match mbps {
        Some(v) => format!("{:.0}", v),
        None => "-".to_string(),
    }
}

fn main() {
    // Long enough for throughput to be meaningful, small enough to keep the
    // bytecode engines (which can be slow on nested quantifiers) bounded.
    let n = 128 * 1024;
    let digits = repeat_to("0123456789", n);
    let abc = repeat_to("abc", n);
    let aaa = repeat_to("a", n);
    let csv = repeat_to("alpha,bravo,charlie,delta,", n);

    let suite: &[(&str, &str, &str)] = &[
        ("selfloop", r"(?:(\d)(\d)(\d))*", &digits),
        ("alt", r"(?:(a)|(b)|(c))*", &abc),
        ("nullable", r"(a*?)*", &aaa),
        ("csv", r"(?:([^,]*),?)*", &csv),
    ];

    println!(
        "{:<10} {:>10} {:>10} {:>10} {:>10}   (MB/s)",
        "case", "backtrack", "pikevm", "nfa", "tdfa"
    );
    println!("{}", "-".repeat(62));

    for &(name, pattern, input) in suite {
        let flags = regress::Flags::from("");
        let ire = match backends::try_parse(pattern.chars().map(u32::from), flags) {
            Ok(ire) => ire,
            Err(e) => {
                println!("{:<10} parse failed: {:?}", name, e);
                continue;
            }
        };
        let cr: CompiledRegex = backends::emit(&ire);
        let nfa = Nfa::try_from(&ire).ok();
        let tdfa = nfa.as_ref().and_then(|n| Tdfa::try_from(n).ok());

        // Sanity: every available backend should report the same match count.
        let bt_count = backends::find::<BacktrackExecutor>(&cr, input, 0).count();
        let mut agree = true;
        let mut check = |c: usize| {
            if c != bt_count {
                agree = false;
            }
        };
        check(backends::find::<PikeVMExecutor>(&cr, input, 0).count());
        if let Some(n) = &nfa {
            check(backends::find::<NfaExecutor>(n, input, 0).count());
        }
        if let Some(t) = &tdfa {
            check(backends::find::<TdfaExecutor>(t, input, 0).count());
        }

        let bt = throughput(input.len(), || {
            backends::find::<BacktrackExecutor>(&cr, input, 0).count()
        });
        let pike = throughput(input.len(), || {
            backends::find::<PikeVMExecutor>(&cr, input, 0).count()
        });
        let nfa_mbps = nfa
            .as_ref()
            .map(|n| throughput(input.len(), || backends::find::<NfaExecutor>(n, input, 0).count()));
        let tdfa_mbps = tdfa.as_ref().map(|t| {
            throughput(input.len(), || {
                backends::find::<TdfaExecutor>(t, input, 0).count()
            })
        });

        println!(
            "{:<10} {:>10} {:>10} {:>10} {:>10}{}",
            name,
            cell(Some(bt)),
            cell(Some(pike)),
            cell(nfa_mbps),
            cell(tdfa_mbps),
            if agree { "" } else { "   ! counts disagree" },
        );
    }
}
