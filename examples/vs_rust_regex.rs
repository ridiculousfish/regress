//! Compare regress's real public API (backtracker/PikeVM, whatever's
//! default) against the `regex` crate (vendored at ../regress-vendor-regex,
//! dev-dependency only) on the pathological pattern this session's
//! investigation focused on, plus a couple of smaller/more realistic sizes
//! for context.
//!
//! Also traces which internal strategy rust-regex's meta engine actually
//! picks (lazy DFA / full DFA / onepass / backtracker / PikeVM), via the
//! `regex` crate's "logging" feature (enabled dev-only in Cargo.toml) and a
//! minimal `log::Log` impl below -- avoids guessing.
//!
//! Run: `cargo run --release --example vs_rust_regex`
//! Run with strategy tracing: `cargo run --release --example vs_rust_regex -- trace`

use std::time::Instant;

struct StderrLogger;
impl log::Log for StderrLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        eprintln!("[{}] {}", record.level(), record.args());
    }
    fn flush(&self) {}
}

fn haystack(n: usize) -> String {
    // n class-matching bytes with a couple of non-matching bytes on each
    // side, so both engines have to do real unanchored search work, not
    // just match at offset 0.
    format!("!!!{}!!!", "a".repeat(n))
}

fn bench_regress(pattern: &str, hay: &str) {
    let t0 = Instant::now();
    let re = match regress::Regex::new(pattern) {
        Ok(re) => re,
        Err(e) => {
            println!("  regress:    COMPILE FAILED: {e:?} ({:?})", t0.elapsed());
            return;
        }
    };
    let compile = t0.elapsed();
    let t1 = Instant::now();
    let m = re.find(hay);
    let search = t1.elapsed();
    println!(
        "  regress:    compile={compile:>10?}  search={search:>10?}  match={:?}",
        m.map(|m| m.range())
    );
}

fn bench_rust_regex(pattern: &str, hay: &str) {
    let t0 = Instant::now();
    let re = match regex::Regex::new(pattern) {
        Ok(re) => re,
        Err(e) => {
            println!("  rust-regex: COMPILE FAILED: {e} ({:?})", t0.elapsed());
            return;
        }
    };
    let compile = t0.elapsed();
    let t1 = Instant::now();
    let m = re.find(hay);
    let search = t1.elapsed();
    println!(
        "  rust-regex: compile={compile:>10?}  search={search:>10?}  match={:?}",
        m.map(|m| m.range())
    );
}

fn main() {
    let trace = std::env::args().nth(1).as_deref() == Some("trace");
    if trace {
        log::set_logger(&StderrLogger).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
        // Just the pathological size, with tracing on -- this alone floods
        // stderr, so keep it to one case.
        let pattern = "[a-zA-Z0-9]{8000}";
        let hay = haystack(8000);
        eprintln!("=== trace: {pattern} ===");
        bench_rust_regex(pattern, &hay);
        return;
    }
    for n in [20usize, 500, 2000, 8000] {
        let pattern = format!("[a-zA-Z0-9]{{{n}}}");
        let hay = haystack(n);
        println!("=== [a-zA-Z0-9]{{{n}}} ===");
        bench_regress(&pattern, &hay);
        bench_rust_regex(&pattern, &hay);
    }
}
