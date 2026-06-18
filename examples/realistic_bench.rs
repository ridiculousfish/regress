//! Realistic cross-backend throughput benchmark over a real corpus.
//!
//! Unlike `backend_bench.rs` — tiny automata over short, highly repetitive
//! inputs that stay entirely L1-resident — this harness runs the rust `regex`
//! crate's canonical **Sherlock** pattern battery over the full text of *The
//! Adventures of Sherlock Holmes* (~580 KiB, public domain, Project Gutenberg).
//! Varied prose with sparse matches touches many transition rows per scan and
//! spans a wide range of automaton sizes and match densities, so cache/locality
//! effects (the shuffle pool, state layout, a future SIMD `Permute`) actually
//! show up here.
//!
//! Each row reports the built TDFA's size (`states`/`marks`) alongside per-
//! backend throughput in MB/s. The four regress backends extract captures (via
//! `find`); the `regex` column uses `captures_iter` as an apples-to-apples
//! reference (its match semantics differ, so its count isn't cross-checked).
//! Patterns the TDFA can't build (budget/unsupported feature) show `-`.
//!
//! ECMAScript has no inline `(?i)`/`(?s)`/`(?m)` modifiers, so case-insensitive
//! / dotall / multiline are passed via each case's flag string instead.
//!
//! Run with: `cargo run --release --example realistic_bench`

use regress::backends::{
    self, BacktrackExecutor, CompiledRegex, Nfa, NfaExecutor, PikeVMExecutor, Tdfa, TdfaExecutor,
};
use std::collections::HashMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

/// Median MB/s over `RUNS` runs of `ITERS` scans each. `scan` runs one
/// `find(...).count()` and returns the count (kept alive via `black_box`).
fn throughput(input_len: usize, mut scan: impl FnMut() -> usize) -> f64 {
    // The haystack is ~5x larger than `backend_bench`'s, so fewer iters suffice.
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
    let dt = times[RUNS / 2];
    (input_len * ITERS) as f64 / dt.as_secs_f64() / 1e6
}

fn cell(mbps: Option<f64>) -> String {
    match mbps {
        Some(v) => format!("{:.0}", v),
        None => "-".to_string(),
    }
}

/// Build a large capture-free alternation from the corpus's own vocabulary:
/// the `n` most frequent purely-alphabetic words, longest-first within a
/// frequency tie (so the alternation prefers the longer match). This yields a
/// big automaton over real text — the transition-table cache stressor the
/// micro-suite lacks. (Note: even with no capture groups, unanchored full-match
/// tracking across the wide alternation leaves a surprisingly large mark file,
/// so the `marks` column here is worth watching.)
fn dictionary_pattern(haystack: &str, n: usize) -> String {
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for word in haystack.split(|c: char| !c.is_ascii_alphabetic()) {
        if word.len() >= 3 {
            *freq.entry(word).or_insert(0) += 1;
        }
    }
    let mut words: Vec<(&str, usize)> = freq.into_iter().collect();
    // Most frequent first; break ties by longer word, then lexically for
    // determinism.
    words.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.len().cmp(&a.0.len())).then(a.0.cmp(b.0)));
    words.truncate(n);
    // Alphabetic words contain no regex metacharacters, so no escaping needed.
    words
        .into_iter()
        .map(|(w, _)| w)
        .collect::<Vec<_>>()
        .join("|")
}

