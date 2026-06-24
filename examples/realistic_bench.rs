//! Realistic cross-backend throughput benchmark over a real corpus.
//!
//! Unlike `backend_bench.rs` — tiny automata over short, highly repetitive
//! inputs that stay entirely L1-resident — this harness runs the rust `regex`
//! crate's canonical **Sherlock** pattern battery over the full text of *The
//! Adventures of Sherlock Holmes* (~580 KiB, public domain, Project Gutenberg).
//! Varied prose with sparse matches touches many transition rows per scan and
//! spans a wide range of automaton sizes and match densities, so executor and
//! cache/locality effects actually show up here (unlike `backend_bench`'s tiny,
//! fully L1-resident cases).
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
    self, BacktrackExecutor, CompiledRegex, Nfa, NfaExecutor, PikeVMExecutor, TdfaExecutor,
    TdfaProgram,
};
#[cfg(feature = "tdfa-jit")]
use regress::backends::{TdfaJitExecutor, TdfaJitProgram};
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

/// The `tdfa-jit` column header segment (`" {:>10}"`), or empty when the feature
/// is off. Keeps the table layout identical when the JIT isn't compiled in.
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

/// Width of the separator rule, widened for the extra JIT column when present.
fn rule_width() -> usize {
    105 + jit_header_seg().len()
}

/// Format a `tdfa-jit` cell. A trailing `*` marks that native code actually ran
/// (vs. an interpreter fallback for a pattern outside the JIT's supported tier).
#[cfg(feature = "tdfa-jit")]
fn jit_cell(mbps: Option<f64>, active: bool) -> String {
    match mbps {
        Some(v) if active => format!("{:.0}*", v),
        Some(v) => format!("{:.0}", v),
        None => "-".to_string(),
    }
}

/// A checksum over a match that *forces capture extraction*: the full-match end
/// plus every capture group's end offset. Summed over all matches it is the
/// per-backend workload the capture-showcase loop times, and — since every
/// regress backend shares ECMAScript capture semantics and the `Some(0..0)`
/// unmatched convention — a value the cross-backend canary compares against the
/// backtracker oracle (captures, not just match counts).
fn cap_sum(m: &regress::Match) -> usize {
    let mut s = m.range.end;
    for c in &m.captures {
        if let Some(r) = c {
            s = s.wrapping_add(r.end);
        }
    }
    s
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

    // Optional CLI filters: run only cases whose name contains any given
    // substring, e.g. `cargo run --release --example realistic_bench -- everything_greedy_nl`.
    let filters: Vec<String> = std::env::args().skip(1).collect();
    let selected = |name: &str| filters.is_empty() || filters.iter().any(|f| name.contains(f));

    println!("Sherlock corpus ({} bytes), throughput (MB/s), higher is better:\n", haystack.len());
    println!(
        "{:<28} {:>7} {:>6} {:>10} {:>10} {:>10} {:>10}{} {:>10}",
        "case", "states", "marks", "backtrack", "pikevm", "nfa", "tdfa", jit_header_seg(), "regex"
    );
    println!("{}", "-".repeat(rule_width()));

    for &(name, pattern, flags_str) in suite {
        if !selected(name) {
            continue;
        }
        let flags = regress::Flags::from(flags_str);
        let mut ire = match backends::try_parse(pattern.chars().map(u32::from), flags) {
            Ok(ire) => ire,
            Err(e) => {
                println!("{:<28} parse failed: {:?}", name, e);
                continue;
            }
        };
        // Optimize the IR once (lowers literal runs to `ByteSequence`, etc.) so
        // every backend — and the TDFA prefilter in particular — sees literals.
        backends::optimize(&mut ire);
        let cr: CompiledRegex = backends::emit(&ire);
        // Single-pass unanchored search (implicit lazy `MatchAny*?` prefix).
        let nfa = Nfa::try_from_unanchored(&ire).ok();
        // The TDFA column drives the automaton via a `TdfaProgram`, which uses a
        // literal prefilter (memchr/memmem to candidates + anchored verify) when
        // the match must begin with a literal, else the plain unanchored scan.
        let tdfa = TdfaProgram::try_from_ir(&ire).ok();
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

        // tdfa-jit column (native-code TDFA where the pattern is in a supported
        // tier; interpreter fallback otherwise). `check` also verifies its count.
        #[cfg(feature = "tdfa-jit")]
        let jit_seg = {
            let prog = TdfaJitProgram::try_from_ir(&ire).ok();
            if let Some(p) = &prog {
                check(backends::find::<TdfaJitExecutor>(p, haystack, 0).count());
            }
            let mbps = prog.as_ref().map(|p| {
                throughput(haystack.len(), || {
                    backends::find::<TdfaJitExecutor>(p, haystack, 0).count()
                })
            });
            let active = prog.as_ref().is_some_and(|p| p.jit_active());
            format!(" {:>10}", jit_cell(mbps, active))
        };
        #[cfg(not(feature = "tdfa-jit"))]
        let jit_seg = String::new();

        println!(
            "{:<28} {:>7} {:>6} {:>10} {:>10} {:>10} {:>10}{} {:>10}{}",
            name,
            states,
            marks,
            cell(Some(bt)),
            cell(Some(pike)),
            cell(nfa_mbps),
            cell(tdfa_mbps),
            jit_seg,
            cell(rx_mbps),
            if agree { "" } else { "   ! counts disagree" },
        );
    }

    capture_showcase(haystack, &selected);
}

