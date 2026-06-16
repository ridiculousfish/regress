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

/// Build a TDFA from a pattern, surfacing the first failing stage as a string.
fn build(pattern: &str) -> Result<Tdfa, String> {
    let flags = regress::Flags::from("");
    let ire = backends::try_parse(pattern.chars().map(u32::from), flags)
        .map_err(|e| format!("parse: {:?}", e))?;
    let nfa = Nfa::try_from(&ire).map_err(|e| format!("nfa: {:?}", e))?;
    Tdfa::try_from(&nfa).map_err(|e| format!("tdfa: {:?}", e))
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

    println!(
        "{:<10} {:>6} {:>7} {:>7} {:>7} {:>9}  {:>9}",
        "case", "states", "marks", "cmds", "copies", "currpos", "MB/s"
    );
    println!("{}", "-".repeat(64));

    for &(name, pattern, input) in suite {
        let tdfa = match build(pattern) {
            Ok(t) => t,
            Err(e) => {
                println!("{:<10} BUILD FAILED ({})", name, e);
                continue;
            }
        };
        let s = tdfa.stats();

        // Only time meaningfully-long inputs; short ones are timing noise.
        let mbps = if input.len() > 4096 {
            let dt = median_time(RUNS, || {
                let mut n = 0usize;
                for _ in 0..ITERS {
                    n += backends::find::<TdfaExecutor>(&tdfa, input, 0).count();
                }
                black_box(n);
            });
            let bytes = (input.len() * ITERS) as f64;
            format!("{:.0}", bytes / dt.as_secs_f64() / 1e6)
        } else {
            "-".to_string()
        };

        println!(
            "{:<10} {:>6} {:>7} {:>7} {:>7} {:>9}  {:>9}",
            name,
            s.num_states,
            s.num_marks,
            s.total_commands,
            s.copy_commands,
            s.currentpos_commands,
            mbps,
        );
    }
}
