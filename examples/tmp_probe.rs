fn main() {
    let pattern = std::env::args().nth(1).unwrap();
    let flags = regress::Flags::default();
    let mut ire = regress::backends::try_parse(pattern.chars().map(u32::from), flags).unwrap();
    regress::backends::optimize(&mut ire);
    let nfa = regress::backends::Nfa::try_from_unanchored(&ire).unwrap();
    let t0 = std::time::Instant::now();
    match regress::backends::Tdfa::try_from(&nfa) {
        Ok(t) => println!("states={} elapsed={:?}", t.num_states(), t0.elapsed()),
        Err(e) => println!("build failed: {e:?} elapsed={:?}", t0.elapsed()),
    }
}
