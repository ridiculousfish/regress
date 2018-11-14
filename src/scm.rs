use crate::bytesearch::{charset_contains, ByteArraySet, ByteSeq, ByteSet, SmallArraySet};
use crate::cursor::{Cursorable, Position};
use crate::indexing::ElementType;
use crate::insn::MAX_CHAR_SET_LENGTH;
use crate::matchers::CharProperties;
use crate::types::BracketContents;

/// A trait for things that match a single Element.
pub trait SingleCharMatcher<Cursor: Cursorable> {
    /// \return whether we match the character at the given position, advancing
    /// the position if so. On a false return, the position is unspecified.
    fn matches(&self, pos: &mut Position, cursor: Cursor) -> bool;
}

/// Insn::Char
pub struct Char<Cursor: Cursorable> {
    pub c: Cursor::Element,
}
impl<Cursor: Cursorable> SingleCharMatcher<Cursor> for Char<Cursor> {
    #[inline(always)]
    fn matches(&self, pos: &mut Position, cursor: Cursor) -> bool {
        match cursor.next(pos) {
            Some(c2) => c2 == self.c,
            _ => false,
        }
    }
}

/// Insn::CharICase
pub struct CharICase<Cursor: Cursorable> {
    pub c: Cursor::Element,
}
impl<Cursor: Cursorable> SingleCharMatcher<Cursor> for CharICase<Cursor> {
    #[inline(always)]
    fn matches(&self, pos: &mut Position, cursor: Cursor) -> bool {
        match cursor.next(pos) {
            Some(c2) => c2 == self.c || Cursor::CharProps::fold(c2) == self.c,
            _ => false,
        }
    }
}

/// Insn::CharSet
pub struct CharSet<'a> {
    pub chars: &'a [char; MAX_CHAR_SET_LENGTH],
}

impl<'a, Cursor: Cursorable> SingleCharMatcher<Cursor> for CharSet<'a> {
    #[inline(always)]
    fn matches(&self, pos: &mut Position, cursor: Cursor) -> bool {
        match cursor.next(pos) {
            Some(c) => charset_contains(self.chars, c.as_char()),
            None => false,
        }
    }
}

/// Insn::Bracket
pub struct Bracket<'a> {
    pub bc: &'a BracketContents,
}

impl<'a, Cursor: Cursorable> SingleCharMatcher<Cursor> for Bracket<'a> {
    #[inline(always)]
    fn matches(&self, pos: &mut Position, cursor: Cursor) -> bool {
        match cursor.next(pos) {
            Some(c) => Cursor::CharProps::bracket(self.bc, c),
            _ => false,
        }
    }
}

/// Insn::MatchAnyExceptLineTerminator
pub struct MatchAnyExceptLineTerminator {}
impl MatchAnyExceptLineTerminator {
    pub fn new() -> Self {
        Self {}
    }
}
impl<Cursor: Cursorable> SingleCharMatcher<Cursor> for MatchAnyExceptLineTerminator {
    #[inline(always)]
    fn matches(&self, pos: &mut Position, cursor: Cursor) -> bool {
        match cursor.next(pos) {
            Some(c2) => !Cursor::CharProps::is_line_terminator(c2),
            _ => false,
        }
    }
}

/// Any ByteSet may match a single char.
pub struct MatchByteSet<'a, Bytes: ByteSet> {
    pub bytes: &'a Bytes,
}

impl<'a, Cursor: Cursorable, Bytes: ByteSet> SingleCharMatcher<Cursor> for MatchByteSet<'a, Bytes> {
    #[inline(always)]
    fn matches(&self, pos: &mut Position, cursor: Cursor) -> bool {
        if let Some(b) = cursor.next_byte(pos) {
            self.bytes.contains(b)
        } else {
            false
        }
    }
}

/// Provide a variant for ByteArraySet where we hold it directly.
/// The arrays are small and so we don't want to indirect through a pointer.
pub struct MatchByteArraySet<ArraySet: SmallArraySet> {
    pub bytes: ByteArraySet<ArraySet>,
}

impl<Cursor: Cursorable, ArraySet: SmallArraySet> SingleCharMatcher<Cursor>
    for MatchByteArraySet<ArraySet>
{
    #[inline(always)]
    fn matches(&self, pos: &mut Position, cursor: Cursor) -> bool {
        if let Some(b) = cursor.next_byte(pos) {
            self.bytes.0.contains(b)
        } else {
            false
        }
    }
}

/// A ByteSeq of length <= 4 may match a single char.
pub struct MatchByteSeq<'a, Bytes: ByteSeq> {
    pub bytes: &'a Bytes,
}

impl<'a, Cursor: Cursorable, Bytes: ByteSeq> SingleCharMatcher<Cursor> for MatchByteSeq<'a, Bytes> {
    #[inline(always)]
    fn matches(&self, pos: &mut Position, cursor: Cursor) -> bool {
        debug_assert!(
            Bytes::LENGTH <= 4,
            "This looks like it could match more than one char"
        );
        cursor.try_match_lit(pos, self.bytes)
    }
}
