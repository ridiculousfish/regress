//! Profiler: where do the bytes go in the TDFA scan loop?
//!
//! Motivating question for JIT codegen work: is byte-time concentrated in
//! *self-loop* states (a transition that re-enters the same state), or spread
//! across a chain of distinct states? The answer decides whether basic-block
//! reordering for fall-through (helps chains) or peeling the hot self-loop edge
//! out of the jump table (helps loops) is the better lever — and whether the
//! per-byte bounds-check-elision win is universal (it is, regardless).
//!
//! For each pattern we drive the *anchored* transition table (`start(0)`, the
//! automaton the JIT capture-free tier actually compiles — it requires
//! `start_fixed`) forward from every candidate position over the Sherlock
//! corpus, following transitions until a dead state, and tally per-state steps
//! and self-loop steps (`next == state`). A run is capped so pathological
//! all-consuming patterns (`.*`) don't go quadratic; the cap is reported.
//!
//! Run with: `cargo run --release --example jit_selfloop_profile`

use regress::backends::{self, Nfa, Tdfa};
use std::fs;

/// Build the *anchored* optimized TDFA for `pattern` (the JIT capture-free
/// tier's automaton), or the first failing stage as a string.
fn build_anchored_tdfa(pattern: &str, flags: &str) -> Result<Tdfa, String> {
    let flags = regress::Flags::from(flags);
    let ire = backends::try_parse(pattern.chars().map(u32::from), flags)
        .map_err(|e| format!("parse: {:?}", e))?;
    // Anchored: mirrors the JIT verify automaton (start_fixed). The unanchored
    // `.*?` prefix would otherwise add a giant prefix self-loop that a prefilter
    // replaces in the real pipeline, confounding the measurement.
    let nfa = Nfa::try_from(&ire).map_err(|e| format!("nfa: {:?}", e))?;
    let mut tdfa = Tdfa::try_from(&nfa).map_err(|e| format!("tdfa: {:?}", e))?;
    tdfa.optimize();
    Ok(tdfa)
}

// `TDFA_DEAD_STATE` (tdfa.rs) — state id 0 is the sink; not re-exported.
const DEAD: u32 = 0;

/// Drive the anchored automaton from every start position; tally steps.
struct Profile {
    total_steps: u64,
    selfloop_steps: u64,
    /// per-state (visits, selfloop_visits)
    per_state: Vec<(u64, u64)>,
}

fn profile(tdfa: &Tdfa, input: &[u8], run_cap: usize) -> Profile {
    let nc = tdfa.num_classes();
    let b2c = tdfa.byte_to_class();
    let trans = tdfa.transitions();
    let start = tdfa.start(0);
    let mut per_state = vec![(0u64, 0u64); tdfa.num_states()];
    let mut total = 0u64;
    let mut selfloop = 0u64;

    for begin in 0..input.len() {
        let mut state = start;
        let mut steps = 0usize;
        let mut i = begin;
        while i < input.len() && steps < run_cap {
            if state == DEAD {
                break;
            }
            let class = b2c[input[i] as usize] as usize;
            let next = trans[state as usize * nc + class];
            if next == DEAD {
                break;
            }
            let is_self = next == state;
            per_state[state as usize].0 += 1;
            if is_self {
                per_state[state as usize].1 += 1;
                selfloop += 1;
            }
            total += 1;
            state = next;
            i += 1;
            steps += 1;
        }
    }
    Profile {
        total_steps: total,
        selfloop_steps: selfloop,
        per_state,
    }
}

fn main() {
    let corpus = fs::read("examples/data/sherlock.txt").expect("read sherlock.txt");
    let corpus = corpus.as_slice();
    // Cap a single anchored run so `.*`-style all-consumers stay ~linear overall.
    const RUN_CAP: usize = 256;

    // Capture-free / start_fixed-eligible patterns from the realistic battery
    // (the shapes the JIT capture-free tier compiles).
    let suite: &[(&str, &str, &str)] = &[
        ("name_sherlock", r"Sherlock", ""),
        ("name_alt4", r"Sher[a-z]+|Hol[a-z]+", ""),
        ("the_lower", r"the", ""),
        ("the_whitespace", r"the\s+\w+", ""),
        ("words", r"\w+", ""),
        ("ing_suffix", r"[a-zA-Z]+ing", ""),
        ("quotes", "\"[^\"]*\"", ""),
        ("everything_greedy", r".*", ""),
        ("ip", r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}", ""),
    ];

    println!(
        "anchored verify automaton, run cap = {} steps/start\n",
        RUN_CAP
    );
    println!(
        "{:<20} {:>7} {:>12} {:>9}  {}",
        "case", "states", "steps", "selfloop", "top states (id:visits sl% [start])"
    );
    println!("{}", "-".repeat(100));

    for &(name, pattern, flags) in suite {
        let tdfa = match build_anchored_tdfa(pattern, flags) {
            Ok(t) => t,
            Err(e) => {
                println!("{:<20} {}", name, e);
                continue;
            }
        };
        let prof = profile(&tdfa, corpus, RUN_CAP);
        let sl_pct = if prof.total_steps == 0 {
            0.0
        } else {
            prof.selfloop_steps as f64 / prof.total_steps as f64 * 100.0
        };
        // Top 4 states by visits.
        let mut idx: Vec<usize> = (0..prof.per_state.len()).collect();
        idx.sort_by_key(|&s| std::cmp::Reverse(prof.per_state[s].0));
        let start0 = tdfa.start(0) as usize;
        let top: Vec<String> = idx
            .iter()
            .take(4)
            .filter(|&&s| prof.per_state[s].0 > 0)
            .map(|&s| {
                let (v, sl) = prof.per_state[s];
                let pct = sl as f64 / v as f64 * 100.0;
                let mark = if s == start0 { " S" } else { "" };
                format!("{}:{}·{:.0}%{}", s, v, pct, mark)
            })
            .collect();
        println!(
            "{:<20} {:>7} {:>12} {:>8.0}% {:>2} {}",
            name,
            tdfa.num_states(),
            prof.total_steps,
            sl_pct,
            "",
            top.join("  ")
        );
    }
    println!(
        "\nsl% = share of byte-steps that re-enter the same state (self-loop).\n\
         top states: id:visits·selfloop%  (S = anchored start state)"
    );
}
