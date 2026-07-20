//! Scratch micro-bench for the per-byte-guards executor path (`\b`, multiline
//! `^`/`$`): patterns whose TDFA scan evaluates zero-width guards per byte.

use regress::backends::{self, TdfaExecutor, TdfaProgram};
use std::time::Instant;

fn main() {
    let haystack: &str = include_str!("data/sherlock.txt");
    let cases: &[(&str, &str, &str)] = &[
        ("word_boundary", r"\bHol\w+", ""),
        ("multiline_start", r"^\w+", "m"),
        ("multiline_end", r"\w+$", "m"),
    ];
    for (name, pattern, flags_str) in cases {
        let mut flags = regress::Flags::default();
        flags.multiline = flags_str.contains('m');
        let mut ire = backends::try_parse(pattern.chars().map(u32::from), flags).unwrap();
        backends::optimize(&mut ire);
        let prog = match TdfaProgram::try_from_ir(&ire) {
            Ok(p) => p,
            Err(e) => {
                println!("{name:20} build failed: {e:?}");
                continue;
            }
        };
        let count: usize = backends::find::<TdfaExecutor>(&prog, haystack, 0).count();
        let iters = 40;
        let start = Instant::now();
        let mut total = 0usize;
        for _ in 0..iters {
            total += backends::find::<TdfaExecutor>(&prog, haystack, 0).count();
        }
        let el = start.elapsed();
        assert_eq!(total, count * iters);
        let mbs = (haystack.len() as f64 * iters as f64) / el.as_secs_f64() / 1e6;
        println!("{name:20} {count:6} matches  {mbs:8.1} MB/s");
    }
}
