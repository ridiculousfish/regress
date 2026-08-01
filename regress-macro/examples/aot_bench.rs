//! AoT vs runtime-engine throughput over the rust `regex` crate's canonical
//! **Sherlock** pattern battery (same corpus and cases as `regress`'s own
//! `examples/realistic_bench.rs`) — but with an added `aot` column, since
//! `regex!` is a proc-macro that needs a pattern literal at expansion time and
//! so can't be driven from that example's runtime data table. Living here
//! (rather than in `regress/examples/`) is what makes that possible: this
//! crate already depends on `regress` with the `codegen` feature, so nothing
//! circular is needed. Building the whole suite as one macro invocation per
//! case, via a small `bench_case!` + `suite!` pair, keeps the case list
//! itself readable as a plain table while still giving `regex!` a literal at
//! each site.
//!
//! Two cases from the sibling suite can't appear here: `line_boundary`
//! (multiline `^`/`$` are per-byte guards, not yet supported ahead of time —
//! a compile error, not a runtime fallback) and `dictionary64` (built at
//! runtime from the corpus, so it's not a literal `regex!` can consume at
//! all). Both are called out explicitly below rather than silently dropped.
//!
//! The `aot` column's suffix marks which of `compile_to_rust`'s two emission
//! tiers ran (see `src/automata/tdfa/rustgen/mod.rs`): unmarked is the
//! unrolled tier (the common case), `t` is the table tier (past
//! `CODEGEN_UNROLL_MAX_STATES`), `L` is the literal-only tier (no verify
//! automaton at all — the prefilter span IS the match).
//!
//! In the "Capturing groups" table, `tdfa`/`tdfa-jit` measure match throughput
//! via the zero-allocation `TdfaMatch` accessor (captures borrowed from a
//! reused scratch buffer), not the public `Match`-producing `find_iter`. The
//! `aot` column matches that via `CompiledMatcher::find_iter_raw` +
//! `CompiledMatch` (the AoT sibling of `TdfaMatch`) rather than `find_iter`,
//! so all three measure matching cost alone — not `Match`/`group_names`
//! construction, which otherwise dominates on capture-heavy patterns (`Match`
//! deep-clones `group_names` — one `Box<str>` allocation per group — on every
//! match; `backtrack`/`pikevm`/`nfa` pay this same tax too, so it isn't an
//! AoT-specific inefficiency, just not what this table means to measure).
//!
//! Run with: `cargo run --release -p regress-macro --example aot_bench`
//! Add `--features tdfa-jit` for the native-code TDFA column too.

use regress::__codegen::MatcherTier;
use regress::backends::{
    self, BacktrackExecutor, CompiledRegex, Nfa, NfaExecutor, PikeVMExecutor, TdfaExecutor,
    TdfaMatches, TdfaProgram,
};
#[cfg(feature = "tdfa-jit")]
use regress::backends::{TdfaJitExecutor, TdfaJitProgram};
use regress_macro::regex;
use std::hint::black_box;
use std::time::{Duration, Instant};

const SHERLOCK: &str = include_str!("../../examples/data/sherlock.txt");

/// Median MB/s over `RUNS` runs of `ITERS` scans each.
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

fn cell(mbps: Option<f64>) -> String {
    match mbps {
        Some(v) => format!("{:.0}", v),
        None => "-".to_string(),
    }
}

/// The `aot` column: throughput plus a one-letter tier suffix when the tier
/// isn't the default (unrolled) one — `t` (table) or `L` (literal-only).
fn aot_cell(mbps: f64, tier: MatcherTier) -> String {
    let suffix = match tier {
        MatcherTier::Unrolled => "",
        MatcherTier::Table => "t",
        MatcherTier::Literal => "L",
    };
    format!("{:.0}{}", mbps, suffix)
}

/// The `tdfa-jit` column header segment, or empty when the feature is off.
fn jit_header_seg() -> String {
    #[cfg(feature = "tdfa-jit")]
    {
        format!(" {:>10}", "tdfa-jit")
    }
    #[cfg(not(feature = "tdfa-jit"))]
    {
        String::new()
    }
}