fn main() {
    // Vendored from the rust `regex` clone (regex-capi/examples/sherlock.txt);
    // public domain. Embedded so the example is self-contained & deterministic.
    let haystack: &str = include_str!("data/sherlock.txt");

    let dict = dictionary_pattern(haystack, 64);

    // (name, pattern, flags). The rust/regex Sherlock battery, with inline
    // modifiers lifted into the flag string (ECMAScript has none).
    let suite: &[(&str, &str, &str)] = &[
        // Literals / names.
        ("name_sherlock", r"Sherlock", ""),
        ("name_holmes", r"Holmes", ""),
        ("name_sherlock_holmes", r"Sherlock Holmes", ""),
        ("name_whitespace", r"Sherlock\s+Holmes", ""),
        // Case-insensitive.
        ("name_sherlock_nocase", r"Sherlock", "i"),
        ("name_holmes_nocase", r"Holmes", "i"),
        ("name_sherlock_holmes_nocase", r"Sherlock Holmes", "i"),
        // Alternations (size ladder).
        ("name_alt1", r"Sherlock|Street", ""),
        ("name_alt2", r"Sherlock|Holmes", ""),
        ("name_alt5", r"Sherlock|Holmes|Watson", ""),
        ("name_alt3", r"Sherlock|Holmes|Watson|Irene|Adler|John|Baker", ""),
        ("name_alt4", r"Sher[a-z]+|Hol[a-z]+", ""),
        ("name_alt4_nocase", r"Sher[a-z]+|Hol[a-z]+", "i"),
        // Common words.
        ("the_lower", r"the", ""),
        ("the_upper", r"The", ""),
        ("the_nocase", r"the", "i"),
        ("the_whitespace", r"the\s+\w+", ""),
        // Scan-heavy.
        ("words", r"\w+", ""),
        ("ing_suffix", r"[a-zA-Z]+ing", ""),
        ("letters", r"\p{L}", "u"),
        ("letters_lower", r"\p{Ll}", "u"),
        ("letters_upper", r"\p{Lu}", "u"),
        ("quotes", "\"[^\"]*\"", ""),
        ("everything_greedy", r".*", ""),
        ("everything_greedy_nl", r".*", "s"),
        // Structured / sparse.
        ("ip", r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}", ""),
        ("before_holmes", r"\w+\s+Holmes", ""),
        ("holmes_cochar_watson", r"Holmes.{0,25}Watson|Watson.{0,25}Holmes", ""),
        ("line_boundary", r"^Sherlock Holmes|Sherlock Holmes$", "m"),
        // Large capture-free automaton (transition-table cache stressor).
        ("dictionary64", dict.as_str(), ""),
    ];

    println!("Sherlock corpus ({} bytes), throughput (MB/s), higher is better:\n", haystack.len());
    println!(
        "{:<28} {:>7} {:>6} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "case", "states", "marks", "backtrack", "pikevm", "nfa", "tdfa", "regex"
    );
    println!("{}", "-".repeat(105));

    for &(name, pattern, flags_str) in suite {
        let flags = regress::Flags::from(flags_str);
        let ire = match backends::try_parse(pattern.chars().map(u32::from), flags) {
            Ok(ire) => ire,
            Err(e) => {
                println!("{:<28} parse failed: {:?}", name, e);
                continue;
            }
        };
        let cr: CompiledRegex = backends::emit(&ire);
        // Single-pass unanchored search (implicit lazy `MatchAny*?` prefix).
        let nfa = Nfa::try_from_unanchored(&ire).ok();
        let tdfa = nfa.as_ref().and_then(|n| Tdfa::try_from(n).ok()).map(|mut t| {
            t.optimize();
            t
        });
        let rx = regex::RegexBuilder::new(pattern)
            .case_insensitive(flags_str.contains('i'))
            .multi_line(flags_str.contains('m'))
            .dot_matches_new_line(flags_str.contains('s'))
            .build()
            .ok();

        // Cross-backend count canary (regress backends should agree).
        let bt_count = backends::find::<BacktrackExecutor>(&cr, haystack, 0).count();
        let mut agree = true;
        let mut check = |c: usize| {
            if c != bt_count {
                agree = false;
            }
        };
        check(backends::find::<PikeVMExecutor>(&cr, haystack, 0).count());
        if let Some(n) = &nfa {
            check(backends::find::<NfaExecutor>(n, haystack, 0).count());
        }
        if let Some(t) = &tdfa {
            check(backends::find::<TdfaExecutor>(t, haystack, 0).count());
        }

        let (states, marks) = match &tdfa {
            Some(t) => {
                let s = t.stats();
                (s.num_states.to_string(), s.num_marks.to_string())
            }
            None => ("-".to_string(), "-".to_string()),
        };

        let bt = throughput(haystack.len(), || {
            backends::find::<BacktrackExecutor>(&cr, haystack, 0).count()
        });
        let pike = throughput(haystack.len(), || {
            backends::find::<PikeVMExecutor>(&cr, haystack, 0).count()
        });
        let nfa_mbps = nfa.as_ref().map(|n| {
            throughput(haystack.len(), || backends::find::<NfaExecutor>(n, haystack, 0).count())
        });
        let tdfa_mbps = tdfa.as_ref().map(|t| {
            throughput(haystack.len(), || {
                backends::find::<TdfaExecutor>(t, haystack, 0).count()
            })
        });
        let rx_mbps = rx.as_ref().map(|r| {
            throughput(haystack.len(), || {
                let mut groups = 0usize;
                for caps in r.captures_iter(haystack) {
                    groups += caps.len();
                }
                groups
            })
        });

        println!(
            "{:<28} {:>7} {:>6} {:>10} {:>10} {:>10} {:>10} {:>10}{}",
            name,
            states,
            marks,
            cell(Some(bt)),
            cell(Some(pike)),
            cell(nfa_mbps),
            cell(tdfa_mbps),
            cell(rx_mbps),
            if agree { "" } else { "   ! counts disagree" },
        );
    }
}
