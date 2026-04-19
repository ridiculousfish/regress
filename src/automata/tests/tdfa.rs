//! TDFA construction and execution tests (Milestone 2).

use crate::automata::nfa::Nfa;
use crate::automata::tdfa::Tdfa;
use crate::automata::tdfa_backend::{
    execute_anchored, execute_anchored_longest_accept, execute_anchored_shortest_accept,
};
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

#[test]
fn greedy_consumes_all_matching_prefix() {
    // Greedy `a*` keeps the loop body alive in the closure (body comes before
    // GOAL in priority order, so truncate-at-first-GOAL retains it). The
    // TDFA can consume the full run of 'a's.
    let greedy = make_tdfa("a*");
    assert_eq!(execute_anchored_longest_accept(&greedy, b""), Some(0));
    assert_eq!(execute_anchored_longest_accept(&greedy, b"a"), Some(1));
    assert_eq!(execute_anchored_longest_accept(&greedy, b"aaa"), Some(3));
    assert_eq!(execute_anchored_longest_accept(&greedy, b"b"), Some(0));
    assert_eq!(execute_anchored_longest_accept(&greedy, b"aab"), Some(2));
}

#[test]
fn lazy_stops_at_empty_match() {
    // Lazy `a*?` puts the loop-exit-to-GOAL path before the loop body. After
    // truncate-at-first-GOAL, the body is dropped as lower-priority. The
    // resulting TDFA accepts the empty prefix and dead-states on the first
    // byte — which is the correct leftmost-lazy anchored match at position 0.
    let lazy = make_tdfa("a*?");
    assert_eq!(execute_anchored_longest_accept(&lazy, b""), Some(0));
    assert_eq!(execute_anchored_longest_accept(&lazy, b"a"), Some(0));
    assert_eq!(execute_anchored_longest_accept(&lazy, b"aaa"), Some(0));
    assert_eq!(execute_anchored_shortest_accept(&lazy, b"aaa"), Some(0));
}

#[test]
fn anchored_boolean_basic() {
    let t = make_tdfa("abc");
    assert!(execute_anchored(&t, b"abc"));
    assert!(!execute_anchored(&t, b"ab"));
    assert!(!execute_anchored(&t, b"abcd"));
    assert!(!execute_anchored(&t, b"xyz"));
}

#[test]
fn loop_matches() {
    let t = make_tdfa("ab*c");
    assert!(execute_anchored(&t, b"ac"));
    assert!(execute_anchored(&t, b"abc"));
    assert!(execute_anchored(&t, b"abbbc"));
    assert!(!execute_anchored(&t, b"a"));
    assert!(!execute_anchored(&t, b"abd"));
}

#[test]
fn char_class() {
    let t = make_tdfa("[a-z]+");
    assert_eq!(execute_anchored_longest_accept(&t, b"hello"), Some(5));
    assert_eq!(execute_anchored_longest_accept(&t, b"Hi"), None);
    assert_eq!(execute_anchored_longest_accept(&t, b"abc123"), Some(3));
}

#[test]
fn alternation_loop() {
    let t = make_tdfa("(ab|cd)+");
    assert_eq!(execute_anchored_longest_accept(&t, b"ab"), Some(2));
    assert_eq!(execute_anchored_longest_accept(&t, b"abcd"), Some(4));
    assert_eq!(execute_anchored_longest_accept(&t, b"abcdab"), Some(6));
    assert_eq!(execute_anchored_longest_accept(&t, b"abx"), Some(2));
    assert_eq!(execute_anchored_longest_accept(&t, b"x"), None);
}

#[test]
fn dead_state_early_exit() {
    let t = make_tdfa("abc");
    assert!(!execute_anchored(&t, b"abd"));
    assert!(!execute_anchored(&t, b"xbc"));
}

#[test]
fn dead_state_is_state_zero() {
    let t = make_tdfa("a");
    assert_eq!(crate::automata::tdfa::TDFA_DEAD_STATE, 0);
    // Dead state is non-accepting.
    assert!(!t.accepting()[0]);
}

