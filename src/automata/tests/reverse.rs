//! Reverse-automaton (required interior/suffix literal) strategy tests.
//!
//! Each case drives the `ReverseInner` strategy (memmem the literal, walk the
//! reversed *prefix* DFA back to the start, forward-verify) and checks it against
//! the classical backtracker — the reference engine — over adversarial inputs
//! that stress leftmost-match selection across multiple literal occurrences. The
//! literal may be interior (`(\w+)'(\w+)`), with the part after it verified by
//! the forward run, not just a suffix.

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
    // Lowercase prefix is unselective (`should_prefilter` rejects it), so this
    // routes to reverse-inner on the suffix literal.
    check(
        r"[a-z]+99",
        &["ab99", "x99 yy99", "99", "abc 3099 def z99", "aa99bb99"],
    );
}

#[test]
fn selective_charclass_run_before_literal() {
    // Even a *selective* leading class (`[0-9]` passes `should_prefilter`) defers
    // to reverse-inner when a required multi-byte literal exists: a `ByteBracket`
    // overlapping the leading `+` loop makes every byte of a run a candidate
    // (O(n²) on a class-heavy input), whereas `memmem` on the literal stays
    // selective. A single-byte interior literal (the `.` in a dotted quad) does
    // *not* trigger this — it's no better than the class scan — so those keep the
    // prefix strategy (covered in `prefix_skip` / `litwindow`).
    check(
        r"[0-9]+foo",
        &["12foo", "x9foo 7foo", "foo", "000foo bar 99foo", "1foo2foo"],
    );
    check(r"[0-9]+99", &["12399", "9999", "x1299 y99", "99"]);
}

#[test]
fn multichar_overlapping_suffix() {
    // Overlapping occurrences of the suffix literal ("aa" in "aaa").
    check(r"x+aa", &["xaa", "xaaa", "xaaaa", "aa", "xxaa yaa", "xaaxaa"]);
}

#[test]
fn inner_literal_contraction() {
    // The motivating interior-literal case: a single-byte `'` between two word
    // runs, with the part *after* the literal verified by the forward run.
    check(
        r"(\w+)'(\w+)",
        &[
            "",
            "don't",
            "it's a can't won't",
            "'leading",       // no word before — no match
            "trailing'",      // no word after — no match
            "a'b'c",          // overlapping candidates around adjacent quotes
            "say it's, y'all",
        ],
    );
}

#[test]
fn inner_literal_with_tail() {
    // Interior literal `@`, then a non-trivial suffix the forward run must match.
    check(
        r"\w+@\w+\.\w+",
        &["a@b.co", "x y@z.org w", "no-at-here", "u@v", "p@q.r s@t.uv"],
    );
}
