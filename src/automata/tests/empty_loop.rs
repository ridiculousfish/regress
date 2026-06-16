//! Tests that loops over empty-matching bodies build and execute with
//! V8-matching captures. Implementation uses `EpsCondition::ProgressSince`
//! to gate iteration-exit and back-edge eps once `min` iterations are
//! satisfied — encoding ES2015 RepeatMatcher's "if min == 0 and
//! y.endIndex == x.endIndex, return failure" rule.
//! See `build_loop_nullable`, `Node::can_match_empty`, rust-lang/regex#779.

use crate::Flags;
use crate::automata::nfa::Nfa;
use crate::automata::nfa_backend::execute;
use crate::automata::tdfa::Tdfa;
use crate::automata::tdfa_backend::execute as tdfa_execute;

fn build(pattern: &str) -> Nfa {
    let mut ire = crate::backends::try_parse(pattern.chars().map(u32::from), Flags::default())
        .expect("parse failed");
    crate::backends::optimize(&mut ire);
    Nfa::try_from(&ire).unwrap_or_else(|e| panic!("expected {pattern:?} to build, got {e:?}"))
}

fn assert_accepted(pattern: &str) {
    let _ = build(pattern);
}

#[test]
fn accepts_loops_over_empty_bodies() {
    assert_accepted("(a?)*");
    assert_accepted("(a*)*");
    assert_accepted("(|x)*");
    assert_accepted("(a|)*");
    assert_accepted("(a?)+");
    assert_accepted("(a*){2,5}");
}

#[test]
fn accepts_loops_over_nonempty_bodies() {
    assert_accepted("a*");
    assert_accepted("a+");
    assert_accepted("(ab)*");
    assert_accepted("a{2,5}");
    assert_accepted("[a-z]+");
    assert_accepted("(ab|cd)+");
}

/// Capture expectations cross-checked against V8:
/// `node -e 'JSON.stringify("...".match(/.../))'`.
fn check(pattern: &str, input: &str, expected_caps: &[Option<(usize, usize)>]) {
    let nfa = build(pattern);
    let m = execute(&nfa, input.as_bytes(), 0)
        .unwrap_or_else(|| panic!("expected {pattern:?} to match {input:?}"));
    let actual: Vec<Option<(usize, usize)>> = m
        .captures
        .iter()
        .map(|c| c.as_ref().map(|r| (r.start, r.end)))
        .collect();
    assert_eq!(
        actual, expected_caps,
        "pattern={pattern:?} input={input:?} captures mismatch"
    );
}

#[test]
fn captures_match_v8_for_nullable_loops() {
    check("(a?)+", "", &[Some((0, 0))]);
    check("(a?)+", "a", &[Some((0, 1))]);
    check("(a?)+", "aa", &[Some((1, 2))]);
    check("(a*)*", "", &[None]);
    check("(a*)*", "aaa", &[Some((0, 3))]);
    check("(|x)*", "x", &[Some((0, 1))]);
    check("(a*){2,5}", "aa", &[Some((2, 2))]);
    check("(a*)+", "", &[Some((0, 0))]);
    check("(a*)+", "aaa", &[Some((0, 3))]);
    check("(.{0,2})*", "abc", &[Some((2, 3))]);
    // Non-greedy nullable loops: prefer the empty / skipped path.
    check("(a?)*?", "a", &[None]);
    check("(a*)*?", "aaa", &[None]);
}

/// TDFA construction-time `ProgressSince` resolution: build a TDFA for
/// each nullable-loop case and verify execution captures match V8 too.
/// Coverage parallels the NFA test above to catch divergences between
/// backends.
fn check_tdfa(pattern: &str, input: &str, expected_caps: &[Option<(usize, usize)>]) {
    let nfa = build(pattern);
    let tdfa = Tdfa::try_from(&nfa)
        .unwrap_or_else(|e| panic!("expected TDFA build for {pattern:?}, got {e:?}"));
    let m = tdfa_execute(&tdfa, input.as_bytes(), 0)
        .unwrap_or_else(|| panic!("expected {pattern:?} to match {input:?} on TDFA"));
    let actual: Vec<Option<(usize, usize)>> = m
        .captures
        .iter()
        .map(|c| c.as_ref().map(|r| (r.start, r.end)))
        .collect();
    assert_eq!(
        actual, expected_caps,
        "TDFA: pattern={pattern:?} input={input:?} captures mismatch"
    );
}

#[test]
fn tdfa_captures_match_v8_for_nullable_loops() {
    check_tdfa("(a?)+", "", &[Some((0, 0))]);
    check_tdfa("(a?)+", "a", &[Some((0, 1))]);
    check_tdfa("(a?)+", "aa", &[Some((1, 2))]);
    check_tdfa("(a*)*", "", &[None]);
    check_tdfa("(a*)*", "aaa", &[Some((0, 3))]);
    check_tdfa("(|x)*", "x", &[Some((0, 1))]);
    check_tdfa("(a*){2,5}", "aa", &[Some((2, 2))]);
    check_tdfa("(a*)+", "", &[Some((0, 0))]);
    check_tdfa("(a*)+", "aaa", &[Some((0, 3))]);
}
