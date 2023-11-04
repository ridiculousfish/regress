use regress::{backends, Error, Flags, Regex};
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

fn format_match(r: &regress::Match, input: &str) -> String {
    let mut result = input[r.range()].to_string();
    for cg in r.captures.iter() {
        result.push(',');
        if let Some(cg) = cg {
            result.push_str(&input[cg.clone()])
        }
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

    if args.optimize {
        backends::optimize(&mut ire);
        if args.dump_phases || args.dump_optimized_ir {
            println!("Optimized IR:\n{}", ire);
        }
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
