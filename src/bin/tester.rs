use std::env;
use std::fs;
use std::time::Instant;

fn print_usage(n: &str) {
    println!("Usage: {} <pattern> <flags> <input>...", n);
    println!("       {} <pattern> <flags>  --file <file_input>...", n);
    println!("       {} <pattern> <flags>  --bench <file_input>...", n);
}

fn format_match(r: &regress::Match, input: &str) -> String {
    let mut result = input[r.total()].to_string();
    for cg in r.captures.iter() {
        result.push(',');
        if let Some(cg) = cg {
            result.push_str(&input[cg.clone()])
        }
    }
    result
}

fn exec_re_on_string(re: &regress::Regex, input: &str) {
    let mut matches = re.find_iter(input);
    if let Some(res) = matches.next() {
        let count = 1 + matches.count();
        println!(
            "Match: {}, total: {}",
            format_match(&res, &input),
            1 + count
        );
    } else {
        println!("No match");
    }
}

fn exec_re_on_path(re: &regress::Regex, path: &str) {
    match fs::read_to_string(path) {
        Ok(contents) => exec_re_on_string(re, contents.as_str()),
        Err(err) => println!("{}: {}", err, path),
    }
}

fn bench_re_on_path(re: &regress::Regex, path: &str) {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            println!("{}: {}", err, path);
            return;
        }
    };
    let input = contents.as_str();
    // Warmup
    let _ = re.find_iter(input).count();
    let start = Instant::now();
    for _ in 0..25 {
        let _ = re.find_iter(input).count();
    }
    let duration = start.elapsed();
    println!("{} ms", duration.as_millis());
}

fn main() -> Result<(), regress::Error> {
    let mut args = env::args();

    // argv[0] is program name.
    let proc_name = args.next().unwrap_or_default();

    // argv[1] is the pattern.
    let pattern = args.next().unwrap_or_else(|| {
        print_usage(&proc_name);
        std::process::exit(1)
    });

    // argv[2] are flags.
    let mut flags = regress::Flags::default();
    if let Some(flags_str) = args.next() {
        flags = regress::Flags::from(flags_str.as_str());
    }
    #[cfg(feature = "dump-phases")]
    {
        flags.dump_init_ir = true;
        flags.dump_opt_ir = true;
        flags.dump_bytecode = true;
    }
    let re = regress::Regex::with_flags(pattern.as_str(), flags)?;

    loop {
        match args.next() {
            None => break,
            Some(s) if s == "--file" => {
                let path = args.next().expect("No file provided");
                exec_re_on_path(&re, path.as_str());
            }
            Some(s) if s == "--bench" => {
                let path = args.next().expect("No file provided");
                bench_re_on_path(&re, path.as_str());
            }
            Some(s) => {
                exec_re_on_string(&re, s.as_str());
            }
        }
    }
    Ok(())
}
