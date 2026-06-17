//! Baseline measurement harness for the TDFA backend.
//!
//! Prints, per stress pattern, the static size of the built automaton
//! (`TdfaStats`: states, marks, command counts) and — for long-input cases —
//! matching throughput in MB/s. The current automaton uses a naive mark/
//! register strategy (one fresh `InputMark` per write, never reused); these
//! numbers are the baseline that future register-allocation work is measured
//! against.
//!
//! Run with: `cargo run --release --example tdfa_bench`

use regress::backends::{self, Nfa, Tdfa, TdfaExecutor};
use std::hint::black_box;
use std::time::{Duration, Instant};

/// Build the NFA for a pattern, surfacing the first failing stage as a string.
fn build_nfa(pattern: &str) -> Result<Nfa, String> {
    let flags = regress::Flags::from("");
    let ire = backends::try_parse(pattern.chars().map(u32::from), flags)
        .map_err(|e| format!("parse: {:?}", e))?;
    // Unanchored build: the TdfaExecutor does single-pass unanchored search.
    Nfa::try_from_unanchored(&ire).map_err(|e| format!("nfa: {:?}", e))
}

/// Repeat `base` until at least `target` bytes long.
fn repeat_to(base: &str, target: usize) -> String {
    let reps = target / base.len().max(1) + 1;
    base.repeat(reps)
}

/// Median wall-clock time of `runs` executions of `f`.
fn median_time(runs: usize, mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            f();
            start.elapsed()
        })
        .collect();
    times.sort();
    times[times.len() / 2]
}

fn main() {
    // ~512 KiB for the long throughput cases.
    let long = 512 * 1024;
    let digits = repeat_to("0123456789", long);
    let abc = repeat_to("abc", long);
    let aaa = repeat_to("a", long);
    let csv = repeat_to("alpha,bravo,charlie,delta,", long);

    // (name, pattern, input). Long inputs (> 4 KiB) also get a throughput run.
    let suite: &[(&str, &str, &str)] = &[
        ("shift", r"(a*)(a{3,4})", "aaaaaaaa"),
        ("selfloop", r"(?:(\d)(\d)(\d))*", &digits),
        ("unroll", r"((a)(b)){1,16}", "abababababababababababababababab"),
        ("alt", r"(?:(a)|(b)|(c))*", &abc),
        ("nullable", r"(a*?)*", &aaa),
        ("csv", r"(?:([^,]*),?)*", &csv),
    ];

    // ITERS scans per timed run; REPS runs for the median.
    const ITERS: usize = 10;
    const RUNS: usize = 5;

    // Columns show unoptimized -> optimized (try_from_unoptimized vs try_from).
    println!("unoptimized -> optimized:\n");
    println!(
        "{:<10} {:>14} {:>14} {:>14} {:>16}",
        "case", "marks", "cmds", "copies", "MB/s"
    );
    println!("{}", "-".repeat(72));

    // Median MB/s of `find().count()` over `input`, or None for short inputs.
    let throughput = |tdfa: &Tdfa, input: &str| -> Option<f64> {
        if input.len() <= 4096 {
            return None;
        }
        let dt = median_time(RUNS, || {
            let mut n = 0usize;
            for _ in 0..ITERS {
                n += backends::find::<TdfaExecutor>(tdfa, input, 0).count();
            }
            black_box(n);
        });
        Some((input.len() * ITERS) as f64 / dt.as_secs_f64() / 1e6)
    };

    for &(name, pattern, input) in suite {
        let nfa = match build_nfa(pattern) {
            Ok(n) => n,
            Err(e) => {
                println!("{:<10} BUILD FAILED ({})", name, e);
                continue;
            }
        };
        let un = match Tdfa::try_from(&nfa) {
            Ok(t) => t,
            Err(e) => {
                println!("{:<10} tdfa: {:?}", name, e);
                continue;
            }
        };
        let mut opt = un.clone();
        opt.optimize();
        let (su, so) = (un.stats(), opt.stats());

        let speed = match (throughput(&un, input), throughput(&opt, input)) {
            (Some(a), Some(b)) => format!("{:.0}->{:.0}", a, b),
            _ => "-".to_string(),
        };

        println!(
            "{:<10} {:>14} {:>14} {:>14} {:>16}",
            name,
            format!("{}->{}", su.num_marks, so.num_marks),
            format!("{}->{}", su.total_commands, so.total_commands),
            format!("{}->{}", su.copy_commands, so.copy_commands),
            speed,
        );
    }
}
