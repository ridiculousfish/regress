//! Interior-literal window filter tests (the `Prefix` strategy's cheap secondary
//! filter).
//!
//! A pattern with an unselective leading byte-class prefilter but a *required
//! interior literal* at a bounded offset — the canonical case is the dotted-quad
//! `(?:[0-9]{1,3}\.){3}[0-9]{1,3}`, whose `[0-9]` prefilter stops at every digit
//! but whose match must contain a `.` within 1–3 bytes of the start — rejects
//! most prefilter candidates with an inline window scan before the per-candidate
//! verify. The filter is only a *necessary* condition, so it must never drop a
//! real match: every case here is cross-checked against the backtracker oracle.

use crate::Flags;
use crate::automata::executors::TdfaExecutor;
use crate::automata::prefilter::TdfaProgram;

fn parse_ir(pattern: &str) -> crate::ir::Regex {
    let mut re =
        crate::backends::try_parse(pattern.chars().map(u32::from), Flags::default()).expect("parse");
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

/// Assert the TDFA `Prefix`-pipeline agrees with the backtracker on every input.
fn assert_agrees(pattern: &str, inputs: &[&str]) {
    let re = parse_ir(pattern);
    let prog = TdfaProgram::try_from_ir(&re).expect("build");
    for &input in inputs {
        assert_eq!(
            tdfa_matches(&prog, input),
            backtrack_matches(pattern, input),
            "pattern {pattern:?} disagreed on {input:?}",
        );
    }
}

#[test]
fn ip_dotted_quad() {
    assert_agrees(
        r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}",
        &[
            "",
            "no digits here at all",
            "lone digits 5 6 7 not an ip",
            "12.34 partial 1.2.3 still partial",
            "visit 192.168.0.1 and 8.8.8.8 now",
            "edge 1.2.3.4 at end 10.0.0.255",
            "1.1.1.1",                   // match at offset 0
            "...... 0.0.0.0 ......",     // literal dots around the match
            "year 1887 page 42 3.1 1.2.3..4", // near-misses: double dot, short
            "back2back 1.2.3.4.5.6.7.8 overlap",
            "255.255.255.255",
            "a1.2.3.4b digit not at word start",
        ],
    );
}

#[test]
fn interior_literal_after_bounded_run() {
    // `[A-Z][0-9]{2}-[0-9]+`: prefilter `[A-Z]` (uppercase, uncommon), required
    // interior literal `-` at a fixed offset 3. Exercises a non-dot literal and a
    // single-valued window.
    assert_agrees(
        r"[A-Z][0-9]{2}-[0-9]+",
        &["", "abc", "X12 no dash", "code X12-345 here", "AA00-1 Z99-0 end"],
    );
}

#[test]
fn no_false_negatives_when_filter_absent() {
    // A pattern with no bounded interior literal (`[0-9]+` only) must still match
    // correctly — `leading_required_byte` returns `None`, leaving the plain
    // prefilter scan.
    assert_agrees(r"[0-9]+", &["", "abc 123 def 4567 z", "0"]);
}
