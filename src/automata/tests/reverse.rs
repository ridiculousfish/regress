//! Reverse-automaton (required-suffix-literal) strategy tests.
//!
//! Each case drives the `ReverseInner` strategy (memmem the suffix, walk the
//! reverse DFA back to the start, forward-verify) and checks it against the
//! classical backtracker — the reference engine — over adversarial inputs that
//! stress leftmost-match selection across multiple literal occurrences.

use crate::Flags;
use crate::automata::executors::TdfaExecutor;
use crate::automata::prefilter::TdfaProgram;

fn parse_ir(pattern: &str) -> crate::ir::Regex {
    let flags = Flags {
        unicode: true,
        ..Default::default()
    };
    let mut re =
        crate::backends::try_parse(pattern.chars().map(u32::from), flags).expect("parse failed");
    crate::optimizer::optimize(&mut re);
    re
}

/// Matches via the TDFA program (should be the reverse-inner strategy).
fn tdfa_matches(prog: &TdfaProgram, input: &str) -> Vec<(usize, usize)> {
    crate::backends::find::<TdfaExecutor>(prog, input, 0)
        .map(|m| (m.range.start, m.range.end))
        .collect()
}

/// Oracle: matches via the classical backtracker.
fn backtrack_matches(pattern: &str, input: &str) -> Vec<(usize, usize)> {
    let re = parse_ir(pattern);
    let cr = crate::backends::emit(&re);
    crate::backends::find::<crate::backends::BacktrackExecutor>(&cr, input, 0)
        .map(|m| (m.range.start, m.range.end))
        .collect()
}

/// Assert the pattern uses the reverse-inner strategy and agrees with the
/// backtracker on every input.
#[track_caller]
fn check(pattern: &str, inputs: &[&str]) {
    let re = parse_ir(pattern);
    let prog = TdfaProgram::try_from_ir(&re).expect("program build failed");
    assert!(
        prog.is_reverse_inner(),
        "pattern `{pattern}` did not select the reverse-inner strategy"
    );
    for &input in inputs {
        assert_eq!(
            tdfa_matches(&prog, input),
            backtrack_matches(pattern, input),
            "mismatch for pattern `{pattern}` on input {input:?}"
        );
    }
}

#[test]
fn word_run_before_literal() {
    // The motivating case: no usable prefix literal, required suffix "Holmes".
    check(
        r"\w+\s+Holmes",
        &[
            "",
            "Holmes",
            "say Holmes",
            "a Holmes b Holmes",
            "xHolmes Holmes",
            "Holmes Holmes Holmes",
            "  Holmes",
            "word Holmes tail",
            "Holmesx yy Holmes",
            "aaaaaaaa   Holmes!!! and then bb Holmes",
        ],
    );
}

#[test]
fn one_or_more_then_literal() {
    check(
        r"a+bc",
        &[
            "aabc",
            "aabc xabc",
            "bc",
            "abcabc",
            "aaaaabc",
            "zabc abc aabc",
            "xbc",
        ],
    );
}

#[test]
fn greedy_dotstar_before_literal_takes_last() {
    // Greedy unbounded PRE: the reverse finds a start, but the forward run must
    // fix the (greedy) end at the LAST literal occurrence.
    check(
        r".*END",
        &["END", "END END", "a END b END", "no terminator", "ENDEND"],
    );
}

#[test]
fn charclass_run_before_literal() {
    // Lowercase prefix is unselective, so this routes to reverse-inner (a
    // selective prefix like `[0-9]+` would correctly use the Phase-1 prefilter).
    check(
        r"[a-z]+99",
        &["ab99", "x99 yy99", "99", "abc 3099 def z99", "aa99bb99"],
    );
}

#[test]
fn multichar_overlapping_suffix() {
    // Overlapping occurrences of the suffix literal ("aa" in "aaa").
    check(r"x+aa", &["xaa", "xaaa", "xaaaa", "aa", "xxaa yaa", "xaaxaa"]);
}
