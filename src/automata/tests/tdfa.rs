//! TDFA construction and execution tests (Milestone 2).

use crate::automata::nfa::Nfa;
use crate::automata::tdfa::Tdfa;
use crate::automata::tdfa_backend::execute_anchored;
use crate::Flags;

fn parse_ir(pattern: &str) -> crate::ir::Regex {
    let flags = Flags {
        unicode: true,
        ..Default::default()
    };
    let mut re = crate::backends::try_parse(pattern.chars().map(u32::from), flags)
        .expect("parse failed");
    crate::optimizer::optimize(&mut re);
    re
}

fn make_tdfa(pattern: &str) -> Tdfa {
    let re = parse_ir(pattern);
    let nfa = Nfa::try_from(&re).expect("nfa build failed");
    Tdfa::try_from(&nfa).expect("tdfa build failed")
}

fn end(tdfa: &Tdfa, input: &[u8]) -> Option<usize> {
    execute_anchored(tdfa, input).map(|r| r.end)
}

#[test]
fn greedy_consumes_all_matching_prefix() {
    // Greedy `a*` keeps the loop body alive in the closure (body comes before
    // GOAL in priority order, so truncate-at-first-GOAL retains it).
    let greedy = make_tdfa("a*");
    assert_eq!(end(&greedy, b""), Some(0));
    assert_eq!(end(&greedy, b"a"), Some(1));
    assert_eq!(end(&greedy, b"aaa"), Some(3));
    assert_eq!(end(&greedy, b"b"), Some(0));
    assert_eq!(end(&greedy, b"aab"), Some(2));
}

#[test]
fn lazy_stops_at_empty_match() {
    // Lazy `a*?` puts the loop-exit-to-GOAL path before the loop body. After
    // truncate-at-first-GOAL, the body is dropped. The TDFA accepts the empty
    // prefix and dead-states on the first byte — the correct leftmost-lazy
    // anchored match at position 0.
    let lazy = make_tdfa("a*?");
    assert_eq!(end(&lazy, b""), Some(0));
    assert_eq!(end(&lazy, b"a"), Some(0));
    assert_eq!(end(&lazy, b"aaa"), Some(0));
}

#[test]
fn anchored_basic() {
    let t = make_tdfa("abc");
    assert_eq!(end(&t, b"abc"), Some(3));
    assert_eq!(end(&t, b"abcd"), Some(3));
    assert_eq!(end(&t, b"ab"), None);
    assert_eq!(end(&t, b"xyz"), None);
}

#[test]
fn loop_matches() {
    let t = make_tdfa("ab*c");
    assert_eq!(end(&t, b"ac"), Some(2));
    assert_eq!(end(&t, b"abc"), Some(3));
    assert_eq!(end(&t, b"abbbc"), Some(5));
    assert_eq!(end(&t, b"a"), None);
    assert_eq!(end(&t, b"abd"), None);
}

#[test]
fn char_class() {
    let t = make_tdfa("[a-z]+");
    assert_eq!(end(&t, b"hello"), Some(5));
    assert_eq!(end(&t, b"Hi"), None);
    assert_eq!(end(&t, b"abc123"), Some(3));
}

#[test]
fn alternation_loop() {
    let t = make_tdfa("(ab|cd)+");
    assert_eq!(end(&t, b"ab"), Some(2));
    assert_eq!(end(&t, b"abcd"), Some(4));
    assert_eq!(end(&t, b"abcdab"), Some(6));
    assert_eq!(end(&t, b"abx"), Some(2));
    assert_eq!(end(&t, b"x"), None);
}

#[test]
fn leftmost_first_alternation() {
    // ECMAScript leftmost-first: `a|ab` picks `a` even though `ab` is longer.
    let t = make_tdfa("a|ab");
    assert_eq!(end(&t, b"ab"), Some(1));
    // Reversed order: `ab|a` picks `ab` when available, falls back to `a`.
    let t2 = make_tdfa("ab|a");
    assert_eq!(end(&t2, b"ab"), Some(2));
    assert_eq!(end(&t2, b"ac"), Some(1));
}

#[test]
fn dead_state_is_state_zero() {
    let t = make_tdfa("a");
    assert_eq!(crate::automata::tdfa::TDFA_DEAD_STATE, 0);
    assert!(!t.accepting()[0]);
}

#[test]
fn greedy_loop_correct() {
    // `a*` on "aaab" — the accept state has non-'a' transitions to dead, so
    // the scan path finds length 3.
    let t = make_tdfa("a*");
    assert_eq!(end(&t, b"aaab"), Some(3));
    assert!(t.start() != crate::automata::tdfa::TDFA_DEAD_STATE);
}

#[test]
fn lazy_start_is_accepting() {
    // `a*?` start accepts but transitions to dead on 'a'.
    let t = make_tdfa("a*?");
    assert!(t.start() != crate::automata::tdfa::TDFA_DEAD_STATE);
    assert!(t.accepting()[t.start() as usize]);
    assert_eq!(end(&t, b"aaa"), Some(0));
}
