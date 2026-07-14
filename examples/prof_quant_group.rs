//! Profiling target for the cap_quant_group pattern `(\w+\s*)+` over
//! the Sherlock corpus using TdfaMatches (zero-allocation lending iterator).
//! Usage: `cargo run --release --example prof_quant_group [iters]`
use regress::automata::prefilter::TdfaProgram;
use regress::automata::executors::TdfaMatches;
use regress::backends;
use std::fs;
use std::hint::black_box;

fn main() {
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);

    let corpus = fs::read("examples/data/sherlock.txt").expect("read sherlock.txt");
    let corpus_str = std::str::from_utf8(&corpus).expect("utf8");
    let pattern = r"(\w+\s*)+";
    let flags = regress::Flags::from("");
    let mut ire = backends::try_parse(pattern.chars().map(u32::from), flags).unwrap();
    backends::optimize(&mut ire);
    let tdfa = TdfaProgram::try_from_ir(&ire).expect("tdfa");

    let mut total = 0usize;
    for _ in 0..iters {
        let mut it = TdfaMatches::new(&tdfa, corpus_str, 0);
        while let Some(m) = it.next() {
            // Consume both full match range and capture group 0.
            total = total.wrapping_add(m.range.end);
            if let Some(c) = m.capture(0) {
                total = total.wrapping_add(c.end);
            }
        }
    }
    black_box(total);
    eprintln!("prof_quant_group: {iters} iters, total {total}");
}
