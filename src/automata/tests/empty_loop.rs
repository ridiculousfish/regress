//! Tests that loops over empty-matching bodies are rejected at NFA build
//! time. See `Node::can_match_empty` / rust-lang/regex#779.

use crate::automata::nfa::{Error, Nfa};
use crate::Flags;

fn try_build(pattern: &str) -> Result<Nfa, Error> {
    let mut ire = crate::backends::try_parse(pattern.chars().map(u32::from), Flags::default())
        .expect("parse failed");
    crate::backends::optimize(&mut ire);
    Nfa::try_from(&ire)
}

fn assert_rejected(pattern: &str) {
    match try_build(pattern) {
        Err(Error::UnsupportedInstruction(msg)) => {
            assert!(
                msg.contains("empty"),
                "expected empty-body rejection for {pattern:?}, got {msg:?}"
            );
        }
        Err(other) => panic!("expected UnsupportedInstruction for {pattern:?}, got {other:?}"),
        Ok(_) => panic!("expected {pattern:?} to be rejected"),
    }
}

fn assert_accepted(pattern: &str) {
    if let Err(e) = try_build(pattern) {
        panic!("expected {pattern:?} to build, got {e:?}");
    }
}

#[test]
fn rejects_loops_over_empty_bodies() {
    assert_rejected("(a?)*");
    assert_rejected("(a*)*");
    assert_rejected("(|x)*");
    assert_rejected("(a|)*");
    assert_rejected("(a?)+");
    assert_rejected("(a*){2,5}");
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
