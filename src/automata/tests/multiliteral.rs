//! Literal-alternation (`MultiLiteral`) strategy tests.
//!
//! A pattern that is exactly an alternation of literals (`Sherlock|Holmes|…`)
//! selects the aho-corasick Teddy multi-substring prefilter and emits matches
//! straight from the searcher — no automaton. Each case is cross-checked against
//! the classical backtracker oracle over inputs that stress leftmost-first
//! priority, overlapping/prefix alternatives, and adjacency.

use crate::Flags;
use crate::automata::executors::TdfaExecutor;
use crate::automata::prefilter::TdfaProgram;

fn parse_ir(pattern: &str) -> crate::ir::Regex {
    let mut flags = Flags::default();
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

fn backtrack_matches(pattern: &str, input: &str) -> Vec<(usize, usize)> {
    let re = parse_ir(pattern);
    let cr = crate::backends::emit(&re);
    crate::backends::find::<crate::backends::BacktrackExecutor>(&cr, input, 0)
        .map(|m| (m.range.start, m.range.end))
        .collect()
}

/// Assert the pattern selects the multi-literal strategy and agrees with the
/// backtracker on every input.
#[track_caller]
fn check(pattern: &str, inputs: &[&str]) {
    let re = parse_ir(pattern);
    let prog = TdfaProgram::try_from_ir(&re).expect("program build failed");
    assert!(
        prog.is_multi_literal(),
        "pattern `{pattern}` did not select the multi-literal strategy"
    );
    for &input in inputs {
        assert_eq!(
            tdfa_matches(&prog, input),
            backtrack_matches(pattern, input),
            "mismatch for `{pattern}` on input {input:?}"
        );
    }
}

#[test]
fn names_alternation() {
    check(
        "Sherlock|Holmes|Watson|Irene|Adler|John|Baker",
        &[
            "",
            "no names here",
            "Sherlock Holmes and Watson met John Baker",
            "AdlerIreneJohn",       // adjacent, back-to-back
            "WatsonWatsonWatson",   // repeated
            "sherlock holmes",      // case-sensitive: no match
            "say BakerBakerBaker!",
        ],
    );
}

#[test]
fn leftmost_first_priority() {
    // Overlapping alternatives at the same start: ECMAScript takes the *first*
    // listed that matches (ordered choice), not the longest. The oracle pins the
    // exact behavior either way.
    check("He|Hello", &["Hello there", "He said Hello", "HelloHe"]);
    check("Hello|He", &["Hello there", "He said Hello", "HelloHe"]);
    check("foo|foobar|foof", &["foobar", "foofoobar", "a foof b"]);
}

#[test]
fn shared_prefix_and_lengths() {
    // Shared prefixes and mixed lengths — exercises Teddy's bucketing and the
    // span (start..end) the match reports.
    check(
        "Sher|Sherlock|Street",
        &["Sherlock on Street", "Sher Sherlock", "Streets of Sher"],
    );
}
