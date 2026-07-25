//! Minimal, isolated comparison: why does `[a-zA-Z0-9_-]{1,2000}` (ranged
//! bound) cost so much less than `[a-z0-9]{2000}` (exact bound) at the same
//! N, through the real `TdfaProgram::try_from_ir` entry point? Prints the
//! `REGRESS_TDFA_MEM_TRACE` thread-growth trace for each in isolation (no
//! other pattern builds in between) plus final chosen-strategy state count.

use regress::backends::{self, TdfaProgram};

fn probe(pat: &str) {
    probe_interval(pat, 200);
}

fn probe_interval(pat: &str, interval: usize) {
    unsafe {
        std::env::set_var("REGRESS_TDFA_MEM_TRACE", interval.to_string());
    }
    eprintln!("\n=== {pat} ===");
    let flags = regress::Flags::default();
    let mut ire = backends::try_parse(pat.chars().map(u32::from), flags).unwrap();
    backends::optimize(&mut ire);
    match TdfaProgram::try_from_ir(&ire) {
        Ok(prog) => {
            let s = prog.stats();
            eprintln!("--- final: states={} heap_bytes={}", s.num_states, s.heap_bytes);
        }
        Err(e) => eprintln!("--- BUILD FAILED: {e:?}"),
    }
}

fn main() {
    probe("[a-z0-9]{2000}");
    probe("([a-z0-9]{2000})");
    probe("(a)(b)?(c)?[a-z0-9]{2000}");
    probe("[a-zA-Z0-9_-]{1,2000}");
    probe("[a-z0-9]{1,2000}");
    probe("[a-z0-9]{0,2000}");
    // Fine-grained trace across the min->optional hinge: does thread count
    // grow linearly through the mandatory 500, then flatten once the
    // optional (shared-exit) section starts?
    probe_interval("[a-z0-9]{500,2000}", 25);
}
