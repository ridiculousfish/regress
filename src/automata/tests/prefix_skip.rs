//! Prefix-skip tests: the `Prefix` strategy warm-starts the anchored verify
//! automaton past the bytes the prefilter already matched (`PrefixSkip` /
//! `compute_prefix_skip` + `compute_byteclass_skip`), in both the interpreter and
//! the JIT.
//!
//! The skip is sound only when the skipped prefix writes no marks, so every case
//! is cross-checked against the classical backtracker oracle — over inputs that
//! exercise matches at offset 0 (where the warm start replaces the
//! anchored-vs-unanchored branch), captures *after* the prefix (the capture-tier
//! warm prologue), and a capture *inside* the prefix (where the skip must decline
//! and the result must still be correct).

use crate::Flags;
use crate::automata::executors::TdfaExecutor;
use crate::automata::prefilter::TdfaProgram;

fn parse_ir(pattern: &str) -> crate::ir::Regex {
    let mut re =
        crate::backends::try_parse(pattern.chars().map(u32::from), Flags::default()).expect("parse");
    crate::optimizer::optimize(&mut re);
    re
}

type Caps = Vec<(usize, usize, Vec<Option<(usize, usize)>>)>;

fn backtrack_caps(pattern: &str, input: &str) -> Caps {
    let re = parse_ir(pattern);
    let cr = crate::backends::emit(&re);
    crate::backends::find::<crate::backends::BacktrackExecutor>(&cr, input, 0)
        .map(|m| {
            let groups =
                m.captures.iter().map(|c| c.as_ref().map(|r| (r.start, r.end))).collect();
            (m.range.start, m.range.end, groups)
        })
        .collect()
}

fn interp_caps(prog: &TdfaProgram, input: &str) -> Caps {
    crate::backends::find::<TdfaExecutor>(prog, input, 0)
        .map(|m| {
            let groups =
                m.captures.iter().map(|c| c.as_ref().map(|r| (r.start, r.end))).collect();
            (m.range.start, m.range.end, groups)
        })
        .collect()
}

/// Assert the interpreter (and, when built, the JIT) `Prefix` pipeline agrees
/// with the backtracker — full match *and* captures — on every input.
fn assert_agrees(pattern: &str, inputs: &[&str]) {
    let re = parse_ir(pattern);
    let prog = TdfaProgram::try_from_ir(&re).expect("build");
    #[cfg(feature = "tdfa-jit")]
    let jit = crate::automata::prefilter::TdfaJitProgram::try_from_ir(&re).expect("build jit");
    for &input in inputs {
        let oracle = backtrack_caps(pattern, input);
        assert_eq!(interp_caps(&prog, input), oracle, "interp {pattern:?} on {input:?}");
        #[cfg(feature = "tdfa-jit")]
        {
            let jit_caps: Caps = crate::backends::find::<
                crate::automata::executors::TdfaJitExecutor,
            >(&jit, input, 0)
            .map(|m| {
                let groups =
                    m.captures.iter().map(|c| c.as_ref().map(|r| (r.start, r.end))).collect();
                (m.range.start, m.range.end, groups)
            })
            .collect();
            assert_eq!(jit_caps, oracle, "jit {pattern:?} on {input:?}");
        }
    }
}

#[test]
fn byteclass_prefix_ip() {
    // `[0-9]` prefilter → length-1 byte-class skip.
    assert_agrees(
        r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}",
        &[
            "",
            "no ip, lone digits 5 6 7",
            "edge 1.2.3.4 mid 10.0.0.255 end",
            "1.1.1.1",                 // match at offset 0 (warm start, no `^` branch)
            "...0.0.0.0...",           // literal dots flank the match
            "a1.2.3.4b 9.9.9.9",       // digit not at a word start, then a clean ip
            "1.2.3..4 partial",        // double dot near-miss
        ],
    );
}

#[test]
fn single_digit_byteclass_accepting_post_state() {
    // The skip's post_state is itself accepting (the boundary accept fires right
    // after the skipped byte). Match is exactly one digit.
    assert_agrees(r"[0-9]", &["", "x", "5", "a7b", "42"]);
}

#[test]
fn literal_prefix_skip() {
    // Exact-literal (`ByteSeq`) prefilter: the whole literal is skipped.
    assert_agrees(
        r"foo[0-9]+",
        &["", "foo", "foo12 bar", "xfoo7", "foofoo9", "FOO5 not matched"],
    );
}

#[test]
fn captures_after_prefix() {
    // Capture tier with a warm start: the group lives after the skipped prefix,
    // so its offsets must be recorded correctly (incl. a match at offset 0).
    assert_agrees(r"[0-9]+(abc)", &["", "12abc", "7abc", "xx 3abc yy", "abc"]);
    assert_agrees(r"foo(\w+)", &["foo", "foobar baz", "xfoobar"]);
}

#[test]
fn capture_inside_prefix_declines_skip() {
    // A group opens on the prefilter byte itself, so the leading transition
    // writes a mark and the skip must decline — the result must still be correct.
    assert_agrees(r"([0-9])x", &["", "9x", "a3x", "12x"]);
    assert_agrees(r"(foo)[0-9]", &["foo7", "xfoo3", "foo"]);
}
