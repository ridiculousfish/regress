#![allow(clippy::uninlined_format_args)]

use regress::backends::Nfa;
use regress::{Error, Flags, Regex, backends};

#[cfg(feature = "nfa")]
use regress::automata::nfa_backend;
#[cfg(feature = "nfa")]
use regress::automata::tdfa::Tdfa;
#[cfg(feature = "nfa")]
use regress::automata::tdfa_backend;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(name = "tester")]
struct Opt {
    /// The regular expression.
    pattern: String,

    /// The flags of the regular expression.
    #[structopt(long, short, parse(from_str = Flags::from))]
    flags: Option<Flags>,

    /// Optimize the IR.
    #[structopt(long, short, takes_value = false)]
    optimize: bool,

    /// Dump the unoptimized IR to stdout
    #[structopt(long)]
    dump_unoptimized_ir: bool,

    /// Dump the optimized IR to stdout
    #[structopt(long)]
    dump_optimized_ir: bool,

    /// Dump the bytecode to stdout.
    #[structopt(long)]
    dump_bytecode: bool,

    /// Dump all regular expression compilation phases to stdout.
    #[structopt(long)]
    dump_phases: bool,

    /// Dump the NFA to stdout.
    #[structopt(long)]
    dump_nfa: bool,

    /// Dump the NFA as Graphviz DOT to stdout.
    #[structopt(long)]
    dump_nfa_dot: bool,

    /// Dump the DFA to stdout.
    #[structopt(long)]
    dump_dfa: bool,

    /// Dump the DFA as Graphviz DOT to stdout.
    #[structopt(long)]
    dump_dfa_dot: bool,

    /// Comma-separated list of executors to run, in order.
    /// Names: bt, pikevm, tnfa, tdfa. If empty, defaults to the classical
    /// backtrack engine with the original (untagged) output format.
    #[structopt(long, value_delimiter = ",")]
    exec: Vec<String>,

    /// The input values to match against.
    #[structopt(conflicts_with_all = &["bench", "file"])]
    inputs: Vec<String>,

    /// Match against the contents of a specified file.
    #[structopt(long, conflicts_with_all = &["bench", "inputs"])]
    file: Option<PathBuf>,

    /// Benchmark the matches of the specified file.
    #[structopt(long, conflicts_with_all = &["file", "inputs"])]
    bench: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Bt,
    PikeVm,
    Tnfa,
    Tdfa,
}

impl Backend {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "bt" => Some(Backend::Bt),
            "pikevm" => Some(Backend::PikeVm),
            "tnfa" => Some(Backend::Tnfa),
            "tdfa" => Some(Backend::Tdfa),
            _ => None,
        }
    }
}

fn format_nfa_error(err: &backends::NfaError) -> String {
    match err {
        backends::NfaError::UnsupportedInstruction(desc) => {
            format!("Unsupported instruction: {}", desc)
        }
        backends::NfaError::BudgetExceeded => "Budget exceeded (too many states)".to_string(),
        backends::NfaError::NotUTF8 => "Invalid UTF-8 character".to_string(),
    }
}

fn format_match(
    range: &core::ops::Range<usize>,
    captures: &[Option<core::ops::Range<usize>>],
    input: &str,
) -> String {
    let mut result = String::new();

    // Show the full matched range
    result.push_str(&format!(
        "\"{}\" ({}..{})",
        &input[range.clone()],
        range.start,
        range.end
    ));

    // Show capture groups if any exist
    if !captures.is_empty() {
        result.push_str(", captures: [");
        for (i, cg) in captures.iter().enumerate() {
            if i > 0 {
                result.push_str(", ");
            }
            if let Some(cg_range) = cg {
                result.push_str(&format!(
                    "\"{}\" ({}..{})",
                    &input[cg_range.clone()],
                    cg_range.start,
                    cg_range.end
                ));
            } else {
                result.push_str("None");
            }
        }
        result.push(']');
    }

    result
}

fn run_bt(re: &Regex, input: &str) {
    let mut matches = re.find_iter(input);
    if let Some(res) = matches.next() {
        let count = 1 + matches.count();
        println!(
            "bt:    Match: {}, total: {}",
            format_match(&res.range, &res.captures, input),
            count
        );
    } else {
        println!("bt:    No match");
    }
}

#[cfg(feature = "backend-pikevm")]
fn run_pikevm(re: &Regex, input: &str) {
    use regress::backends::PikeVMExecutor;
    let mut matches = regress::backends::find::<PikeVMExecutor>(re, input, 0);
    if let Some(res) = matches.next() {
        let count = 1 + matches.count();
        println!(
            "pikevm: Match: {}, total: {}",
            format_match(&res.range, &res.captures, input),
            count
        );
    } else {
        println!("pikevm: No match");
    }
}

