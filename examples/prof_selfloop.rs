//! Focused profiling target: run the `selfloop` case `(?:(\d)(\d)(\d))*` over
//! 128 KB of digits through one chosen backend, in a long loop, so a sampler
//! can attribute the cost. Usage: `prof_selfloop <tdfa|backtrack|nfa|pikevm> [iters]`
use regress::backends::{
    self, BacktrackExecutor, CompiledRegex, Nfa, NfaExecutor, PikeVMExecutor, Tdfa, TdfaExecutor,
};
use std::hint::black_box;

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "tdfa".into());
    let iters: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);

    let input = "0123456789".repeat(128 * 1024 / 10 + 1);
    let pattern = r"(?:(\d)(\d)(\d))*";
    let flags = regress::Flags::from("");
    let ire = backends::try_parse(pattern.chars().map(u32::from), flags).unwrap();
    let cr: CompiledRegex = backends::emit(&ire);
    let nfa = Nfa::try_from_unanchored(&ire).unwrap();
    let mut tdfa = Tdfa::try_from(&nfa).unwrap();
    tdfa.optimize();

    let mut total = 0usize;
    for _ in 0..iters {
        let c = match which.as_str() {
            "tdfa" => backends::find::<TdfaExecutor>(&tdfa, &input, 0).count(),
            "nfa" => backends::find::<NfaExecutor>(&nfa, &input, 0).count(),
            "backtrack" => backends::find::<BacktrackExecutor>(&cr, &input, 0).count(),
            "pikevm" => backends::find::<PikeVMExecutor>(&cr, &input, 0).count(),
            other => panic!("unknown backend {other}"),
        };
        total += c;
    }
    black_box(total);
    eprintln!("{which}: {iters} iters, total matches {total}");
}
