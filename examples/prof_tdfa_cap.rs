//! Profile TDFA capture iteration (TdfaMatches) in isolation.
use regress::backends::{self, TdfaMatches, TdfaProgram};
use std::hint::black_box;

fn main() {
    let iters: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2000);
    let pattern = std::env::args().nth(2).unwrap_or_else(|| r"(\w+)".into());

    let haystack = include_str!("/home/peter/github/regress/examples/data/sherlock.txt");
    let flags = regress::Flags::default();
    let mut ire = backends::try_parse(pattern.chars().map(u32::from), flags).unwrap();
    backends::optimize(&mut ire);
    let prog = TdfaProgram::try_from_ir(&ire).unwrap();

    let mut total = 0usize;
    for _ in 0..iters {
        let mut it = TdfaMatches::new(&prog, haystack, 0);
        while let Some(m) = it.next() {
            total = total.wrapping_add(m.range.end);
            for i in 0..m.num_captures() {
                if let Some(r) = m.capture(i) { total = total.wrapping_add(r.end); }
            }
        }
    }
    black_box(total);
    eprintln!("done: {total}");
}