fn rule_width() -> usize {
    116 + jit_header_seg().len()
}

/// A trailing `*` marks that native code actually ran (vs. an interpreter
/// fallback for a pattern outside the JIT's supported tier).
#[cfg(feature = "tdfa-jit")]
fn jit_cell(mbps: Option<f64>, active: bool) -> String {
    match mbps {
        Some(v) if active => format!("{:.0}*", v),
        Some(v) => format!("{:.0}", v),
        None => "-".to_string(),
    }
}

/// A checksum over a match that forces capture extraction, matching
/// `realistic_bench.rs`'s `cap_sum`/`tdfa_cap_sum`.
fn cap_sum(m: &regress::Match) -> usize {
    let mut s = m.range.end;
    for c in &m.captures {
        if let Some(r) = c {
            s = s.wrapping_add(r.end);
        }
    }
    s
}

fn tdfa_cap_sum(m: &regress::backends::TdfaMatch<'_>) -> usize {
    let mut s = m.range.end;
    for i in 0..m.num_captures() {
        if let Some(r) = m.capture(i) {
            s = s.wrapping_add(r.end);
        }
    }
    s
}

/// Lean capture checksum over a [`regress::__codegen::CompiledMatch`] — the
/// `aot` column's equivalent of `tdfa_cap_sum`, so its "Capturing groups"
/// throughput measures the same thing `tdfa`/`tdfa-jit` do: matching cost
/// alone, not `Match`/`group_names` construction (see `find_iter_raw`'s doc
/// comment in `codegen_rt.rs` for why that distinction matters here).
fn aot_cap_sum(m: &regress::__codegen::CompiledMatch<'_>) -> usize {
    let mut s = m.range.end;
    for i in 0..m.num_captures() {
        if let Some(r) = m.capture(i) {
            s = s.wrapping_add(r.end);
        }
    }
    s
}

/// One row: build every backend, cross-check match counts against the
/// backtracker oracle, print throughput. `$pattern`/`$flags` must be literals
/// (they feed `regex!` directly), so this is a `macro_rules!` rather than a
/// function — see the module doc for why the whole suite is expressed this
/// way instead of a runtime data table.
macro_rules! bench_case {
    ($name:literal, $pattern:literal, $flags:literal) => {{
        let flags = regress::Flags::from($flags);
        let mut ire = backends::try_parse($pattern.chars().map(u32::from), flags).expect("parse");
        backends::optimize(&mut ire);
        let cr: CompiledRegex = backends::emit(&ire);
        let nfa = Nfa::try_from_unanchored(&ire).ok();
        let tdfa = TdfaProgram::try_from_ir(&ire).ok();
        let rx = regex::RegexBuilder::new($pattern)
            .case_insensitive($flags.contains('i'))
            .multi_line($flags.contains('m'))
            .dot_matches_new_line($flags.contains('s'))
            .build()
            .ok();
        let aot = regex!($pattern, $flags);

        let bt_count = backends::find::<BacktrackExecutor>(&cr, SHERLOCK, 0).count();
        let mut agree = true;
        let mut check = |c: usize| {
            if c != bt_count {
                agree = false;
            }
        };
        check(backends::find::<PikeVMExecutor>(&cr, SHERLOCK, 0).count());
        if let Some(n) = &nfa {
            check(backends::find::<NfaExecutor>(n, SHERLOCK, 0).count());
        }
        if let Some(t) = &tdfa {
            check(backends::find::<TdfaExecutor>(t, SHERLOCK, 0).count());
        }
        check(aot.find_iter(SHERLOCK).count());

        let (states, marks) = match &tdfa {
            Some(t) => {
                let s = t.stats();
                (s.num_states.to_string(), s.num_marks.to_string())
            }
            None => ("-".to_string(), "-".to_string()),
        };

        let bt = throughput(SHERLOCK.len(), || {
            backends::find::<BacktrackExecutor>(&cr, SHERLOCK, 0).count()
        });
        let pike = throughput(SHERLOCK.len(), || {
            backends::find::<PikeVMExecutor>(&cr, SHERLOCK, 0).count()
        });
        let nfa_mbps = nfa.as_ref().map(|n| {
            throughput(SHERLOCK.len(), || backends::find::<NfaExecutor>(n, SHERLOCK, 0).count())
        });
        let tdfa_mbps = tdfa.as_ref().map(|t| {
            throughput(SHERLOCK.len(), || {
                backends::find::<TdfaExecutor>(t, SHERLOCK, 0).count()
            })
        });
        let aot_mbps = throughput(SHERLOCK.len(), || aot.find_iter(SHERLOCK).count());
        let rx_mbps = rx.as_ref().map(|r| {
            throughput(SHERLOCK.len(), || r.find_iter(SHERLOCK).count())
        });

        #[cfg(feature = "tdfa-jit")]
        let jit_seg = {
            let prog = TdfaJitProgram::try_from_ir(&ire).ok();
            if let Some(p) = &prog {
                check(backends::find::<TdfaJitExecutor>(p, SHERLOCK, 0).count());
            }
            let mbps = prog.as_ref().map(|p| {
                throughput(SHERLOCK.len(), || {
                    backends::find::<TdfaJitExecutor>(p, SHERLOCK, 0).count()
                })
            });
            let active = prog.as_ref().is_some_and(|p| p.jit_active());
            format!(" {:>10}", jit_cell(mbps, active))
        };
        #[cfg(not(feature = "tdfa-jit"))]
        let jit_seg = String::new();

        println!(
            "{:<28} {:>7} {:>6} {:>10} {:>10} {:>10} {:>10}{} {:>11} {:>10}{}",
            $name,
            states,
            marks,
            cell(Some(bt)),
            cell(Some(pike)),
            cell(nfa_mbps),
            cell(tdfa_mbps),
            jit_seg,
            aot_cell(aot_mbps, aot.tier()),
            cell(rx_mbps),
            if agree { "" } else { "   ! counts disagree" },
        );
    }};
}