#[cfg(feature = "nfa")]
fn run_tnfa(nfa: &Nfa, input: &str) {
    match nfa_backend::execute(nfa, input.as_bytes()) {
        Some(m) => {
            println!(
                "tnfa:  Match: {}, total: 1",
                format_match(&m.range, &m.captures, input)
            );
        }
        None => println!("tnfa:  No match"),
    }
}

#[cfg(feature = "nfa")]
fn run_tdfa(tdfa: &Tdfa, input: &str) {
    match tdfa_backend::execute(tdfa, input.as_bytes()) {
        Some(m) => {
            println!(
                "tdfa:  Match: {}, total: 1",
                format_match(&m.range, &m.captures, input)
            );
        }
        None => println!("tdfa:  No match"),
    }
}

fn bench_re_on_path(re: &Regex, path: &Path) {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            println!("{}: {}", err, path.display());
            return;
        }
    };
    let input = contents.as_str();
    re.find_iter(input).count();
    let start = Instant::now();
    for _ in 0..25 {
        re.find_iter(input).count();
    }
    let duration = start.elapsed();
    println!("{} ms", duration.as_millis());
}

#[cfg(feature = "nfa")]
#[path = "dfa_display.rs"]
mod dfa_display;

#[cfg(feature = "nfa")]
#[path = "tdfa_display.rs"]
mod tdfa_display;

