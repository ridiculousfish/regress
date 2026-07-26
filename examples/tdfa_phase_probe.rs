//! Reproduce the `[a-zA-Z0-9]{N}` construction-time phase breakdown from the
//! `tdfa-size-limits` investigation: which part of `Tdfa::try_from` actually
//! costs the time, and how that shifts once `num_marks` crosses
//! `MAX_FALLBACK_MARKS` (16384) and `compute_accept_fallback` falls back to
//! its cheap conservative approximation.
//!
//! Run: `cargo run --release --example tdfa_phase_probe --features nfa -- 500 1000 2000 2300 2500 8000`
//! (defaults to that same set if no sizes are given).

use regress::automata::nfa::Nfa;
use regress::automata::tdfa::Tdfa;
use regress::backends;
use std::time::Instant;

fn probe(n: usize) {
    unsafe {
        std::env::set_var("REGRESS_TDFA_PHASE_TRACE", "1");
        std::env::set_var("REGRESS_TDFA_MEM_TRACE", (n + 1).to_string());
    }
    let pat = format!("[a-zA-Z0-9]{{{n}}}");
    let flags = regress::Flags::default();
    let mut ire = backends::try_parse(pat.chars().map(u32::from), flags).unwrap();
    backends::optimize(&mut ire);
    let nfa = Nfa::try_from_unanchored(&ire).unwrap();
    eprintln!("=== n={n} ===");
    let t0 = Instant::now();
    match Tdfa::try_from(&nfa) {
        Ok(t) => eprintln!("-- states={} elapsed={:?}", t.num_states(), t0.elapsed()),
        Err(e) => eprintln!("-- BUILD FAILED: {e:?} elapsed={:?}", t0.elapsed()),
    }
}

fn main() {
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .map(|s| s.parse().unwrap())
        .collect();
    let sizes = if sizes.is_empty() {
        vec![500, 1000, 2000, 2300, 2500, 8000]
    } else {
        sizes
    };
    for n in sizes {
        probe(n);
    }
}
