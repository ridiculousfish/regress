//! Tests that `\b` and `\B` word-boundary assertions build and execute
//! with V8-matching semantics. Implementation uses
//! `EpsCondition::WordBoundary { invert, unicode_icase }`, evaluated at
//! runtime by the NFA backend via `anchors::prev_byte_is_word_char` /
//! `next_byte_is_word_char`. TDFA currently bails on word-boundary
//! patterns (see TODO in `close_priority`).

use crate::Flags;
use crate::automata::nfa::Nfa;
use crate::automata::nfa_backend::execute;
use crate::automata::tdfa::Tdfa;
use crate::automata::tdfa_backend::execute as tdfa_execute;

fn build(pattern: &str, unicode: bool, icase: bool) -> Nfa {
    let flags = Flags {
        unicode,
        icase,
        ..Flags::default()
    };
    let mut ire =
        crate::backends::try_parse(pattern.chars().map(u32::from), flags).expect("parse failed");
    crate::backends::optimize(&mut ire);
    Nfa::try_from(&ire).unwrap_or_else(|e| panic!("expected {pattern:?} to build, got {e:?}"))
}

/// V8 cross-checked via `node -e 'JSON.stringify("...".match(/.../<flags>))'`.
fn check_match(pattern: &str, input: &str, expected: Option<(usize, usize)>) {
    check_match_flagged(pattern, input, false, false, expected);
}

fn check_match_flagged(
    pattern: &str,
    input: &str,
    unicode: bool,
    icase: bool,
    expected: Option<(usize, usize)>,
) {
    let nfa = build(pattern, unicode, icase);
    // V8's `.match` is unanchored — try each starting byte offset until
    // one succeeds. Walks bytes (not codepoints) since the NFA is over
    // the byte stream and `\b` predicates evaluate at byte positions.
    let bytes = input.as_bytes();
    let mut found: Option<(usize, usize)> = None;
    for start in 0..=bytes.len() {
        if let Some(m) = execute(&nfa, bytes, start) {
            found = Some((m.range.start, m.range.end));
            break;
        }
    }
    assert_eq!(
        found, expected,
        "pattern={pattern:?} input={input:?} unicode={unicode} icase={icase}"
    );
}

#[test]
fn b_at_word_starts_and_ends() {
    check_match(r"\bfoo", "foo bar", Some((0, 3)));
    check_match(r"\bfoo", "xfoo", None);
    check_match(r"foo\b", "foo", Some((0, 3)));
    check_match(r"foo\b", "foobar", None);
    check_match(r"foo\b", "foo!", Some((0, 3)));
}

#[test]
fn capital_b_negates() {
    check_match(r"\Bfoo", "xfoo", Some((1, 4)));
    check_match(r"\Bfoo", "foo", None);
    check_match(r"a\Bb", "ab", Some((0, 2)));
    check_match(r"a\bb", "ab", None);
}

#[test]
fn b_against_eoi_and_soi() {
    // Empty input: SOI == EOI, neither is a word char → no boundary.
    check_match(r"\b", "", None);
    // Single word char: SOI before and EOI after both count as boundaries.
    check_match(r"\b", "x", Some((0, 0)));
    check_match(r"\b\b", "x", Some((0, 0)));
}

#[test]
fn b_unicode_icase_kelvin_and_long_s() {
    // U+212A Kelvin sign — UTF-8: E2 84 AA. Folds to ASCII 'k' under icase.
    // With `iu` flags, `\bk` should treat Kelvin as a word char, so SOI →
    // Kelvin is a boundary and the literal `k` matches Kelvin via case-fold.
    check_match_flagged(r"\bk", "\u{212A}", true, true, Some((0, 3)));
    // U+017F "ſ" — UTF-8: C5 BF. Folds to ASCII 's' under icase.
    check_match_flagged(r"\bs", "\u{017F}", true, true, Some((0, 2)));
}

#[test]
fn b_unicode_icase_after_word_char() {
    // After a word char then a fold-to-word char: no boundary between them.
    // V8: "kK".match(/k\bK/iu) → null (no boundary between 'k' and Kelvin).
    check_match_flagged(r"k\bk", "k\u{212A}", true, true, None);
    // \B between them holds:
    check_match_flagged(r"k\Bk", "k\u{212A}", true, true, Some((0, 4)));
}

/// TDFA-side coverage: word-boundary predicates resolve at runtime via
/// the `anchor_alt` mechanism (per-state list of conditional state
/// switches), with primary closures skipping `\b`/`\B` eps and alt
/// closures traversing them.
fn check_tdfa(
    pattern: &str,
    input: &str,
    unicode: bool,
    icase: bool,
    expected: Option<(usize, usize)>,
) {
    let nfa = build(pattern, unicode, icase);
    let tdfa = Tdfa::try_from(&nfa)
        .unwrap_or_else(|e| panic!("expected TDFA build for {pattern:?}, got {e:?}"));
    let bytes = input.as_bytes();
    let mut found: Option<(usize, usize)> = None;
    for start in 0..=bytes.len() {
        if let Some(m) = tdfa_execute(&tdfa, bytes, start) {
            found = Some((m.range.start, m.range.end));
            break;
        }
    }
    assert_eq!(
        found, expected,
        "TDFA: pattern={pattern:?} input={input:?} unicode={unicode} icase={icase}"
    );
}

#[test]
fn tdfa_matches_v8_for_word_boundary() {
    check_tdfa(r"\bfoo", "foo bar", false, false, Some((0, 3)));
    check_tdfa(r"\bfoo", "xfoo", false, false, None);
    check_tdfa(r"foo\b", "foo", false, false, Some((0, 3)));
    check_tdfa(r"foo\b", "foobar", false, false, None);
    check_tdfa(r"\Bfoo", "xfoo", false, false, Some((1, 4)));
    check_tdfa(r"\Bfoo", "foo", false, false, None);
    check_tdfa(r"\bfoo\b", "foo bar", false, false, Some((0, 3)));
    check_tdfa(r"\b", "x", false, false, Some((0, 0)));
    check_tdfa(r"\bk", "\u{212A}", true, true, Some((0, 3)));
    check_tdfa(r"\bs", "\u{017F}", true, true, Some((0, 2)));
    check_tdfa(r"k\bk", "k\u{212A}", true, true, None);
    check_tdfa(r"k\Bk", "k\u{212A}", true, true, Some((0, 4)));
}
