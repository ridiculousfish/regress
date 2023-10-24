use crate::bytesearch::{charset_contains, ByteArraySet, ByteSeq, ByteSet, SmallArraySet};
use crate::cursor::Direction;
use crate::indexing::{ElementType, InputIndexer};
use crate::insn::MAX_CHAR_SET_LENGTH;
use crate::matchers::CharProperties;
use crate::types::BracketContents;
use crate::unicode::PropertyEscape;
use crate::{cursor, CASE_MATCHER};

/// A trait for things that match a single Element.
pub trait SingleCharMatcher<Input: InputIndexer, Dir: Direction> {
    /// \return whether we match the character at the given position, advancing
    /// the position if so. On a false return, the position is unspecified.
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, icase: bool) -> bool;
}

/// Insn::Char
pub struct Char<Input: InputIndexer> {
    pub c: Input::Element,
}
impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for Char<Input> {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, _icase: bool) -> bool {
        match cursor::next(input, dir, pos) {
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
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, _icase: bool) -> bool {
        match cursor::next(input, dir, pos) {
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
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, icase: bool) -> bool {
        match cursor::next(input, dir, pos) {
            Some(c) => {
                let mut c = c.as_u32();
                if icase {
                    c = CASE_MATCHER.simple_fold(char::from_u32(c).unwrap()).into();
                }
                charset_contains(self.chars, c)
            }
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
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, icase: bool) -> bool {
        match cursor::next(input, dir, pos) {
            Some(c) if icase => {
                let c = CASE_MATCHER.simple_fold(char::from_u32(c.into()).unwrap());
                Input::CharProps::bracket(self.bc, c.into())
            }
            Some(c) => Input::CharProps::bracket(self.bc, c.into()),
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
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, _icase: bool) -> bool {
        // If there is a character, it counts as a match.
        cursor::next(input, dir, pos).is_some()
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
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, _icase: bool) -> bool {
        match cursor::next(input, dir, pos) {
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
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, icase: bool) -> bool {
        if let Some(mut b) = cursor::next_byte(input, dir, pos) {
            if icase {
                b = CASE_MATCHER.simple_fold(char::from(b)) as u8;
            }
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
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, icase: bool) -> bool {
        if let Some(mut b) = cursor::next_byte(input, dir, pos) {
            if icase {
                b = CASE_MATCHER.simple_fold(char::from(b)) as u8;
            }
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
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, icase: bool) -> bool {
        debug_assert!(
            Bytes::LENGTH <= 4,
            "This looks like it could match more than one char"
        );
        cursor::try_match_lit(input, dir, pos, self.bytes, icase)
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
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position, _icase: bool) -> bool {
        match cursor::next(input, dir, pos) {
            Some(c2) => self.property_escape.contains(c2.into()),
            _ => false,
        }
    }
}