/// Same as `bench_case!`, but every backend materializes capture groups per
/// match (`cap_sum`/`tdfa_cap_sum`/`aot_cap_sum`) instead of just counting —
/// mirrors `realistic_bench.rs`'s `capture_showcase`.
macro_rules! bench_case_captures {
    ($name:literal, $pattern:literal, $flags:literal) => {{
        let flags = regress::Flags::from($flags);
        let mut ire = backends::try_parse($pattern.chars().map(u32::from), flags).expect("parse");
        backends::optimize(&mut ire);
        let cr: CompiledRegex = backends::emit(&ire);
        let nfa = Nfa::try_from_unanchored(&ire).ok();
        let tdfa = TdfaProgram::try_from_ir(&ire).ok();
        let rx = regex::RegexBuilder::new($pattern)
            .case_insensitive($flags.contains('i'))
            .multi_line($flags.contains('m'))
            .dot_matches_new_line($flags.contains('s'))
            .build()
            .ok();
        let aot = regex!($pattern, $flags);

        let bt_sum: usize = backends::find::<BacktrackExecutor>(&cr, SHERLOCK, 0)
            .map(|m| cap_sum(&m))
            .sum();
        let mut agree = true;
        let mut check = |c: usize| agree &= c == bt_sum;
        check(backends::find::<PikeVMExecutor>(&cr, SHERLOCK, 0).map(|m| cap_sum(&m)).sum());
        if let Some(n) = &nfa {
            check(backends::find::<NfaExecutor>(n, SHERLOCK, 0).map(|m| cap_sum(&m)).sum());
        }
        if let Some(t) = &tdfa {
            check(backends::find::<TdfaExecutor>(t, SHERLOCK, 0).map(|m| cap_sum(&m)).sum());
        }
        check({
            let mut it = aot.find_iter_raw(SHERLOCK);
            let mut total = 0usize;
            while let Some(m) = it.next() {
                total = total.wrapping_add(aot_cap_sum(&m));
            }
            total
        });

        let (states, marks) = match &tdfa {
            Some(t) => {
                let s = t.stats();
                (s.num_states.to_string(), s.num_marks.to_string())
            }
            None => ("-".to_string(), "-".to_string()),
        };

        let bt = throughput(SHERLOCK.len(), || {
            backends::find::<BacktrackExecutor>(&cr, SHERLOCK, 0).map(|m| cap_sum(&m)).sum()
        });
        let pike = throughput(SHERLOCK.len(), || {
            backends::find::<PikeVMExecutor>(&cr, SHERLOCK, 0).map(|m| cap_sum(&m)).sum()
        });
        let nfa_mbps = nfa.as_ref().map(|n| {
            throughput(SHERLOCK.len(), || {
                backends::find::<NfaExecutor>(n, SHERLOCK, 0).map(|m| cap_sum(&m)).sum()
            })
        });
        let tdfa_mbps = tdfa.as_ref().map(|t| {
            throughput(SHERLOCK.len(), || {
                let mut it = TdfaMatches::new(t, SHERLOCK, 0);
                let mut total = 0usize;
                while let Some(m) = it.next() {
                    total = total.wrapping_add(tdfa_cap_sum(&m));
                }
                total
            })
        });
        let aot_mbps = throughput(SHERLOCK.len(), || {
            let mut it = aot.find_iter_raw(SHERLOCK);
            let mut total = 0usize;
            while let Some(m) = it.next() {
                total = total.wrapping_add(aot_cap_sum(&m));
            }
            total
        });
        let rx_mbps = rx.as_ref().map(|r| {
            throughput(SHERLOCK.len(), || {
                let mut total = 0usize;
                for caps in r.captures_iter(SHERLOCK) {
                    for i in 0..caps.len() {
                        if let Some(g) = caps.get(i) {
                            total = total.wrapping_add(g.end());
                        }
                    }
                }
                total
            })
        });

        #[cfg(feature = "tdfa-jit")]
        let jit_seg = {
            let prog = TdfaJitProgram::try_from_ir(&ire).ok();
            if let Some(p) = &prog {
                check(
                    backends::find::<TdfaJitExecutor>(p, SHERLOCK, 0).map(|m| cap_sum(&m)).sum(),
                );
            }
            let mbps = prog.as_ref().map(|p| {
                throughput(SHERLOCK.len(), || {
                    let mut it = TdfaMatches::new(&**p, SHERLOCK, 0);
                    let mut total = 0usize;
                    while let Some(m) = it.next() {
                        total = total.wrapping_add(tdfa_cap_sum(&m));
                    }
                    total
                })
            });
            let active = prog.as_ref().is_some_and(|p| p.jit_active());
            format!(" {:>10}", jit_cell(mbps, active))
        };
        #[cfg(not(feature = "tdfa-jit"))]
        let jit_seg = String::new();

        println!(
            "{:<28} {:>7} {:>6} {:>10} {:>10} {:>10} {:>10}{} {:>11} {:>10}{}",
            $name,
            states,
            marks,
            cell(Some(bt)),
            cell(Some(pike)),
            cell(nfa_mbps),
            cell(tdfa_mbps),
            jit_seg,
            aot_cell(aot_mbps, aot.tier()),
            cell(rx_mbps),
            if agree { "" } else { "   ! capture checksums disagree" },
        );
    }};
}

