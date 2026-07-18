//! Size-aware strategy selection tests (the Scan → `Prefix` fallback).
//!
//! A bounded repeat whose body overlaps the pattern's first byte (`a.{12}b`)
//! makes the unanchored subset construction track every pending start in the
//! window, so the Scan automaton's states grow as 2^window. `try_from_ir`
//! then falls back to `Strategy::Prefix` on the start predicate that
//! `should_prefilter` had rejected as too common — and does the same when the
//! unanchored build exceeds the TDFA state budget outright, where the
//! anchored verify is the only way to build the pattern at all. The fallback
//! only changes *how* we search, never *what* matches: every case here is
//! cross-checked against the backtracker oracle.

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

fn build(pattern: &str) -> TdfaProgram {
    TdfaProgram::try_from_ir(&parse_ir(pattern)).expect("build")
}

/// Assert the program agrees with the backtracker on every input.
fn assert_agrees(pattern: &str, prog: &TdfaProgram, inputs: &[&str]) {
    for &input in inputs {
        assert_eq!(
            tdfa_matches(prog, input),
            backtrack_matches(pattern, input),
            "pattern {pattern:?} disagreed on {input:?}",
        );
    }
}

/// Inputs that stress the window: candidate starts at every position, matches
/// at the start/middle/end, rejected candidates before a real match, `.`-
/// excluded terminators (`\n`, U+2028) inside the window, multibyte window
/// contents, and inputs shorter than the window.
const WINDOW_INPUTS: &[&str] = &[
    "",
    "ab",
    "a123456789012b",
    "xxa123456789012b_a123456789012byy",
    "aaaaaaaaaaaaaaab",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaab",
    "a12345678901xb no, then a123456789012b yes",
    "a12345678901\nb",
    "a\u{2028}23456789012b",
    "aαβγδεζηθικλμb",
    "ααααaαβγδεζηθικλμbαααα",
];

#[test]
fn window_blowup_selects_prefix() {
    let pattern = "a.{12}b";
    let prog = build(pattern);
    assert!(prog.is_prefix(), "expected the Prefix fallback for {pattern:?}");
    assert_agrees(pattern, &prog, WINDOW_INPUTS);
}

#[test]
fn window_blowup_with_captures_selects_prefix() {
    let pattern = "a(.{12})b";
    let prog = build(pattern);
    assert!(prog.is_prefix());
    assert_agrees(pattern, &prog, WINDOW_INPUTS);
}

/// `a.{16}b` overflows the TDFA state budget as a Scan automaton
/// (~11·2^16 states); the budget-exceeded fallback builds it as `Prefix`.
#[test]
fn over_budget_falls_back_to_prefix() {
    let pattern = "a.{16}b";
    let prog = build(pattern);
    assert!(prog.is_prefix());
    assert_agrees(
        pattern,
        &prog,
        &[
            "",
            "a1234567890123456b",
            "xxa1234567890123456b and a1234567890123456byy",
            "aaaaaaaaaaaaaaaaaab",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab",
            "a123456789012345\nb",
        ],
    );
}

/// A small window determinizes to a handful of states: the common-byte
/// predicate stays rejected and the single-pass scan remains the strategy.
#[test]
fn small_window_stays_scan() {
    let prog = build("a.{2}b");
    assert!(prog.is_scan());
}

/// An unbounded-width pattern keeps the scan even when its automaton is
/// large: a dead prefilter candidate could cost O(n) to verify, so the
/// linear single pass is the safer machine.
#[test]
fn unbounded_width_stays_scan() {
    let prog = build(r"a.{12}b\w*");
    assert!(prog.is_scan());
}
