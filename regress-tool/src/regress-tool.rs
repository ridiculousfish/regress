#![allow(clippy::uninlined_format_args)]

use regress::backends::Nfa;
use regress::{backends, Error, Flags, Regex};

#[cfg(feature = "nfa")]
use regress::automata::nfa_backend;
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

    /// Use NFA backend for execution.
    #[structopt(long)]
    nfa: bool,

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

fn format_nfa_error(err: &regress::backends::NfaError) -> String {
    match err {
        regress::backends::NfaError::UnsupportedInstruction(desc) => {
            format!("Unsupported instruction: {}", desc)
        }
        regress::backends::NfaError::BudgetExceeded => {
            "Budget exceeded (too many states)".to_string()
        }
    }
}

fn format_match(r: &regress::Match, input: &str) -> String {
    let mut result = String::new();
    
    // Show the full matched range
    result.push_str(&format!("\"{}\" ({}..{})", 
                            &input[r.range()], 
                            r.range().start, 
                            r.range().end));
    
    // Show capture groups if any exist
    if !r.captures.is_empty() {
        result.push_str(", captures: [");
        for (i, cg) in r.captures.iter().enumerate() {
            if i > 0 {
                result.push_str(", ");
            }
            if let Some(cg_range) = cg {
                result.push_str(&format!("\"{}\" ({}..{})", 
                                       &input[cg_range.clone()],
                                       cg_range.start,
                                       cg_range.end));
            } else {
                result.push_str("None");
            }
        }
        result.push(']');
    }
    
    result
}

fn exec_re_on_string(re: &Regex, input: &str) {
    let mut matches = re.find_iter(input);
    if let Some(res) = matches.next() {
        let count = 1 + matches.count();
        println!("Match: {}, total: {}", format_match(&res, input), count);
    } else {
        println!("No match");
    }
}

#[cfg(feature = "nfa")]
fn exec_nfa_on_string(nfa: &Nfa, input: &str) {
    match nfa_backend::execute_nfa(nfa, input.as_bytes()) {
        Some(nfa_match) => {
            let matched_text = &input[nfa_match.range.clone()];
            println!("Match: \"{}\" ({}..{}), total: 1", 
                    matched_text, 
                    nfa_match.range.start, 
                    nfa_match.range.end);
        }
        None => {
            println!("No match");
        }
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
    // Warmup
    re.find_iter(input).count();
    let start = Instant::now();
    for _ in 0..25 {
        re.find_iter(input).count();
    }
    let duration = start.elapsed();
    println!("{} ms", duration.as_millis());
}

fn main() -> Result<(), Error> {
    let args = Opt::from_args();

    let flags = args.flags.unwrap_or_default();
    let mut ire = backends::try_parse(args.pattern.chars().map(u32::from), flags)?;
    if args.dump_phases || args.dump_unoptimized_ir {
        println!("Unoptimized IR:\n{}", ire);
    }

    if args.nfa || args.optimize {
        backends::optimize(&mut ire);
        if args.dump_phases || args.dump_optimized_ir {
            println!("Optimized IR:\n{}", ire);
        }
    }

    if args.nfa && !cfg!(feature = "nfa") {
        println!("NFA backend not available. Compile with --features nfa");
        std::process::exit(1);
    }

    #[cfg(feature = "nfa")]
    if args.nfa {
        let nfa = Nfa::try_from(&ire);
        let nfa = match nfa {
            Ok(nfa) => nfa,
            Err(err) => {
                println!("Failed to generate NFA: {}", format_nfa_error(&err));
                std::process::exit(1);
            }
        };
        if args.dump_nfa {
            println!("NFA:\n{}", nfa.to_readable_string());
        }
        if let Some(ref path) = args.file {
            match fs::read_to_string(path) {
                Ok(contents) => exec_nfa_on_string(&nfa, contents.as_str()),
                Err(err) => println!("{}: {}", err, path.display()),
            };
        } else {
            for input in args.inputs {
                exec_nfa_on_string(&nfa, &input);
            }
        }
        return Ok(());
    }

    let cr = backends::emit(&ire);
    if args.dump_phases || args.dump_bytecode {
        println!("Bytecode:\n{:#?}", cr);
    }
    let re = cr.into();

    if let Some(ref path) = args.file {
        match fs::read_to_string(path) {
            Ok(contents) => exec_re_on_string(&re, contents.as_str()),
            Err(err) => println!("{}: {}", err, path.display()),
        };
    } else if let Some(ref path) = args.bench {
        bench_re_on_path(&re, path);
    } else {
        for input in args.inputs {
            exec_re_on_string(&re, &input);
        }
    }
    Ok(())
}
