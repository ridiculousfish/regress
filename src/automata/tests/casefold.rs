//! Case-insensitive-literal (`CaseFoldLiteral`) strategy tests.
//!
//! Each case drives the fold-clean-run search + start-window automaton verify
//! and checks it against the classical backtracker over inputs that stress
//! mixed case, the width-changing unicode folds (ſ→s, Kelvin→k), and leftmost
//! selection across multiple occurrences.

use crate::Flags;
use crate::automata::executors::TdfaExecutor;
use crate::automata::prefilter::TdfaProgram;

fn parse_ir(pattern: &str, flags_str: &str) -> crate::ir::Regex {
    let mut flags = Flags::from(flags_str);
    flags.unicode = true;
    let mut re =
        crate::backends::try_parse(pattern.chars().map(u32::from), flags).expect("parse failed");
    crate::optimizer::optimize(&mut re);
    re
}

fn tdfa_matches(prog: &TdfaProgram, input: &str) -> Vec<(usize, usize)> {
    crate::backends::find::<TdfaExecutor>(prog, input, 0)
        .map(|m| (m.range.start, m.range.end))
        .collect()
}

fn backtrack_matches(pattern: &str, flags_str: &str, input: &str) -> Vec<(usize, usize)> {
    let re = parse_ir(pattern, flags_str);
    let cr = crate::backends::emit(&re);
    crate::backends::find::<crate::backends::BacktrackExecutor>(&cr, input, 0)
        .map(|m| (m.range.start, m.range.end))
        .collect()
}

/// Assert the pattern uses the case-fold-literal strategy and agrees with the
/// backtracker on every input.
#[track_caller]
fn check(pattern: &str, inputs: &[&str]) {
    let re = parse_ir(pattern, "i");
    let prog = TdfaProgram::try_from_ir(&re).expect("program build failed");
    assert!(
        prog.is_casefold_literal(),
        "pattern `{pattern}`/i did not select the case-fold-literal strategy"
    );
    for &input in inputs {
        assert_eq!(
            tdfa_matches(&prog, input),
            backtrack_matches(pattern, "i", input),
            "mismatch for `{pattern}`/i on input {input:?}"
        );
    }
}

#[test]
fn whole_icase_literal_mixed_case() {
    check(
        "Sherlock",
        &[
            "",
            "Sherlock",
            "SHERLOCK",
            "sHeRlOcK and sherlock",
            "no match here",
            "say Sherlock Holmes, Sherlock!",
            "sherlocksherlock",
        ],
    );
}

#[test]
fn clean_run_is_prefix() {
    // 'Holmes' icase: clean run "Holme" is a prefix, trailing 's' is the
    // problematic (s→ſ) position.
    check(
        "Holmes",
        &["holmes", "HOLMES HOLMES", "mr Holmes", "holmesx", "aholmes"],
    );
}

#[test]
fn all_common_letters() {
    // "the" — every letter common, no rare anchor: exercises the SWAR pair path.
    check(
        "the",
        &["the", "THE the tHe", "theatre", "breathe", "tttthe", "h t e"],
    );
}

#[test]
fn literal_with_space() {
    check(
        "Sherlock Holmes",
        &[
            "sherlock holmes",
            "SHERLOCK HOLMES!",
            "Sherlock  Holmes", // two spaces — no match
            "x Sherlock Holmes y",
        ],
    );
}

#[test]
fn width_changing_unicode_folds() {
    // The crux: ſ (U+017F, 2 bytes) folds to 's', Kelvin (U+212A, 3 bytes) to
    // 'k'. The match span is wider than the ASCII literal; the automaton verify
    // must handle it. Compared against the backtracker, which is the oracle.
    check(
        "Sherlock",
        &[
            "ſherlock",         // long-s at the start
            "say ſherlock!",    // long-s, offset
            "Sherlocﬀ no",      // (not a fold) ensure no spurious match
            "sherlocK",         // ascii K
        ],
    );
    // 'k'→Kelvin: only relevant if regress folds it; either way match the oracle.
    check("milk", &["MILK", "miﬀ no", "the milk"]);
}

#[test]
fn teddy_dense_near_misses() {
    // Adversarial for the Teddy prefilter: many partial prefixes (`Sh`, `She`,
    // `Sher`) that share the first bytes but don't complete, plus offset and
    // long-s (ſ, mid- and start-of-word) folds. Cross-checked against the oracle
    // so it validates whichever candidate finder is compiled in.
    check(
        "Sherlock",
        &[
            "she shed sheer sherbet sherlock shelf",
            "ſherlock sherlocſ Sherlock", // ſ leads one real match, garbles another
            "sherlocksherlocksherlock",
            "SHERSHERSHERLOCK",
            "no sherl ock here",
        ],
    );
}

/// With `prefilter-teddy` on, the canonical `Sherlock`/i pattern must actually
/// drive candidates through Teddy (not silently fall back to `CaseFoldSearcher`).
#[cfg(feature = "prefilter-teddy")]
#[test]
fn teddy_is_active_for_sherlock() {
    let re = parse_ir("Sherlock", "i");
    let prog = TdfaProgram::try_from_ir(&re).expect("program build failed");
    assert!(prog.uses_teddy(), "Sherlock/i should use the Teddy prefilter");
}
