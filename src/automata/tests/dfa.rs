use crate::automata::dfa::Dfa;
use crate::automata::dfa_backend::execute_anchored;
use crate::automata::nfa::Nfa;
use crate::automata::nfa_backend::execute_nfa;
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

fn make_dfa(pattern: &str) -> Dfa {
    let re = parse_ir(pattern);
    let nfa = Nfa::try_from(&re).expect("nfa build failed");
    Dfa::try_from(&nfa).expect("dfa build failed")
}

fn make_nfa(pattern: &str) -> Nfa {
    let re = parse_ir(pattern);
    Nfa::try_from(&re).expect("nfa build failed")
}

/// Check that DFA and NFA agree on whether the input matches (anchored, full).
fn assert_dfa_nfa_agree(pattern: &str, input: &str) {
    let nfa = make_nfa(pattern);
    let dfa = Dfa::try_from(&nfa).expect("dfa build failed");

    let dfa_result = execute_anchored(&dfa,input.as_bytes());
    let nfa_result = execute_nfa(&nfa, input.as_bytes());

    // NFA returns a match range; for anchored full match, the match must cover
    // the entire input.
    let nfa_full_match = nfa_result
        .as_ref()
        .map(|m| m.range.start == 0 && m.range.end == input.len())
        .unwrap_or(false);

    assert_eq!(
        dfa_result, nfa_full_match,
        "DFA/NFA disagree on pattern {:?} with input {:?}: dfa={}, nfa_match={:?}",
        pattern, input, dfa_result, nfa_result
    );
}

#[test]
fn test_literal() {
    let dfa = make_dfa("abc");
    assert!(execute_anchored(&dfa,b"abc"));
    assert!(!execute_anchored(&dfa,b"ab"));
    assert!(!execute_anchored(&dfa,b"abcd"));
    assert!(!execute_anchored(&dfa,b"xyz"));
    assert!(!execute_anchored(&dfa,b""));
}

#[test]
fn test_alternation() {
    let dfa = make_dfa("a|b|c");
    assert!(execute_anchored(&dfa,b"a"));
    assert!(execute_anchored(&dfa,b"b"));
    assert!(execute_anchored(&dfa,b"c"));
    assert!(!execute_anchored(&dfa,b"d"));
    assert!(!execute_anchored(&dfa,b"ab"));
    assert!(!execute_anchored(&dfa,b""));
}

#[test]
fn test_star() {
    let dfa = make_dfa("a*");
    assert!(execute_anchored(&dfa,b""));
    assert!(execute_anchored(&dfa,b"a"));
    assert!(execute_anchored(&dfa,b"aaa"));
    assert!(!execute_anchored(&dfa,b"b"));
    assert!(!execute_anchored(&dfa,b"ab"));
}

#[test]
fn test_plus() {
    let dfa = make_dfa("a+");
    assert!(!execute_anchored(&dfa,b""));
    assert!(execute_anchored(&dfa,b"a"));
    assert!(execute_anchored(&dfa,b"aaaa"));
    assert!(!execute_anchored(&dfa,b"b"));
}

#[test]
fn test_optional() {
    let dfa = make_dfa("ab?c");
    assert!(execute_anchored(&dfa,b"ac"));
    assert!(execute_anchored(&dfa,b"abc"));
    assert!(!execute_anchored(&dfa,b"abbc"));
    assert!(!execute_anchored(&dfa,b"a"));
}

#[test]
fn test_bracket_class() {
    let dfa = make_dfa("[a-z]+");
    assert!(execute_anchored(&dfa,b"hello"));
    assert!(execute_anchored(&dfa,b"z"));
    assert!(!execute_anchored(&dfa,b"Hello"));
    assert!(!execute_anchored(&dfa,b"123"));
    assert!(!execute_anchored(&dfa,b""));
}

#[test]
fn test_wildcard_bracket() {
    // Use a bracket class as a stand-in for dot (which the NFA doesn't support yet).
    let dfa = make_dfa("a[a-z]b");
    assert!(execute_anchored(&dfa,b"axb"));
    assert!(execute_anchored(&dfa,b"aab"));
    assert!(!execute_anchored(&dfa,b"ab"));
    assert!(!execute_anchored(&dfa,b"a1b"));
}

#[test]
fn test_unicode_literal() {
    let dfa = make_dfa("café");
    assert!(execute_anchored(&dfa,"café".as_bytes()));
    assert!(!execute_anchored(&dfa,"cafe".as_bytes()));
    assert!(!execute_anchored(&dfa,b""));
}

#[test]
fn test_unicode_bracket() {
    // CJK character range.
    let dfa = make_dfa("[一-龥]+");
    assert!(execute_anchored(&dfa,"中文".as_bytes()));
    assert!(execute_anchored(&dfa,"一".as_bytes()));
    assert!(!execute_anchored(&dfa,"abc".as_bytes()));
    assert!(!execute_anchored(&dfa,b""));
}

#[test]
fn test_empty_pattern() {
    let dfa = make_dfa("");
    assert!(execute_anchored(&dfa,b""));
    assert!(!execute_anchored(&dfa,b"a"));
}

#[test]
fn test_bounded_quantifier() {
    let dfa = make_dfa("a{2,4}");
    assert!(!execute_anchored(&dfa,b"a"));
    assert!(execute_anchored(&dfa,b"aa"));
    assert!(execute_anchored(&dfa,b"aaa"));
    assert!(execute_anchored(&dfa,b"aaaa"));
    assert!(!execute_anchored(&dfa,b"aaaaa"));
}

#[test]
fn test_dead_state() {
    let dfa = make_dfa("x");
    assert!(!execute_anchored(&dfa,b"y"));
    // Once in dead state, stays dead regardless of further input.
    assert!(!execute_anchored(&dfa,b"yxxxxxxxxx"));
}

#[test]
fn test_dfa_nfa_agreement() {
    let cases: &[(&str, &[&str])] = &[
        ("abc", &["abc", "ab", "abcd", "", "xyz"]),
        ("a|b", &["a", "b", "c", "ab", ""]),
        ("a*b", &["b", "ab", "aab", "aaab", "a", ""]),
        ("[0-9]+", &["0", "123", "9", "abc", "", "1a"]),
        (
            "a(b|c)d",
            &["abd", "acd", "ad", "abcd", "a", ""],
        ),
        ("(ab)+", &["ab", "abab", "ababab", "a", "b", ""]),
    ];

    for &(pattern, inputs) in cases {
        for &input in inputs {
            assert_dfa_nfa_agree(pattern, input);
        }
    }
}

#[test]
fn test_byte_class_count() {
    // A simple ASCII pattern should have few byte classes.
    let dfa = make_dfa("abc");
    // 'a', 'b', 'c' split the byte space into at most ~7 classes:
    // [0, 'a'-1], ['a'], ['a'+1, 'b'-1], ['b'], ['b'+1, 'c'-1], ['c'], ['c'+1, 255]
    // Plus possible NFA internal ranges. Should be compact.
    assert!(
        dfa.num_classes() < 20,
        "Expected few byte classes for 'abc', got {}",
        dfa.num_classes()
    );
}
