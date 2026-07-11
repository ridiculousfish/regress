//! Focused profiling target for cap_words1 `(\w+)` pattern.
//! Usage: `cargo run --release --example prof_cap_words [tdfa|tdfa-jit] [iters]`
use regress::backends::{TdfaMatches, TdfaProgram};
#[cfg(feature = "tdfa-jit")]
use regress::backends::TdfaJitProgram;
use std::hint::black_box;
use std::fs;

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "tdfa".into());
    let iters: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);

    let haystack = fs::read_to_string("examples/data/sherlock.txt")
        .unwrap_or_else(|_| "hello world foo bar".repeat(10000));

    let pattern = r"(\w+)";
    let flags = regress::Flags::from("");
    let ire = regress::backends::try_parse(pattern.chars().map(u32::from), flags).unwrap();

    let prog = TdfaProgram::try_from_ir(&ire).unwrap();

    let mut total = 0usize;

    match which.as_str() {
        "tdfa" => {
            for _ in 0..iters {
                let mut it = TdfaMatches::new(&prog, &haystack, 0);
                while let Some(m) = it.next() {
                    total = total.wrapping_add(m.range.end);
                    for i in 0..m.num_captures() {
                        if let Some(r) = m.capture(i) {
                            total = total.wrapping_add(r.end);
                        }
                    }
                }
            }
        }
        #[cfg(feature = "tdfa-jit")]
        "tdfa-jit" => {
            let jit = TdfaJitProgram::try_from_ir(&ire).unwrap();
            for _ in 0..iters {
                let mut it = TdfaMatches::new(&*jit, &haystack, 0);
                while let Some(m) = it.next() {
                    total = total.wrapping_add(m.range.end);
                    for i in 0..m.num_captures() {
                        if let Some(r) = m.capture(i) {
                            total = total.wrapping_add(r.end);
                        }
                    }
                }
            }
        }
        other => panic!("unknown backend {other}"),
    }

    black_box(total);
    eprintln!("{which}: {iters} iters, result {total}");
}
