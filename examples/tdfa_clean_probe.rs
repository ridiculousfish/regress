//! Construction-time probe with only REGRESS_TDFA_PHASE_TRACE set (not
//! MEM_TRACE, which adds its own O(threads) unwrapped cost) -- for accurate
//! per-phase timing against real production behavior.
//!
//! Run: cargo run --release --example tdfa_clean_probe --features nfa -- 8000

use regress::automata::nfa::Nfa;
use regress::automata::tdfa::Tdfa;
use regress::backends;
use std::time::Instant;

fn probe(n: usize) {
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
    unsafe {
        std::env::set_var("REGRESS_TDFA_PHASE_TRACE", "1");
    }
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .map(|s| s.parse().unwrap())
        .collect();
    let sizes = if sizes.is_empty() { vec![8000] } else { sizes };
    for n in sizes {
        probe(n);
    }
}
