//! TDFA construction-cost stress harness.
//!
//! Run with:
//!     cargo run --release --example tdfa_stress --features nfa
//!
//! Prints a per-pattern table of NFA size, num_tags, TDFA build time,
//! and outcome (states / BudgetExceeded / other error). Iterate on the
//! TDFA construction by editing/adding patterns here and re-running.
//!
//! Patterns are categorized roughly by what they exercise — the
//! ":fast:" baseline cases should always finish in microseconds; the
//! ":slow:" cases are the ones currently blowing past the budget and
//! that we want construction to handle better.

#[cfg(feature = "nfa")]
use std::time::Instant;

#[cfg(feature = "nfa")]
use regress::Flags;
#[cfg(feature = "nfa")]
use regress::automata::tdfa::Tdfa;
#[cfg(feature = "nfa")]
use regress::backends::{self, Nfa};

#[cfg(feature = "nfa")]
struct Pattern {
    label: &'static str,
    regex: &'static str,
}

#[cfg(feature = "nfa")]
const PATTERNS: &[Pattern] = &[
    // :fast: baselines — should build in microseconds.
    Pattern {
        label: "fast: literal",
        regex: r"abc",
    },
    Pattern {
        label: "fast: alternation",
        regex: r"a|b|c",
    },
    Pattern {
        label: "fast: loop",
        regex: r"(a|b)+",
    },
    Pattern {
        label: "fast: one capture",
        regex: r"(\w+)",
    },
    Pattern {
        label: "fast: many literals in alt",
        regex: r"a|b|c|d|e|f|g|h|i|j|k|l|m|n|o|p|q|r|s|t|u|v|w|x|y|z",
    },
    // multiline ^ in the middle of a pattern (exercises the
    // anchor_alt / mid-input switch).
    Pattern {
        label: "mline ^: (a*)^(a*)$",
        regex: r"(a*)^(a*)$",
    },
    Pattern {
        label: "$-only: bar$",
        regex: r"bar$",
    },
    Pattern {
        label: "$-only: (a*)$",
        regex: r"(a*)$",
    },
    // :slow: capture-heavy patterns — currently hit BudgetExceeded.
    // Adding capture groups to the alternation explodes state count
    // because every (NFA subset × tag_map) pair becomes a distinct
    // canonical state.
    Pattern {
        label: "slow: caps_in_alt_5",
        regex: r"((a)|(b)|(c)|(d)|(e))+",
    },
    Pattern {
        label: "slow: caps_in_alt_10",
        regex: r"((a)|(b)|(c)|(d)|(e)|(f)|(g)|(h)|(i)|(j))+",
    },
    Pattern {
        label: "slow: regexp-capture body",
        // run_regexp_capture_test, line 563 with the `^` stripped so we
        // characterize the body alone (the anchor isn't what's blowing up).
        regex: r"(((N({)?)|(R)|(U)|(V)|(B)|(H)|(n((n)|(r)|(v)|(h))?)|(r(r)?)|(v)|(b((n)|(b))?)|(h))|((Y)|(A)|(E)|(o(u)?)|(p(u)?)|(q(u)?)|(s)|(t)|(u)|(w)|(x(u)?)|(y)|(z)|(a((T)|(A)|(L))?)|(c)|(e)|(f(u)?)|(g(u)?)|(i)|(j)|(l)|(m(u)?)))+",
    },
];

#[cfg(feature = "nfa")]
struct Row {
    label: &'static str,
    pattern_len: usize,
    num_tags: usize,
    nfa_ms: f64,
    tdfa_ms: f64,
    outcome: String,
}

#[cfg(feature = "nfa")]
fn measure(pat: &Pattern) -> Row {
    let flags = Flags::default();
    let mut ir = backends::try_parse(pat.regex.chars().map(u32::from), flags).expect("parse");
    backends::optimize(&mut ir);

    let t0 = Instant::now();
    let nfa = Nfa::try_from(&ir).expect("nfa");
    let nfa_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let num_tags = nfa.num_tags();

    let t1 = Instant::now();
    let res = Tdfa::try_from(&nfa);
    let tdfa_ms = t1.elapsed().as_secs_f64() * 1000.0;

    let outcome = match res {
        Ok(tdfa) => format!("{} states", tdfa.num_states()),
        Err(e) => format!("{:?}", e),
    };

    Row {
        label: pat.label,
        pattern_len: pat.regex.len(),
        num_tags,
        nfa_ms,
        tdfa_ms,
        outcome,
    }
}

#[cfg(feature = "nfa")]
fn main() {
    println!(
        "{:<32} {:>7} {:>5} {:>10} {:>10}  outcome",
        "label", "pat_len", "tags", "nfa_ms", "tdfa_ms"
    );
    println!("{}", "-".repeat(96));
    for pat in PATTERNS {
        let row = measure(pat);
        println!(
            "{:<32} {:>7} {:>5} {:>10.3} {:>10.3}  {}",
            row.label, row.pattern_len, row.num_tags, row.nfa_ms, row.tdfa_ms, row.outcome,
        );
    }

    println!("\nExecution tests:");
    let exec_cases: &[(&str, &str)] = &[
        (r"bar$", "bar"),
        (r"bar$", "barX"),
        (r"bar$", "bar\nfoo"),
        (r"(a*)^(a*)$", "aa\raaa"),
        (r"(a*)^(a*)$", ""),
        (r"(a*)$", "aaa"),
    ];
    for (pat, input) in exec_cases {
        execute_once(pat, input);
    }
}

#[cfg(feature = "nfa")]
fn execute_once(pat: &str, input: &str) {
    let flags = Flags::default();
    let mut ir = backends::try_parse(pat.chars().map(u32::from), flags).expect("parse");
    backends::optimize(&mut ir);
    let nfa = match Nfa::try_from(&ir) {
        Ok(n) => n,
        Err(e) => {
            println!("  pat={:?} input={:?}: nfa-err {:?}", pat, input, e);
            return;
        }
    };
    let tdfa = match Tdfa::try_from(&nfa) {
        Ok(t) => t,
        Err(e) => {
            println!("  pat={:?} input={:?}: tdfa-err {:?}", pat, input, e);
            return;
        }
    };
    let m = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        regress::automata::tdfa_backend::execute(&tdfa, input.as_bytes(), 0)
    }));
    match m {
        Ok(Some(m)) => println!("  pat={:?} input={:?}: tdfa-match {:?}", pat, input, m),
        Ok(None) => println!("  pat={:?} input={:?}: tdfa-no-match", pat, input),
        Err(_) => println!("  pat={:?} input={:?}: tdfa-panic", pat, input),
    }
}

#[cfg(not(feature = "nfa"))]
fn main() {
    eprintln!("This example requires the `nfa` feature:");
    eprintln!("    cargo run --release --example tdfa_stress --features nfa");
}
