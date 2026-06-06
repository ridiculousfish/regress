//! Predicate evaluation for `^` and `$` anchors on byte-level input.
//!
//! Both the NFA executor (during eps closure) and the TDFA executor (per
//! step + at EOI) call into these helpers. The predicates index directly
//! into the input slice — no separate buffer needed, since both executors
//! hold the full input.
//!
//! Line terminators per ES9 11.3 (matching `crate::matchers::is_line_terminator`):
//! `\n` (0x0A), `\r` (0x0D), U+2028 (0xE2 0x80 0xA8), U+2029 (0xE2 0x80 0xA9).

use crate::automata::nfa::{EpsCondition, TEXT_POS_NO_MATCH, TextPos};

/// True iff the codepoint ending at byte position `pos` is a line terminator.
/// "Ending at" means `input[..pos]`'s last codepoint. Returns false at
/// `pos == 0` (no preceding codepoint).
///
/// For positions that aren't codepoint boundaries (i.e. mid UTF-8 sequence),
/// this naturally returns false: ASCII LTs are single bytes, and the
/// 0xE2 0x80 0xA8/0xA9 sequence only fully matches at the boundary AFTER
/// the third byte. Mid-codepoint positions don't satisfy either check.
#[inline]
pub fn prev_byte_is_line_terminator(input: &[u8], pos: usize) -> bool {
    if pos == 0 {
        return false;
    }
    match input[pos - 1] {
        0x0A | 0x0D => true,
        0xA8 | 0xA9 if pos >= 3 && input[pos - 3] == 0xE2 && input[pos - 2] == 0x80 => true,
        _ => false,
    }
}

/// True iff the codepoint ending at byte position `pos` is a word char
/// per ES9 21.2.2.6.2. `unicode_icase` widens the set with the only two
/// non-ASCII codepoints that fold to ASCII word chars: U+017F "ſ" → 's'
/// (UTF-8 `C5 BF`) and U+212A Kelvin → 'k' (UTF-8 `E2 84 AA`). See
/// `unicodetables::nonascii_folds_to_ascii_word_char`.
///
/// ASCII bytes (< 0x80) never appear inside UTF-8 multi-byte sequences,
/// so an ASCII match on `input[pos - 1]` is unambiguously a single
/// codepoint regardless of context.
#[inline]
pub fn prev_byte_is_word_char(input: &[u8], pos: usize, unicode_icase: bool) -> bool {
    if pos == 0 {
        return false;
    }
    match input[pos - 1] {
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => true,
        0xBF if unicode_icase && pos >= 2 && input[pos - 2] == 0xC5 => true,
        0xAA if unicode_icase
            && pos >= 3
            && input[pos - 3] == 0xE2
            && input[pos - 2] == 0x84 =>
        {
            true
        }
        _ => false,
    }
}

/// True iff the codepoint starting at byte position `pos` is a word char.
/// See `prev_byte_is_word_char` for the `unicode_icase` widening rule.
#[inline]
pub fn next_byte_is_word_char(input: &[u8], pos: usize, unicode_icase: bool) -> bool {
    if pos >= input.len() {
        return false;
    }
    match input[pos] {
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => true,
        0xC5 if unicode_icase && pos + 1 < input.len() && input[pos + 1] == 0xBF => true,
        0xE2 if unicode_icase
            && pos + 2 < input.len()
            && input[pos + 1] == 0x84
            && input[pos + 2] == 0xAA =>
        {
            true
        }
        _ => false,
    }
}

/// True iff the codepoint starting at byte position `pos` is a line
/// terminator. Returns false at `pos >= input.len()` (no next codepoint).
#[inline]
pub fn next_byte_is_line_terminator(input: &[u8], pos: usize) -> bool {
    if pos >= input.len() {
        return false;
    }
    match input[pos] {
        0x0A | 0x0D => true,
        0xE2 if pos + 2 < input.len()
            && input[pos + 1] == 0x80
            && matches!(input[pos + 2], 0xA8 | 0xA9) =>
        {
            true
        }
        _ => false,
    }
}

impl EpsCondition {
    /// Whether this predicate is satisfied at byte position `pos` in `input`,
    /// with the per-thread `tags` array (used by `ProgressSince`).
    #[inline]
    pub fn holds(&self, input: &[u8], pos: usize, tags: &[TextPos]) -> bool {
        match self {
            EpsCondition::Always => true,
            EpsCondition::StartOfLine { multiline: false } => pos == 0,
            EpsCondition::StartOfLine { multiline: true } => {
                pos == 0 || prev_byte_is_line_terminator(input, pos)
            }
            EpsCondition::EndOfLine { multiline: false } => pos == input.len(),
            EpsCondition::EndOfLine { multiline: true } => {
                pos == input.len() || next_byte_is_line_terminator(input, pos)
            }
            EpsCondition::WordBoundary {
                invert,
                unicode_icase,
            } => {
                let prev = prev_byte_is_word_char(input, pos, *unicode_icase);
                let curr = next_byte_is_word_char(input, pos, *unicode_icase);
                (prev != curr) != *invert
            }
            EpsCondition::ProgressSince(idx) => {
                let v = tags[*idx as usize];
                v == TEXT_POS_NO_MATCH || v < pos
            }
        }
    }
}