fn print_header(title: &str) {
    println!("{}", title);
    println!(
        "{:<28} {:>7} {:>6} {:>10} {:>10} {:>10} {:>10}{} {:>11} {:>10}",
        "case", "states", "marks", "backtrack", "pikevm", "nfa", "tdfa", jit_header_seg(), "aot", "regex"
    );
    println!("{}", "-".repeat(rule_width()));
}

fn main() {
    print_header(&format!("Sherlock corpus ({} bytes), throughput (MB/s), higher is better:\n", SHERLOCK.len()));

    // Literals / names.
    bench_case!("name_sherlock", "Sherlock", "");
    bench_case!("name_holmes", "Holmes", "");
    bench_case!("name_sherlock_holmes", "Sherlock Holmes", "");
    bench_case!("name_whitespace", r"Sherlock\s+Holmes", "");
    // Case-insensitive.
    bench_case!("name_sherlock_nocase", "Sherlock", "i");
    bench_case!("name_holmes_nocase", "Holmes", "i");
    bench_case!("name_sherlock_holmes_nocase", "Sherlock Holmes", "i");
    // Alternations (size ladder).
    bench_case!("name_alt1", "Sherlock|Street", "");
    bench_case!("name_alt2", "Sherlock|Holmes", "");
    bench_case!("name_alt5", "Sherlock|Holmes|Watson", "");
    bench_case!("name_alt3", "Sherlock|Holmes|Watson|Irene|Adler|John|Baker", "");
    bench_case!("name_alt4", r"Sher[a-z]+|Hol[a-z]+", "");
    bench_case!("name_alt4_nocase", r"Sher[a-z]+|Hol[a-z]+", "i");
    // Common words.
    bench_case!("the_lower", "the", "");
    bench_case!("the_upper", "The", "");
    bench_case!("the_nocase", "the", "i");
    bench_case!("the_whitespace", r"the\s+\w+", "");
    bench_case!("there_nocase", "there", "i");
    // Scan-heavy.
    bench_case!("words", r"\w+", "");
    bench_case!("ing_suffix", "[a-zA-Z]+ing", "");
    bench_case!("letters", r"\p{L}", "u");
    bench_case!("letters_lower", r"\p{Ll}", "u");
    bench_case!("letters_upper", r"\p{Lu}", "u");
    bench_case!("quotes", "\"[^\"]*\"", "");
    bench_case!("everything_greedy", ".*", "");
    bench_case!("everything_greedy_nl", ".*", "s");
    // Structured / sparse.
    bench_case!("ip", r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}", "");
    bench_case!("before_holmes", r"\w+\s+Holmes", "");
    bench_case!("holmes_cochar_watson", r"Holmes.{0,25}Watson|Watson.{0,25}Holmes", "");
    // `line_boundary` (`^Sherlock Holmes|Sherlock Holmes$`, flag "m") is
    // skipped: multiline anchors are per-byte guards, not yet supported by
    // the ahead-of-time compiler (a compile error, so it can't be a row in
    // this macro-driven suite the way the other engines' "-" cells are).
    bench_case!("end_anchor", "Holmes$", "");
    bench_case!("end_anchor_alt", "Holmes$|Watson$", "");
    // `dictionary64` is skipped too: it's built at runtime from the corpus's
    // own vocabulary (see `realistic_bench.rs::dictionary_pattern`), so it's
    // not a literal `regex!` can consume — there's no way to give it an
    // `aot` column in a suite expressed this way.

    println!();
    print_header("Capturing groups — every backend materializes capture groups per match (MB/s):\n");

    bench_case_captures!("cap_words1", r"(\w+)", "");
    bench_case_captures!("cap_words2", r"(\w+)\s+(\w+)", "");
    bench_case_captures!("cap_words3", r"(\w+)\s+(\w+)\s+(\w+)", "");
    bench_case_captures!("cap_words4", r"(\w+)\s+(\w+)\s+(\w+)\s+(\w+)", "");
    bench_case_captures!("cap_first_rest", r"(\w)(\w*)", "");
    bench_case_captures!("cap_nested", r"((\w+)\s+(\w+))", "");
    bench_case_captures!("cap_separators", r"(\w+)(\s+)(\w+)", "");
    bench_case_captures!("cap_optional", r"(\w+)(\s+\w+)?", "");
    bench_case_captures!("cap_five", r"(\w)(\w)(\w)(\w)(\w)", "");
    bench_case_captures!("cap_quant_group", r"(\w+\s*)+", "");
    bench_case_captures!("cap_quoted", "\"([^\"]*)\"", "");
    bench_case_captures!("cap_contraction", r"(\w+)'(\w+)", "");
}
