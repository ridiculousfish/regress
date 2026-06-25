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

/// Assert the pattern agrees with the backtracker on every input. When
/// `expect_multi`, also assert it selected the multi-literal strategy — only for
/// alternations with diverse first bytes; alternatives that share a single rare
/// first byte are (correctly) routed to the single-`memchr` `Prefix` strategy
/// instead (see `teddy_preferred`), so those just check the oracle.
#[track_caller]
fn check_with(pattern: &str, expect_multi: bool, inputs: &[&str]) {
    let re = parse_ir(pattern);
    let prog = TdfaProgram::try_from_ir(&re).expect("program build failed");
    if expect_multi {
        assert!(
            prog.is_multi_literal(),
            "pattern `{pattern}` did not select the multi-literal strategy"
        );
    }
    for &input in inputs {
        assert_eq!(
            tdfa_matches(&prog, input),
            backtrack_matches(pattern, input),
            "mismatch for `{pattern}` on input {input:?}"
        );
    }
}

#[track_caller]
fn check(pattern: &str, inputs: &[&str]) {
    check_with(pattern, true, inputs);
}

/// Oracle-only (strategy-agnostic): for alternations that share a first byte.
#[track_caller]
fn check_oracle(pattern: &str, inputs: &[&str]) {
    check_with(pattern, false, inputs);
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
    // exact behavior either way. `He|Hello` shares a rare first byte (→ Prefix);
    // `foo|…` shares a common one (`f`), which stays multi-literal.
    check_oracle("He|Hello", &["Hello there", "He said Hello", "HelloHe"]);
    check_oracle("Hello|He", &["Hello there", "He said Hello", "HelloHe"]);
    check("foo|foobar|foof", &["foobar", "foofoobar", "a foof b"]);
}

#[test]
fn shared_rare_first_byte_routes_to_prefix() {
    // `Sherlock|Street` share the single rare byte `S`: a memchr on `S` beats
    // Teddy, so the gate routes it *away* from multi-literal. Diverse first bytes
    // (`Sherlock|Holmes`) keep Teddy. Both must still match the oracle.
    let p1 = TdfaProgram::try_from_ir(&parse_ir("Sherlock|Street")).unwrap();
    assert!(!p1.is_multi_literal(), "shared rare `S` should not use Teddy");
    let p2 = TdfaProgram::try_from_ir(&parse_ir("Sherlock|Holmes")).unwrap();
    assert!(p2.is_multi_literal(), "diverse first bytes should use Teddy");
    check_oracle("Sherlock|Street", &["a Sherlock and a Street", "Streetlock"]);
}

#[test]
fn shared_prefix_and_lengths() {
    // Shared prefixes and mixed lengths — all start with the rare byte `S`, so
    // this routes to the single-`memchr` Prefix strategy; the span the match
    // reports is still checked against the oracle.
    check_oracle(
        "Sher|Sherlock|Street",
        &["Sherlock on Street", "Sher Sherlock", "Streets of Sher"],
    );
}
