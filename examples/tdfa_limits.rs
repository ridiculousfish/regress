//! Scratch probe: practical size limits for the table-based TDFA.
//!
//! Builds raw unanchored `Tdfa`s (bypassing `TdfaProgram` strategy fallbacks)
//! for two families — `a.{N}b` (exponential subset blowup: every pending
//! start in the window is a state bit) and k-word literal alternations
//! (linear, trie-shaped) — and reports build/optimize time, state count,
//! table heap, and interpreter scan throughput on adversarial input.

use regress::automata::tdfa_backend::execute;
use regress::backends::{self, Nfa, Tdfa};
use std::time::Instant;

fn bench_case(label: &str, pattern: &str, input: &[u8]) {
    let flags = regress::Flags::default();
    let mut ire = backends::try_parse(pattern.chars().map(u32::from), flags).unwrap();
    backends::optimize(&mut ire);
    let nfa = match Nfa::try_from_unanchored(&ire) {
        Ok(n) => n,
        Err(e) => {
            println!("{label:24} NFA build failed: {e:?}");
            return;
        }
    };
    let t0 = Instant::now();
    let mut tdfa = match Tdfa::try_from(&nfa) {
        Ok(t) => t,
        Err(e) => {
            println!("{label:24} TDFA build failed after {:.2?}: {e:?}", t0.elapsed());
            return;
        }
    };
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;
    let raw_states = tdfa.num_states();
    let t1 = Instant::now();
    tdfa.optimize();
    let opt_ms = t1.elapsed().as_secs_f64() * 1e3;
    let s = tdfa.stats();

    // Throughput: single unanchored pass over the (non-matching) input, best
    // of 3.
    let mut best = f64::INFINITY;
    for _ in 0..3 {
        let t = Instant::now();
        let m = execute(&tdfa, input, 0);
        let el = t.elapsed().as_secs_f64();
        assert!(m.is_none(), "input unexpectedly matched");
        best = best.min(el);
    }
    let mbs = input.len() as f64 / best / 1e6;

    println!(
        "{label:24} build {build_ms:9.1}ms  opt {opt_ms:9.1}ms  states {raw_states:6}->{:6}  classes {:3}  heap {:9.2} MB  scan {mbs:8.1} MB/s",
        s.num_states,
        tdfa.num_classes(),
        s.heap_bytes as f64 / 1e6,
    );
}

/// Deterministic pseudo-random lowercase word, 8 chars.
fn word(seed: u64) -> String {
    let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1);
    (0..8)
        .map(|_| {
            x ^= x >> 33;
            x = x.wrapping_mul(0xFF51AFD7ED558CCD);
            b'a' + (x % 26) as u8
        })
        .map(char::from)
        .collect()
}

fn main() {
    let adversarial: Vec<u8> = vec![b'a'; 4 << 20]; // 4 MB of 'a'
    println!("== a.{{N}}b (exponential subset family), 4 MB all-'a' input ==");
    for n in [4usize, 6, 8, 10, 11, 12, 13] {
        bench_case(&format!("a.{{{n}}}b"), &format!("a.{{{n}}}b"), &adversarial);
    }

    // Non-matching prose-ish input for the trie family: words the alternation
    // doesn't contain (digits break every 8-char word).
    let prose: Vec<u8> = (0..(4 << 20)).map(|i| if i % 5 == 4 { b'0' } else { b'a' + (i % 26) as u8 }).collect();
    println!("\n== k-word literal alternation (trie family), 4 MB synthetic input ==");
    for k in [256usize, 1024, 4096, 8192] {
        let pat: String = (0..k as u64).map(word).collect::<Vec<_>>().join("|");
        bench_case(&format!("{k}-word alt"), &pat, &prose);
    }
}
