//! Alternation-prefix (`AltPrefix`) strategy tests.
//!
//! An alternation whose every branch has a leading literal prefix
//! (`Sher[a-z]+|Hol[a-z]+`, with or without `/i`, or a case-insensitive literal
//! alternation `Sherlock|Holmes`/i) selects a Teddy prefilter over the union of
//! the branches' leading case cross-products, then verifies each candidate with
//! the anchored automaton. Cross-checked against the classical backtracker over
//! inputs that stress branch selection, greedy tails, the ſ fold, and captures.

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

/// Assert the pattern selects the alternation-prefix strategy and agrees with
/// the backtracker (match extents) on every input.
#[track_caller]
fn check(pattern: &str, flags: &str, inputs: &[&str]) {
    let re = parse_ir(pattern, flags);
    let prog = TdfaProgram::try_from_ir(&re).expect("program build failed");
    assert!(
        prog.is_alt_prefix(),
        "pattern `{pattern}`/{flags} did not select the alt-prefix strategy"
    );
    for &input in inputs {
        assert_eq!(
            tdfa_matches(&prog, input),
            backtrack_matches(pattern, flags, input),
            "mismatch for `{pattern}`/{flags} on input {input:?}"
        );
    }
}

#[test]
fn prefix_branches_with_tail() {
    check(
        r"Sher[a-z]+|Hol[a-z]+",
        "",
        &[
            "",
            "Sherlock met Holmes",
            "Sher",            // tail needs ≥1 — no match
            "Holmes Sherlock", // both branches, order
            "no match here",
            "HolaHolaSherb",
        ],
    );
}

#[test]
fn prefix_branches_with_tail_icase() {
    // The crushed `name_alt4_nocase` case: case-insensitive prefixes + greedy
    // tail. Includes a long-s (ſher…) leading fold.
    check(
        r"Sher[a-z]+|Hol[a-z]+",
        "i",
        &[
            "SHERLOCK and holmes",
            "ſherlock",        // long-s prefix fold
            "hOlMeS sHeRb",
            "her hol sher",    // bare `hol`/`sher` then non-letter
        ],
    );
}

#[test]
fn icase_literal_alternation() {
    // A case-insensitive *literal* alternation: branches are whole literals, but
    // the icase fold sets keep it out of the (case-sensitive) MultiLiteral path.
    check(
        r"Sherlock|Holmes|Watson",
        "i",
        &["sherlock HOLMES watson", "WatsonwatsonHolmes", "no names"],
    );
}

#[test]
fn capturing_tail() {
    // A capture group after the prefix exercises the automaton verify's capture
    // path (the prefix is still literal, so alt-prefix is selected).
    check(
        r"Sher([a-z]+)|Hol([a-z]+)",
        "",
        &["Sherlock", "Holmes and Sherlock", "Sher x Hola"],
    );
}