/// Capture-group showcase. Same corpus and column layout as the main table, but
/// every backend *materializes the capture groups* of every match (timed via
/// `cap_sum`) rather than just counting — the workload where a tagged DFA pulls
/// ahead of the thread-tracking PikeVM (and the regex crate's capture engines):
/// the bytecode engines carry a capture-slot set per live thread, while the TDFA
/// resolves all groups in one linear pass (its per-match cost shows up only as a
/// bigger `marks` file, not slower scanning). The canary now checks the capture
/// checksum, so it verifies the TDFA's *captures* against the backtracker oracle.
fn capture_showcase(haystack: &str, selected: &impl Fn(&str) -> bool) {
    let suite: &[(&str, &str, &str)] = &[
        // Capture-count ladder over word runs: no usable literal prefilter, so
        // every engine must scan the whole corpus and the only growing cost is
        // capture bookkeeping. The TDFA stays flat as groups are added while the
        // PikeVM degrades per added slot; watch the `marks` column climb.
        ("cap_words1", r"(\w+)", ""),
        ("cap_words2", r"(\w+)\s+(\w+)", ""),
        ("cap_words3", r"(\w+)\s+(\w+)\s+(\w+)", ""),
        ("cap_words4", r"(\w+)\s+(\w+)\s+(\w+)\s+(\w+)", ""),
        // Other many-match capture patterns.
        ("cap_first_rest", r"(\w)(\w*)", ""),
        // Capture-shape varieties.
        ("cap_nested", r"((\w+)\s+(\w+))", ""), // nested groups
        ("cap_separators", r"(\w+)(\s+)(\w+)", ""), // capture the whitespace too
        ("cap_optional", r"(\w+)(\s+\w+)?", ""), // 2nd group is None at line ends
        ("cap_five", r"(\w)(\w)(\w)(\w)(\w)", ""), // many small groups
        ("cap_quant_group", r"(\w+\s*)+", ""),  // quantified group: last iteration wins
        // Capturing variant of the capture-free `quotes` row, to isolate the
        // cost of materializing the inner group on a content-heavy match.
        ("cap_quoted", "\"([^\"]*)\"", ""),
        // Honest counterpoint: when a rare literal exists the regex crate's
        // prefilter (here a memmem on `'`) skips the scan the TDFA still does.
        ("cap_contraction", r"(\w+)'(\w+)", ""),
    ];

    println!("\nCapturing groups — every backend materializes capture groups per match (MB/s):\n");
    println!(
        "{:<28} {:>7} {:>6} {:>10} {:>10} {:>10} {:>10}{} {:>10}",
        "case", "states", "marks", "backtrack", "pikevm", "nfa", "tdfa", jit_header_seg(), "regex"
    );
    println!("{}", "-".repeat(rule_width()));

    for &(name, pattern, flags_str) in suite {
        if !selected(name) {
            continue;
        }
        let flags = regress::Flags::from(flags_str);
        let mut ire = match backends::try_parse(pattern.chars().map(u32::from), flags) {
            Ok(ire) => ire,
            Err(e) => {
                println!("{:<28} parse failed: {:?}", name, e);
                continue;
            }
        };
        backends::optimize(&mut ire);
        let cr: CompiledRegex = backends::emit(&ire);
        let nfa = Nfa::try_from_unanchored(&ire).ok();
        let tdfa = TdfaProgram::try_from_ir(&ire).ok();
        let rx = regex::RegexBuilder::new(pattern)
            .case_insensitive(flags_str.contains('i'))
            .multi_line(flags_str.contains('m'))
            .dot_matches_new_line(flags_str.contains('s'))
            .build()
            .ok();

        // Capture canary: checksum over matches AND their group offsets, so the
        // TDFA's captures (not just counts) are verified against the backtracker.
        let bt_sum: usize = backends::find::<BacktrackExecutor>(&cr, haystack, 0)
            .map(|m| cap_sum(&m))
            .sum();
        let mut agree = true;
        let mut check = |c: usize| agree &= c == bt_sum;
        check(backends::find::<PikeVMExecutor>(&cr, haystack, 0).map(|m| cap_sum(&m)).sum());
        if let Some(n) = &nfa {
            check(backends::find::<NfaExecutor>(n, haystack, 0).map(|m| cap_sum(&m)).sum());
        }
        if let Some(t) = &tdfa {
            check(backends::find::<TdfaExecutor>(t, haystack, 0).map(|m| cap_sum(&m)).sum());
        }

        let (states, marks) = match &tdfa {
            Some(t) => {
                let s = t.stats();
                (s.num_states.to_string(), s.num_marks.to_string())
            }
            None => ("-".to_string(), "-".to_string()),
        };

        let bt = throughput(haystack.len(), || {
            backends::find::<BacktrackExecutor>(&cr, haystack, 0).map(|m| cap_sum(&m)).sum()
        });
        let pike = throughput(haystack.len(), || {
            backends::find::<PikeVMExecutor>(&cr, haystack, 0).map(|m| cap_sum(&m)).sum()
        });
        let nfa_mbps = nfa.as_ref().map(|n| {
            throughput(haystack.len(), || {
                backends::find::<NfaExecutor>(n, haystack, 0).map(|m| cap_sum(&m)).sum()
            })
        });
        let tdfa_mbps = tdfa.as_ref().map(|t| {
            throughput(haystack.len(), || {
                backends::find::<TdfaExecutor>(t, haystack, 0).map(|m| cap_sum(&m)).sum()
            })
        });
        // Force the regex crate to materialize captures too (sum of group ends).
        let rx_mbps = rx.as_ref().map(|r| {
            throughput(haystack.len(), || {
                let mut total = 0usize;
                for caps in r.captures_iter(haystack) {
                    for i in 0..caps.len() {
                        if let Some(g) = caps.get(i) {
                            total = total.wrapping_add(g.end());
                        }
                    }
                }
                total
            })
        });

        // tdfa-jit column, materializing captures (cap_sum), same as the others.
        #[cfg(feature = "tdfa-jit")]
        let jit_seg = {
            let prog = TdfaJitProgram::try_from_ir(&ire).ok();
            if let Some(p) = &prog {
                check(
                    backends::find::<TdfaJitExecutor>(p, haystack, 0)
                        .map(|m| cap_sum(&m))
                        .sum(),
                );
            }
            let mbps = prog.as_ref().map(|p| {
                throughput(haystack.len(), || {
                    backends::find::<TdfaJitExecutor>(p, haystack, 0)
                        .map(|m| cap_sum(&m))
                        .sum()
                })
            });
            let active = prog.as_ref().is_some_and(|p| p.jit_active());
            format!(" {:>10}", jit_cell(mbps, active))
        };
        #[cfg(not(feature = "tdfa-jit"))]
        let jit_seg = String::new();

        println!(
            "{:<28} {:>7} {:>6} {:>10} {:>10} {:>10} {:>10}{} {:>10}{}",
            name,
            states,
            marks,
            cell(Some(bt)),
            cell(Some(pike)),
            cell(nfa_mbps),
            cell(tdfa_mbps),
            jit_seg,
            cell(rx_mbps),
            if agree { "" } else { "   ! capture checksums disagree" },
        );
    }
}
