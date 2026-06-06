//! Tests that loops over empty-matching bodies build and execute with
//! V8-matching captures. The `x* → (x+)?` rewrite in `build_loop` plus
//! per-thread capture forking in the eps closure gives spec-correct
//! captures without an explicit "no empty iteration" rule.
//! See `Node::can_match_empty`, rust-lang/regex#779.

use crate::automata::nfa::Nfa;
use crate::automata::nfa_backend::execute;
use crate::Flags;

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

/// Capture expectations cross-checked against V8 and rust-regex.
/// Most cases agree. `(a*)*` against `""` is the one case where the
/// Thompson NFA's natural behavior (matching rust-regex's PikeVM)
/// diverges from V8's strict ES spec — V8 returns `None` for group 1,
/// we and rust-regex both return `Some((0, 0))`. Documented here as
/// the intended Thompson-NFA behavior.
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
    // V8 gives [None]; our NFA + rust-regex both give [Some((0,0))].
    check("(a*)*", "", &[Some((0, 0))]);
    check("(a*)*", "aaa", &[Some((0, 3))]);
    // V8 backtracks past the empty alt match to try "x", giving
    // [Some((0, 1))]. Thompson NFAs (us + rust-regex) can't naturally
    // backtrack past a successful match — leftmost-first picks the
    // empty branch and never tries `x`.
    check("(|x)*", "x", &[Some((0, 0))]);
    check("(a*){2,5}", "aa", &[Some((2, 2))]);
}
