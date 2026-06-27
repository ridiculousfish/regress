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
        0xAA if unicode_icase && pos >= 3 && input[pos - 3] == 0xE2 && input[pos - 2] == 0x84 => {
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

/// Bit positions in a position's *boundary signature* — the small set of facts
/// that every ECMAScript byte-boundary zero-width assertion (`^ $ \b \B`) is a
/// pure function of. Computed once per position by [`boundary_signature`] and
/// decoded by [`EpsCondition::holds_sig`]. This is the single shared vocabulary
/// the interpreter, the determinizer, and (eventually) the JIT all speak.
pub mod boundary {
    /// `pos == 0` (start of the haystack).
    pub const AT_TEXT_START: u8 = 1 << 0;
    /// `pos == input.len()` (end of the haystack).
    pub const AT_TEXT_END: u8 = 1 << 1;
    /// The codepoint ending at `pos` is a line terminator.
    pub const PREV_LINE_TERM: u8 = 1 << 2;
    /// The codepoint starting at `pos` is a line terminator.
    pub const NEXT_LINE_TERM: u8 = 1 << 3;
    /// The codepoint ending at `pos` is a word char.
    pub const PREV_WORD: u8 = 1 << 4;
    /// The codepoint starting at `pos` is a word char.
    pub const NEXT_WORD: u8 = 1 << 5;
}

/// Compute the [`boundary`] signature at byte position `pos`. `unicode_icase`
/// widens the word-char set with the only two non-ASCII codepoints that fold to
/// ASCII word chars (U+017F ſ, U+212A Kelvin); it affects only the `*_WORD`
/// bits. This is the single source of truth for the neighbour-byte logic — it
/// just packs the [`prev_byte_is_line_terminator`] / [`next_byte_is_word_char`]
/// family into a bitmask so a position's facts are computed once and reused.
#[inline]
pub fn boundary_signature(input: &[u8], pos: usize, unicode_icase: bool) -> u8 {
    use boundary::*;
    let mut sig = 0u8;
    if pos == 0 {
        sig |= AT_TEXT_START;
    }
    if pos == input.len() {
        sig |= AT_TEXT_END;
    }
    if prev_byte_is_line_terminator(input, pos) {
        sig |= PREV_LINE_TERM;
    }
    if next_byte_is_line_terminator(input, pos) {
        sig |= NEXT_LINE_TERM;
    }
    if prev_byte_is_word_char(input, pos, unicode_icase) {
        sig |= PREV_WORD;
    }
    if next_byte_is_word_char(input, pos, unicode_icase) {
        sig |= NEXT_WORD;
    }
    sig
}

impl EpsCondition {
    /// Whether this predicate is satisfied at byte position `pos` in `input`,
    /// with the per-thread `tags` array (used by `ProgressSince`). The
    /// byte-boundary predicates (`^ $ \b \B`) are decoded from a
    /// [`boundary_signature`] via [`holds_sig`](Self::holds_sig) so the semantics
    /// live in exactly one place.
    #[inline]
    pub fn holds(&self, input: &[u8], pos: usize, tags: &[TextPos]) -> bool {
        match self {
            EpsCondition::Always => true,
            EpsCondition::ProgressSince(idx) => {
                let v = tags[*idx as usize];
                v == TEXT_POS_NO_MATCH || v < pos
            }
            // `unicode_icase` matters only for the `*_WORD` bits.
            EpsCondition::WordBoundary { unicode_icase, .. } => {
                self.holds_sig(boundary_signature(input, pos, *unicode_icase))
            }
            // `^`/`$` ignore the word bits, so the icase flag is irrelevant here.
            EpsCondition::StartOfLine { .. } | EpsCondition::EndOfLine { .. } => {
                self.holds_sig(boundary_signature(input, pos, false))
            }
        }
    }

    /// Decode this predicate against a precomputed [`boundary_signature`]. Pure —
    /// no input access — and the authoritative mapping from boundary facts to
    /// assertion truth, mirrored by the JIT and keyed on by the determinizer's
    /// alt/conditional construction.
    ///
    /// Only the byte-boundary predicates (`^ $ \b \B`) are boundary functions.
    /// `Always` is trivially true; `ProgressSince` is tag-based, not a boundary
    /// predicate, and must be evaluated via [`holds`](Self::holds) — it imposes
    /// no boundary constraint, so it is reported as satisfied here. (It never
    /// appears as a runtime anchor alt/conditional; it is resolved statically
    /// during closure construction.)
    #[inline]
    pub fn holds_sig(&self, sig: u8) -> bool {
        use boundary::*;
        match self {
            EpsCondition::Always | EpsCondition::ProgressSince(_) => true,
            EpsCondition::StartOfLine { multiline: false } => sig & AT_TEXT_START != 0,
            EpsCondition::StartOfLine { multiline: true } => {
                sig & (AT_TEXT_START | PREV_LINE_TERM) != 0
            }
            EpsCondition::EndOfLine { multiline: false } => sig & AT_TEXT_END != 0,
            EpsCondition::EndOfLine { multiline: true } => sig & (AT_TEXT_END | NEXT_LINE_TERM) != 0,
            EpsCondition::WordBoundary { invert, .. } => {
                let prev = sig & PREV_WORD != 0;
                let next = sig & NEXT_WORD != 0;
                (prev != next) != *invert
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::boundary::*;
    use super::boundary_signature;
    use crate::automata::nfa::EpsCondition;

    #[test]
    fn signature_bits() {
        // (input, pos, icase) -> exact signature.
        let f = |s: &[u8], p: usize| boundary_signature(s, p, false);

        assert_eq!(f(b"", 0), AT_TEXT_START | AT_TEXT_END);
        // "ab": start | next-word.
        assert_eq!(f(b"ab", 0), AT_TEXT_START | NEXT_WORD);
        // between two word chars: prev-word | next-word (no boundary).
        assert_eq!(f(b"ab", 1), PREV_WORD | NEXT_WORD);
        // end of text, after a word char.
        assert_eq!(f(b"ab", 2), AT_TEXT_END | PREV_WORD);
        // "a b": at the space, prev is word, next (space) is not.
        assert_eq!(f(b"a b", 1), PREV_WORD);
        // right after '\n': prev is a line terminator, next is a word char.
        assert_eq!(f(b"a\nb", 2), PREV_LINE_TERM | NEXT_WORD);
        // just before '\n': next is a line terminator.
        assert_eq!(f(b"a\nb", 1), PREV_WORD | NEXT_LINE_TERM);

        // U+2028 (E2 80 A8) line separator: at the boundary after it.
        let ls = "x\u{2028}y".as_bytes();
        let after = 1 + 3; // 'x' + 3-byte separator
        assert_eq!(boundary_signature(ls, after, false), PREV_LINE_TERM | NEXT_WORD);

        // ſ (U+017F, C5 BF) folds to a word char only under unicode-icase.
        let sf = "\u{017F}".as_bytes();
        assert_eq!(boundary_signature(sf, sf.len(), false) & PREV_WORD, 0);
        assert_eq!(
            boundary_signature(sf, sf.len(), true) & PREV_WORD,
            PREV_WORD
        );
    }

    #[test]
    fn holds_sig_matches_holds() {
        // holds() is now defined on top of holds_sig(boundary_signature(..)); this
        // pins the decode against the neighbour-byte helpers across edge inputs.
        let inputs: &[&[u8]] = &[
            b"",
            b"a",
            b"ab",
            b"a b",
            b"a\nb",
            b"\n\n",
            "x\u{2028}\u{017F}".as_bytes(),
        ];
        let conds = [
            EpsCondition::StartOfLine { multiline: false },
            EpsCondition::StartOfLine { multiline: true },
            EpsCondition::EndOfLine { multiline: false },
            EpsCondition::EndOfLine { multiline: true },
            EpsCondition::WordBoundary { invert: false, unicode_icase: false },
            EpsCondition::WordBoundary { invert: true, unicode_icase: false },
            EpsCondition::WordBoundary { invert: false, unicode_icase: true },
        ];
        for input in inputs {
            for pos in 0..=input.len() {
                for c in &conds {
                    let icase = matches!(
                        c,
                        EpsCondition::WordBoundary { unicode_icase: true, .. }
                    );
                    let sig = boundary_signature(input, pos, icase);
                    assert_eq!(
                        c.holds(input, pos, &[]),
                        c.holds_sig(sig),
                        "cond {c:?} input {input:?} pos {pos}"
                    );
                }
            }
        }
    }
}
