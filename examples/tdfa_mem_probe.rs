//! Instrumentation probe: estimate TDFA construction-time memory, focused on
//! unanchored search patterns (implicit leading `.*?`, via
//! `Nfa::try_from_unanchored`) vs. anchored construction on the same pattern
//! body.
//!
//! Two independent measurements, combined:
//! 1. Process-wide allocator peak (current - and high-water-mark) bytes
//!    around the raw `Tdfa::try_from` call (pre-`optimize()`), via a global
//!    counting allocator.
//! 2. Thread-count-per-state growth during construction, via the library's
//!    opt-in `REGRESS_TDFA_MEM_TRACE=<n>` stderr trace (see `MemTrace` in
//!    `src/automata/tdfa.rs`) — set here so every run reports.
//!
//! Run: `cargo run --release --example tdfa_mem_probe --features nfa`

use regress::backends::{self, Nfa, Tdfa, TdfaProgram};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct PeakAlloc;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for PeakAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let cur = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(cur, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            if new_size >= layout.size() {
                let cur = CURRENT.fetch_add(new_size - layout.size(), Ordering::Relaxed) + (new_size - layout.size());
                PEAK.fetch_max(cur, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOC: PeakAlloc = PeakAlloc;

fn reset_peak() {
    PEAK.store(CURRENT.load(Ordering::Relaxed), Ordering::Relaxed);
}
fn peak_delta_mb(base: usize) -> f64 {
    (PEAK.load(Ordering::Relaxed).saturating_sub(base)) as f64 / 1e6
}

fn build_nfa(pattern: &str, unanchored: bool) -> Nfa {
    let flags = regress::Flags::default();
    let mut ire = backends::try_parse(pattern.chars().map(u32::from), flags).unwrap();
    backends::optimize(&mut ire);
    if unanchored {
        Nfa::try_from_unanchored(&ire).unwrap()
    } else {
        Nfa::try_from(&ire).unwrap()
    }
}

fn probe(label: &str, pattern: &str, unanchored: bool, trace_interval: usize) {
    // SAFETY: this probe is single-threaded (no concurrent env mutation).
    unsafe {
        std::env::set_var("REGRESS_TDFA_MEM_TRACE", trace_interval.to_string());
    }
    let nfa = build_nfa(pattern, unanchored);
    let base = CURRENT.load(Ordering::Relaxed);
    reset_peak();
    let t0 = Instant::now();
    match Tdfa::try_from(&nfa) {
        Ok(tdfa) => {
            let elapsed = t0.elapsed();
            eprintln!(
                "== {label:32} states={:6} elapsed={:8.2?} peak_delta={:9.2} MB current_delta={:9.2} MB",
                tdfa.num_states(),
                elapsed,
                peak_delta_mb(base),
                (CURRENT.load(Ordering::Relaxed).saturating_sub(base)) as f64 / 1e6,
            );
        }
        Err(e) => {
            eprintln!(
                "== {label:32} BUILD FAILED: {e:?} elapsed={:8.2?} peak_delta={:9.2} MB",
                t0.elapsed(),
                peak_delta_mb(base),
            );
        }
    }
}

fn main() {
    // Exponential subset family (a.{N}b): every pending start-offset in the
    // window is a state bit, so N stays small (matches examples/tdfa_limits.rs).
    println!("--- unanchored (implicit leading .*?) vs anchored, a.{{N}}b family ---");
    for n in [8usize, 10, 11, 12, 13] {
        let pat = format!("a.{{{n}}}b");
        probe(&format!("unanchored a.{{{n}}}b"), &pat, true, 200);
        probe(&format!("anchored   a.{{{n}}}b"), &pat, false, 200);
    }

    // Ordinary bounded repetition (linear in N) — the family the existing
    // tdfa-size-limits memory note measured. Anchored vs. unanchored isolates
    // the cost of the implicit leading .*? prefix.
    println!("\n--- [a-z0-9]{{N}}, unanchored vs anchored ---");
    for n in [1000usize, 2000, 4000, 6000, 8000] {
        let pat = format!("[a-z0-9]{{{n}}}");
        probe(&format!("unanchored [a-z0-9]{{{n}}}"), &pat, true, 500);
        probe(&format!("anchored   [a-z0-9]{{{n}}}"), &pat, false, 500);
    }

    // Realistic, *small*-N patterns: no one writes {8000}, but bare bounded
    // repeats of everyday width (hex tokens, hashes, fixed-width IDs) are
    // common and unanchored-by-default in real searches.
    println!("\n--- realistic small-N patterns, unanchored vs anchored ---");
    for pat in [".{12}", ".{20}", ".{64}", "[0-9a-f]{8}", "[0-9a-f]{32}", "\\d{16}"] {
        probe(&format!("unanchored {pat}"), pat, true, 100000);
        probe(&format!("anchored   {pat}"), pat, false, 100000);
    }

    // Isolate whether ONE literal boundary is enough to trigger the
    // exponential family, or both (leading + trailing) are required.
    println!("\n--- one-sided anchor vs two-sided, N=12 ---");
    for pat in ["a.{12}", ".{12}b", "a.{12}b"] {
        probe(&format!("unanchored {pat}"), pat, true, 100000);
    }

    // The REAL entry point: does TdfaProgram::try_from_ir (what regex!/tdfa-jit
    // actually call) hit the blowup, or does its Scan-budget pre-gate /
    // start-predicate fallback already dodge it before paying for the raw
    // unanchored Tdfa::try_from?
    println!("\n--- real entry point (TdfaProgram::try_from_ir) ---");
    for pat in [
        "a.{12}b",
        "a.{12}",
        ".{12}b",
        "a.{512}b",
        "[a-z0-9]{300}",
        "[a-z0-9]{500}",
        "[a-z0-9]{700}",
        "[a-z0-9]{1000}",
        "[a-z0-9]{2000}",
        "[a-z0-9]{4000}",
        "[a-z0-9]{8000}",
        "[a-zA-Z0-9_-]{1,2000}",
        "[\\s\\S]{0,2000}",
    ] {
        let flags = regress::Flags::default();
        let mut ire = backends::try_parse(pat.chars().map(u32::from), flags).unwrap();
        backends::optimize(&mut ire);
        let base = CURRENT.load(Ordering::Relaxed);
        reset_peak();
        let t0 = Instant::now();
        match TdfaProgram::try_from_ir(&ire) {
            Ok(prog) => {
                let s = prog.stats();
                eprintln!(
                    "== entry {pat:20} states={:6} elapsed={:8.2?} peak_delta={:9.2} MB",
                    s.num_states,
                    t0.elapsed(),
                    peak_delta_mb(base),
                );
            }
            Err(e) => {
                eprintln!("== entry {pat:20} BUILD FAILED: {e:?} elapsed={:8.2?}", t0.elapsed());
            }
        }
    }
}
