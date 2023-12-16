use crate::bytesearch::{charset_contains, ByteArraySet, ByteSeq, ByteSet, SmallArraySet};
use crate::cursor;
use crate::cursor::Direction;
use crate::indexing::{ElementType, InputIndexer};
use crate::insn::MAX_CHAR_SET_LENGTH;
use crate::matchers::CharProperties;
use crate::types::BracketContents;
use crate::unicode::{is_character_class, PropertyEscape};

/// A trait for things that match a single Element.
pub trait SingleCharMatcher<Input: InputIndexer, Dir: Direction> {
    /// \return whether we match the character at the given position, advancing
    /// the position if so. On a false return, the position is unspecified.
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, unicode: bool) -> bool;
}

/// Insn::Char
pub struct Char<Input: InputIndexer> {
    pub c: Input::Element,
}
impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for Char<Input> {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, unicode: bool) -> bool {
        match cursor::next(input, dir, pos, unicode) {
            Some(c2) => c2 == self.c,
            _ => false,
        }
    }
}

/// Insn::CharICase
pub struct CharICase<Input: InputIndexer> {
    pub c: Input::Element,
}
impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for CharICase<Input> {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, unicode: bool) -> bool {
        match cursor::next(input, dir, pos, unicode) {
            Some(c2) => c2 == self.c || Input::CharProps::fold(c2) == self.c,
            _ => false,
        }
    }
}

/// Insn::CharSet
pub struct CharSet<'a> {
    pub chars: &'a [u32; MAX_CHAR_SET_LENGTH],
}

impl<'a, Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for CharSet<'a> {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, unicode: bool) -> bool {
        match cursor::next(input, dir, pos, unicode) {
            Some(c) => charset_contains(self.chars, c.as_u32()),
            None => false,
        }
    }
}

/// Insn::Bracket
pub struct Bracket<'a> {
    pub bc: &'a BracketContents,
}

impl<'a, Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for Bracket<'a> {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, unicode: bool) -> bool {
        match cursor::next(input, dir, pos, unicode) {
            Some(c) => Input::CharProps::bracket(self.bc, c),
            _ => false,
        }
    }
}

/// Insn::MatchAny
pub struct MatchAny {}
impl MatchAny {
    pub fn new() -> Self {
        Self {}
    }
}
impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for MatchAny {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, unicode: bool) -> bool {
        // If there is a character, it counts as a match.
        cursor::next(input, dir, pos, unicode).is_some()
    }
}

/// Insn::MatchAnyExceptLineTerminator
pub struct MatchAnyExceptLineTerminator {}
impl MatchAnyExceptLineTerminator {
    pub fn new() -> Self {
        Self {}
    }
}
impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir>
    for MatchAnyExceptLineTerminator
{
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, unicode: bool) -> bool {
        match cursor::next(input, dir, pos, unicode) {
            Some(c2) => !Input::CharProps::is_line_terminator(c2),
            _ => false,
        }
    }
}

/// Any ByteSet may match a single char.
pub struct MatchByteSet<'a, Bytes: ByteSet> {
    pub bytes: &'a Bytes,
}

impl<'a, Input: InputIndexer, Dir: Direction, Bytes: ByteSet> SingleCharMatcher<Input, Dir>
    for MatchByteSet<'a, Bytes>
{
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, _: bool) -> bool {
        if let Some(b) = cursor::next_byte(input, dir, pos) {
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

impl<Input: InputIndexer, Dir: Direction, ArraySet: SmallArraySet> SingleCharMatcher<Input, Dir>
    for MatchByteArraySet<ArraySet>
{
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, _: bool) -> bool {
        if let Some(b) = cursor::next_byte(input, dir, pos) {
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

impl<'a, Input: InputIndexer, Dir: Direction, Bytes: ByteSeq> SingleCharMatcher<Input, Dir>
    for MatchByteSeq<'a, Bytes>
{
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, _: bool) -> bool {
        debug_assert!(
            Bytes::LENGTH <= 4,
            "This looks like it could match more than one char"
        );
        cursor::try_match_lit(input, dir, pos, self.bytes)
    }
}

/// TODO: doc comment
pub struct UnicodePropertyEscape<'a> {
    pub property_escape: &'a PropertyEscape,
}

impl<'a, Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir>
    for UnicodePropertyEscape<'a>
{
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, unicode: bool) -> bool {
        match cursor::next(input, dir, pos, unicode) {
            Some(c2) => is_character_class(c2.into(), self.property_escape),
            _ => false,
        }
    }
}