fn main() -> Result<(), Error> {
    let args = Opt::from_args();

    let mut backends_list: Vec<Backend> = args
        .exec
        .iter()
        .map(|name| Backend::parse(name).unwrap_or_else(|| panic!("unknown executor: {}", name)))
        .collect();
    if backends_list.is_empty() {
        backends_list.push(Backend::Bt);
    }

    let wants_nfa = backends_list
        .iter()
        .any(|b| matches!(b, Backend::Tnfa | Backend::Tdfa))
        || args.dump_nfa
        || args.dump_nfa_dot
        || args.dump_dfa
        || args.dump_dfa_dot;

    let flags = args.flags.unwrap_or_default();
    let mut ire = backends::try_parse(args.pattern.chars().map(u32::from), flags)?;
    if args.dump_phases || args.dump_unoptimized_ir {
        println!("Unoptimized IR:\n{}", ire);
    }

    if wants_nfa || args.optimize {
        backends::optimize(&mut ire);
        if args.dump_phases || args.dump_optimized_ir {
            println!("Optimized IR:\n{}", ire);
        }
    }

    // Lazy build caches. None = not attempted; Some(Err) = built but failed.
    let mut regex_cache: Option<Regex> = None;
    #[cfg(feature = "nfa")]
    let mut nfa_cache: Option<Result<Nfa, String>> = None;
    #[cfg(feature = "nfa")]
    let mut tdfa_cache: Option<Result<Tdfa, String>> = None;

    // Dumps. Each triggers its own build via the cache.
    if args.dump_nfa {
        #[cfg(feature = "nfa")]
        {
            if nfa_cache.is_none() {
                nfa_cache = Some(Nfa::try_from(&ire).map_err(|e| format_nfa_error(&e)));
            }
            match nfa_cache.as_ref().unwrap() {
                Ok(nfa) => println!("NFA:\n{}", nfa.to_readable_string()),
                Err(msg) => println!("Failed to generate NFA: {}", msg),
            }
        }
        #[cfg(not(feature = "nfa"))]
        println!("NFA backend not available. Compile with --features nfa");
    }
    if args.dump_nfa_dot {
        #[cfg(feature = "nfa")]
        {
            if nfa_cache.is_none() {
                nfa_cache = Some(Nfa::try_from(&ire).map_err(|e| format_nfa_error(&e)));
            }
            match nfa_cache.as_ref().unwrap() {
                Ok(nfa) => print!("{}", nfa.to_dot_string()),
                Err(msg) => println!("Failed to generate NFA: {}", msg),
            }
        }
        #[cfg(not(feature = "nfa"))]
        println!("NFA backend not available. Compile with --features nfa");
    }
    if args.dump_dfa {
        #[cfg(feature = "nfa")]
        {
            if tdfa_cache.is_none() {
                if nfa_cache.is_none() {
                    nfa_cache = Some(Nfa::try_from(&ire).map_err(|e| format_nfa_error(&e)));
                }
                tdfa_cache = Some(match nfa_cache.as_ref().unwrap() {
                    Ok(nfa) => Tdfa::try_from(nfa).map_err(|e| format!("{:?}", e)),
                    Err(msg) => Err(format!("NFA build failed: {}", msg)),
                });
            }
            match tdfa_cache.as_ref().unwrap() {
                Ok(tdfa) => println!("{}", tdfa_display::to_readable_string(tdfa)),
                Err(msg) => println!("Failed to generate TDFA: {}", msg),
            }
        }
        #[cfg(not(feature = "nfa"))]
        println!("TDFA backend not available. Compile with --features nfa");
    }
    if args.dump_dfa_dot {
        #[cfg(feature = "nfa")]
        {
            if nfa_cache.is_none() {
                nfa_cache = Some(Nfa::try_from(&ire).map_err(|e| format_nfa_error(&e)));
            }
            match nfa_cache.as_ref().unwrap() {
                Ok(nfa) => match regress::automata::dfa::Dfa::try_from(nfa) {
                    Ok(dfa) => print!("{}", dfa_display::to_dot_string(&dfa)),
                    Err(err) => println!("Failed to generate DFA: {:?}", err),
                },
                Err(msg) => println!("Failed to generate NFA (needed for DFA): {}", msg),
            }
        }
        #[cfg(not(feature = "nfa"))]
        println!("DFA backend not available. Compile with --features nfa");
    }

    // Bytecode dump (always available) — needs the Regex.
    if args.dump_phases || args.dump_bytecode {
        let cr = backends::emit(&ire);
        println!("Bytecode:\n{:#?}", cr);
    }

    // --bench is its own mode — always uses the classical regex, doesn't
    // honor --exec selection.
    if let Some(ref path) = args.bench {
        if regex_cache.is_none() {
            regex_cache = Some(backends::emit(&ire).into());
        }
        bench_re_on_path(regex_cache.as_ref().unwrap(), path);
        return Ok(());
    }

    // --exec dispatch path.
    {
        let inputs: Vec<String> = if let Some(ref path) = args.file {
            match fs::read_to_string(path) {
                Ok(contents) => vec![contents],
                Err(err) => {
                    println!("{}: {}", err, path.display());
                    std::process::exit(1);
                }
            }
        } else {
            args.inputs.clone()
        };

        for input in &inputs {
            for &backend in &backends_list {
                match backend {
                    Backend::Bt => {
                        if regex_cache.is_none() {
                            regex_cache = Some(backends::emit(&ire).into());
                        }
                        run_bt(regex_cache.as_ref().unwrap(), input);
                    }
                    Backend::PikeVm => {
                        #[cfg(feature = "backend-pikevm")]
                        {
                            if regex_cache.is_none() {
                                regex_cache = Some(backends::emit(&ire).into());
                            }
                            run_pikevm(regex_cache.as_ref().unwrap(), input);
                        }
                        #[cfg(not(feature = "backend-pikevm"))]
                        println!("pikevm: not available (compile with --features backend-pikevm)");
                    }
                    Backend::Tnfa => {
                        #[cfg(feature = "nfa")]
                        {
                            if nfa_cache.is_none() {
                                nfa_cache =
                                    Some(Nfa::try_from(&ire).map_err(|e| format_nfa_error(&e)));
                            }
                            match nfa_cache.as_ref().unwrap() {
                                Ok(nfa) => run_tnfa(nfa, input),
                                Err(msg) => println!("tnfa:  build failed: {}", msg),
                            }
                        }
                        #[cfg(not(feature = "nfa"))]
                        println!("tnfa:  not available (compile with --features nfa)");
                    }
                    Backend::Tdfa => {
                        #[cfg(feature = "nfa")]
                        {
                            if tdfa_cache.is_none() {
                                if nfa_cache.is_none() {
                                    nfa_cache =
                                        Some(Nfa::try_from(&ire).map_err(|e| format_nfa_error(&e)));
                                }
                                tdfa_cache = Some(match nfa_cache.as_ref().unwrap() {
                                    Ok(nfa) => Tdfa::try_from(nfa).map_err(|e| format!("{:?}", e)),
                                    Err(msg) => Err(format!("NFA build failed: {}", msg)),
                                });
                            }
                            match tdfa_cache.as_ref().unwrap() {
                                Ok(tdfa) => run_tdfa(tdfa, input),
                                Err(msg) => println!("tdfa:  build failed: {}", msg),
                            }
                        }
                        #[cfg(not(feature = "nfa"))]
                        println!("tdfa:  not available (compile with --features nfa)");
                    }
                }
            }
        }
    }
    Ok(())
}
