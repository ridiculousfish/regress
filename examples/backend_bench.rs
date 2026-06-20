//! Cross-backend matching-throughput benchmark.
//!
//! Runs the same stress suite through all four regress backends — classical
//! backtracking, PikeVM, the Thompson NFA simulation, and the tagged DFA — plus
//! the upstream `regex` crate as a reference column, and reports MB/s for each.
//!
//! Every column extracts capture groups, so the comparison is apples-to-apples:
//! the regress backends iterate `find` (which produces full `Match`es with
//! captures), and the `regex` column uses `captures_iter` (not `find_iter`,
//! which would skip capture extraction and overstate `regex` by ~10x).
//!
//! The four regress backends are built from the same *unoptimized* IR (the
//! bytecode optimizer isn't applied — PikeVM doesn't support its `Loop1CharBody`
//! form) via the non-ASCII `find` path (apples-to-apples within regress); the
//! TDFA is `optimize()`d. The `regex` column is the upstream crate in its
//! default configuration, shown as a reference; its match semantics differ, so
//! its count isn't cross-checked.
//!
//! The suite spans ASCII as well as harder UTF-8 paths — non-BMP (4-byte)
//! codepoints (astral `.`, astral/CJK character classes, non-BMP literal
//! alternations) and word boundaries in non-ASCII context (`\b` next to
//! multi-byte non-word chars, plus the `iu` `ſ`/Kelvin folds). Each case carries
//! its own ECMAScript flag string (e.g. `u`, `iu`).
//!
//! `regex` is a dev-dependency pointing at a local clone outside the repo.
//!
//! Run with: `cargo run --release --example backend_bench`

use regress::backends::{
    self, BacktrackExecutor, CompiledRegex, Nfa, NfaExecutor, PikeVMExecutor, Tdfa, TdfaExecutor,
    TdfaProgram,
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
    // Non-BMP (4-byte) and other multi-byte inputs.
    let emoji_mix = repeat_to("😀🎉🚀🦊", n);
    let emoji_run = repeat_to("😀😁😂😃😄", n);
    let emoji_alt = repeat_to("😀🎉🚀", n);
    let cjk = repeat_to("世界你好语言", n);
    // `·` = U+00B7 (2-byte non-word), `世界` = 3-byte non-word: word boundaries
    // here straddle multi-byte codepoints.
    let wb_ctx = repeat_to("hello·world世界 ", n);
    // U+017F `ſ` and U+212A Kelvin `K` are the only non-ASCII codepoints that
    // count as word chars under the `iu` folds; `·` separates them so each `\b`
    // fires.
    let wb_fold = repeat_to("·ſ·\u{212A}", n);

    let suite: &[(&str, &str, &str, &str)] = &[
        ("selfloop", r"(?:(\d)(\d)(\d))*", "", &digits),
        ("alt", r"(?:(a)|(b)|(c))*", "", &abc),
        ("nullable", r"(a*?)*", "", &aaa),
        ("csv", r"(?:([^,]*),?)*", "", &csv),
        ("astral-dot", r"(?:(.)(.))*", "u", &emoji_mix),
        ("astral-cls", r"[\u{1F600}-\u{1F64F}]+", "u", &emoji_run),
        ("astral-alt", r"(?:(\u{1F600})|(\u{1F389})|(\u{1F680}))+", "u", &emoji_alt),
        ("cjk-cls", r"([\u{4E00}-\u{9FFF}])+", "u", &cjk),
        ("wb-context", r"\b[a-z]+\b", "", &wb_ctx),
        ("wb-fold", r"(?:\b\u{017F}|\b\u{212A})", "iu", &wb_fold),
    ];

    println!("throughput (MB/s), higher is better:\n");
    println!(
        "{:<10} {:>11} {:>11} {:>11} {:>11} {:>11}",
        "case", "backtrack", "pikevm", "nfa", "tdfa", "regex"
    );
    println!("{}", "-".repeat(70));

    for &(name, pattern, flags_str, input) in suite {
        let flags = regress::Flags::from(flags_str);
        let ire = match backends::try_parse(pattern.chars().map(u32::from), flags) {
            Ok(ire) => ire,
            Err(e) => {
                println!("{:<10} parse failed: {:?}", name, e);
                continue;
            }
        };
        let cr: CompiledRegex = backends::emit(&ire);
        // Executors do single-pass unanchored search; build the unanchored
        // automaton (implicit lazy `MatchAny*?` prefix).
        let nfa = Nfa::try_from_unanchored(&ire).ok();
        let tdfa = nfa.as_ref().and_then(|n| Tdfa::try_from(n).ok()).map(|mut t| {
            t.optimize();
            // Plain scan program (no prefilter): this suite measures the
            // automaton in isolation over tiny L1-resident inputs.
            TdfaProgram::scan(t)
        });
        // The upstream `regex` crate rejects some empty-repeat patterns; if so,
        // its column is just "-". Its count isn't cross-checked (different
        // match semantics). Honor the case-insensitive / multiline / dotall
        // flags; `u` has no toggle (the crate is Unicode-aware by default).
        let rx = regex::RegexBuilder::new(pattern)
            .case_insensitive(flags_str.contains('i'))
            .multi_line(flags_str.contains('m'))
            .dot_matches_new_line(flags_str.contains('s'))
            .build()
            .ok();

        // Sanity: every regress backend should report the same match count.
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
        let rx_mbps = rx.as_ref().map(|r| {
            throughput(input.len(), || {
                // Extract capture groups, matching the regress backends: their
                // `find` produces full `Match`es with captures, so `captures_iter`
                // (not `find_iter`) is the apples-to-apples comparison.
                let mut groups = 0usize;
                for caps in r.captures_iter(input) {
                    groups += caps.len();
                }
                groups
            })
        });

        println!(
            "{:<10} {:>11} {:>11} {:>11} {:>11} {:>11}{}",
            name,
            cell(Some(bt)),
            cell(Some(pike)),
            cell(nfa_mbps),
            cell(tdfa_mbps),
            cell(rx_mbps),
            if agree { "" } else { "   ! counts disagree" },
        );
    }
}
