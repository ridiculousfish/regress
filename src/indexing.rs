use crate::bytesearch;
use crate::matchers;
use crate::util::{is_utf8_continuation, utf8_w2, utf8_w3, utf8_w4};
use std::cmp::Eq;
use std::convert::TryInto;
use std::{ops, str};

// A type which may be an Element.
pub trait ElementType:
    std::fmt::Debug
    + Copy
    + Clone
    + std::cmp::Eq
    + std::cmp::Ord
    + std::convert::Into<char>
    + std::convert::Into<u32>
    + std::convert::TryFrom<u32>
{
    /// Return the length of ourself in bytes.
    fn bytelength(self) -> usize;

    /// Return another ElementType as self.
    #[inline(always)]
    fn try_from<Elem: ElementType>(v: Elem) -> Option<Self> {
        // Annoying there is no char->u8 conversion.
        let vv: u32 = v.into();
        vv.try_into().ok()
    }

    #[inline(always)]
    fn as_char(self) -> char {
        self.into()
    }
}

impl ElementType for char {
    #[inline(always)]
    fn bytelength(self) -> usize {
        self.len_utf8()
    }
}

impl ElementType for u8 {
    #[inline(always)]
    fn bytelength(self) -> usize {
        1
    }
}

/// A trait which references a position in the input string.
/// The intent is that this may be satisfied via indexes or pointers.
/// Positions must be subtractable, producing usize.
pub trait PositionType: std::fmt::Debug + Copy + Clone + PartialEq + Eq + PartialOrd + Ord
where
    Self: ops::Add<usize, Output = Self>,
    Self: ops::Sub<usize, Output = Self>,
    Self: ops::Sub<Self, Output = usize>,
    Self: ops::AddAssign<usize>,
    Self: ops::SubAssign<usize>,
{
}

/// A simple index-based position.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexPosition(usize);

impl ops::Add<usize> for IndexPosition {
    type Output = Self;
    fn add(self, rhs: usize) -> Self::Output {
        debug_assert!(self.0 + rhs >= self.0, "Overflow");
        IndexPosition(self.0 + rhs)
    }
}

impl ops::AddAssign<usize> for IndexPosition {
    fn add_assign(&mut self, rhs: usize) {
        *self = *self + rhs;
    }
}

impl ops::SubAssign<usize> for IndexPosition {
    fn sub_assign(&mut self, rhs: usize) {
        *self = *self - rhs;
    }
}

impl ops::Sub<IndexPosition> for IndexPosition {
    type Output = usize;
    fn sub(self, rhs: Self) -> Self::Output {
        debug_assert!(self.0 >= rhs.0, "Underflow");
        self.0 - rhs.0
    }
}

impl ops::Sub<usize> for IndexPosition {
    type Output = IndexPosition;
    fn sub(self, rhs: usize) -> Self::Output {
        debug_assert!(self.0 >= rhs, "Underflow");
        IndexPosition(self.0 - rhs)
    }
}

impl PositionType for IndexPosition {}

/// A reference position holds a reference to a byte and uses pointer arithmetic.
/// This must use raw pointers because it must be capable of representing the one-past-the-end value.
/// TODO: thread lifetimes through this.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RefPosition(*const u8);

impl PositionType for RefPosition {}

impl ops::Add<usize> for RefPosition {
    type Output = Self;
    fn add(self, rhs: usize) -> Self::Output {
        Self(unsafe { self.0.add(rhs) })
    }
}

impl ops::Sub<RefPosition> for RefPosition {
    type Output = usize;
    fn sub(self, rhs: Self) -> Self::Output {
        debug_assert!(self.0 >= rhs.0, "Underflow");
        unsafe { rhs.0.offset_from(self.0) as usize }
    }
}

impl ops::Sub<usize> for RefPosition {
    type Output = RefPosition;
    fn sub(self, rhs: usize) -> Self::Output {
        debug_assert!(self.0 as usize >= rhs, "Underflow");
        Self(unsafe { self.0.sub(rhs) })
    }
}

impl ops::AddAssign<usize> for RefPosition {
    fn add_assign(&mut self, rhs: usize) {
        *self = *self + rhs;
    }
}

impl ops::SubAssign<usize> for RefPosition {
    fn sub_assign(&mut self, rhs: usize) {
        *self = *self - rhs;
    }
}

// A helper type that holds a string and allows indexing into it.
pub trait InputIndexer: std::fmt::Debug + Copy + Clone
where
    Self::CharProps: matchers::CharProperties<Element = Self::Element>,
{
    /// The char type, typically u8 or char.
    type Element: ElementType;

    /// The CharProperties to use for the given element.
    type CharProps: matchers::CharProperties<Element = Self::Element>;

    /// A type which references a position in the input string.
    type Position: PositionType;

    /// \return the byte contents.
    fn contents(&self) -> &[u8];

    /// \return the length of the contents, in bytes.
    fn bytelength(&self) -> usize {
        self.contents().len()
    }

    /// \return a slice of the contents.
    fn slice(&self, start: Self::Position, end: Self::Position) -> &[u8];

    /// \return a sub-input. Note that positions in the original may no longer be valid in the sub-input.
    fn subinput(&self, range: ops::Range<Self::Position>) -> Self;

    /// \return the char to the right (starting at) \p idx, or None if we are at
    /// the end. Advance the position by the amount.
    fn next_right(&self, pos: &mut Self::Position) -> Option<Self::Element>;

    /// \return the char to the left (ending just before) \p idx, or None if we are at
    /// the end. Retreat the position by the amount.
    fn next_left(&self, pos: &mut Self::Position) -> Option<Self::Element>;

    // Like next_right, but does not decode the element.
    fn next_right_pos(&self, pos: Self::Position) -> Option<Self::Position>;

    // Like next_left, but does not decode the element.
    fn next_left_pos(&self, pos: Self::Position) -> Option<Self::Position>;

    /// \return the byte to the right (starting at) \p idx, or None if we are at
    /// the end.
    fn peek_byte_right(&self, pos: Self::Position) -> Option<u8>;

    /// \return the byte to the left (ending just before) \p idx, or None if we
    /// are at the start.
    fn peek_byte_left(&self, pos: Self::Position) -> Option<u8>;

    /// \return a position at the left end of this input.
    fn left_end(&self) -> Self::Position;

    /// \return a position at the right end of this nput.
    fn right_end(&self) -> Self::Position;

    /// Move a position right by a certain amount.
    /// \return the new position, or None if it would exceed the length.
    fn try_move_right(&self, pos: Self::Position, amt: usize) -> Option<Self::Position>;

    /// Move a position left by a certain amount.
    /// \return the new position, or None if it would underflow 0.
    fn try_move_left(&self, pos: Self::Position, amt: usize) -> Option<Self::Position>;

    /// Convert a position to an offset.
    fn pos_to_offset(&self, pos: Self::Position) -> usize;

    /// Apply a literal byte matcher, finding a literal byte sequence in a string.
    /// \return the new position, or None on failure.
    fn find_bytes<Search: bytesearch::ByteSearcher>(
        &self,
        pos: Self::Position,
        search: &Search,
    ) -> Option<Self::Position>;

    /// Peek at the char to the right of a position, without changing that position.
    #[inline(always)]
    fn peek_right(&self, mut pos: Self::Position) -> Option<Self::Element> {
        self.next_right(&mut pos)
    }

    /// Peek at the char to the left of a position, without changing that position.
    #[inline(always)]
    fn peek_left(&self, mut pos: Self::Position) -> Option<Self::Element> {
        self.next_left(&mut pos)
    }
}

/// \return the length of a UTF8 sequence starting with this byte.
#[inline(always)]
const fn utf8_seq_len(b: u8) -> usize {
    if b < 128 {
        1
    } else {
        match b & 0xF0 {
            0xE0 => 3,
            0xF0 => 4,
            _ => 2,
        }
    }
}

/// \return whether a byte represents the start of a utf8 sequence (aka a
/// char boundary).
#[inline(always)]
fn is_seq_start(b: u8) -> bool {
    // Taken from is_char_boundary.
    // "This is bit magic equivalent to: b < 128 || b >= 192"
    (b as i8) >= -0x40
}

#[derive(Debug, Copy, Clone)]
pub struct Utf8Input<'a> {
    input: &'a str,
}

impl<'a> Utf8Input<'a> {
    pub fn new(s: &'a str) -> Self {
        Self { input: s }
    }

    /// \return a byte at a given position.
    /// This asserts that we are not at the right end.
    #[inline(always)]
    fn getb(&self, pos: <Self as InputIndexer>::Position) -> u8 {
        debug_assert!(self.left_end() <= pos && pos < self.right_end());
        if cfg!(feature = "prohibit-unsafe") {
            self.contents()[self.pos_to_offset(pos)]
        } else {
            unsafe { *self.contents().get_unchecked(self.pos_to_offset(pos)) }
        }
    }

    /// Helper to avoid annoying cast.
    #[inline(always)]
    fn pos_to_offset(&self, pos: <Self as InputIndexer>::Position) -> usize {
        self.debug_assert_valid_pos(pos);
        let res = pos.0;
        debug_assert!(res <= self.contents().len(), "Position out of bounds");
        res
    }

    /// \return a slice as a str.
    #[inline(always)]
    fn str_slice(&self, range: ops::Range<<Self as InputIndexer>::Position>) -> &'a str {
        self.debug_assert_boundary(range.start);
        self.debug_assert_boundary(range.end);
        if cfg!(feature = "prohibit-unsafe") {
            &self.input[std::ops::Range {
                start: self.pos_to_offset(range.start),
                end: self.pos_to_offset(range.end),
            }]
        } else {
            unsafe {
                self.input.get_unchecked(std::ops::Range {
                    start: self.pos_to_offset(range.start),
                    end: self.pos_to_offset(range.end),
                })
            }
        }
    }

    /// Assert that a position is valid, i.e. between our left and right ends.
    #[inline(always)]
    fn debug_assert_valid_pos(&self, pos: <Self as InputIndexer>::Position) {
        debug_assert!(self.left_end() <= pos && pos <= self.right_end());
    }

    /// Assert that a position is a valid UTF8 character boundary.
    #[inline(always)]
    fn debug_assert_boundary(&self, pos: <Self as InputIndexer>::Position) {
        self.debug_assert_valid_pos(pos);
        debug_assert!(pos.0 == self.input.len() || is_seq_start(self.getb(pos)));
    }
}

impl<'a> InputIndexer for Utf8Input<'a> {
    type Position = IndexPosition;
    type Element = char;
    type CharProps = matchers::UTF8CharProperties;

    #[inline(always)]
    fn contents(&self) -> &[u8] {
        self.input.as_bytes()
    }

    #[inline(always)]
    fn slice(&self, start: Self::Position, end: Self::Position) -> &[u8] {
        self.debug_assert_valid_pos(start);
        self.debug_assert_valid_pos(end);
        debug_assert!(end >= start, "Slice start after end");
        &self.contents()[std::ops::Range {
            start: start.0,
            end: end.0,
        }]
    }

    #[inline(always)]
    fn subinput(&self, range: ops::Range<Self::Position>) -> Self {
        Self::new(self.str_slice(range))
    }

    #[inline(always)]
    fn next_right(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        self.debug_assert_boundary(*pos);
        if *pos == self.right_end() {
            return None;
        }

        let b0 = self.getb(*pos);
        if b0 < 128 {
            *pos += 1;
            return Some(b0 as Self::Element);
        }

        // Multibyte case.
        let len = utf8_seq_len(b0);
        let codepoint = match len {
            2 => utf8_w2(b0, self.getb(*pos + 1)),
            3 => utf8_w3(b0, self.getb(*pos + 1), self.getb(*pos + 2)),
            4 => utf8_w4(
                b0,
                self.getb(*pos + 1),
                self.getb(*pos + 2),
                self.getb(*pos + 3),
            ),
            _ => rs_unreachable!("Invalid utf8 sequence length"),
        };
        *pos += len;
        if let Some(c) = std::char::from_u32(codepoint) {
            Some(c)
        } else {
            rs_unreachable!("Should have decoded a valid char from utf8 sequence");
        }
    }

    #[inline(always)]
    fn next_right_pos(&self, mut pos: Self::Position) -> Option<Self::Position> {
        self.debug_assert_boundary(pos);
        if pos == self.right_end() {
            return None;
        }

        let b0 = self.getb(pos);
        if b0 < 128 {
            return Some(pos + 1);
        }

        // Multibyte case.
        pos += utf8_seq_len(b0);
        self.debug_assert_boundary(pos);
        Some(pos)
    }

    #[inline(always)]
    fn next_left(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        self.debug_assert_boundary(*pos);
        if *pos == self.left_end() {
            return None;
        }

        let z = self.getb(*pos - 1);
        if z < 128 {
            *pos -= 1;
            return Some(z as Self::Element);
        }

        // Multibyte case.
        // bytes are w x y z, with 'pos' pointing after z.
        let codepoint;
        let y = self.getb(*pos - 2);
        if !is_utf8_continuation(y) {
            codepoint = utf8_w2(y, z);
            *pos -= 2;
        } else {
            let x = self.getb(*pos - 3);
            if !is_utf8_continuation(x) {
                codepoint = utf8_w3(x, y, z);
                *pos -= 3;
            } else {
                let w = self.getb(*pos - 4);
                codepoint = utf8_w4(w, x, y, z);
                *pos -= 4;
            }
        }
        self.debug_assert_boundary(*pos);
        if let Some(c) = std::char::from_u32(codepoint) {
            Some(c)
        } else {
            rs_unreachable!("Should have decoded a valid char from utf8 sequence");
        }
    }

    #[inline(always)]
    fn next_left_pos(&self, mut pos: Self::Position) -> Option<Self::Position> {
        self.debug_assert_boundary(pos);
        if pos == self.left_end() {
            return None;
        }

        let z = self.getb(pos - 1);
        if z < 128 {
            pos -= 1;
            self.debug_assert_valid_pos(pos);
            return Some(pos);
        }

        if !is_utf8_continuation(self.getb(pos - 2)) {
            pos -= 2;
        } else if !is_utf8_continuation(self.getb(pos - 3)) {
            pos -= 3;
        } else {
            debug_assert!(!is_utf8_continuation(self.getb(pos - 4)));
            pos -= 4;
        }
        self.debug_assert_valid_pos(pos);
        Some(pos)
    }

    #[inline(always)]
    fn peek_byte_right(&self, pos: Self::Position) -> Option<u8> {
        self.debug_assert_valid_pos(pos);
        if pos == self.right_end() {
            None
        } else {
            Some(self.getb(pos))
        }
    }

    #[inline(always)]
    fn peek_byte_left(&self, pos: Self::Position) -> Option<u8> {
        self.debug_assert_valid_pos(pos);
        if pos == self.left_end() {
            None
        } else {
            Some(self.getb(pos - 1))
        }
    }

    #[inline(always)]
    fn try_move_right(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if self.right_end() - pos < amt {
            None
        } else {
            pos += amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    #[inline(always)]
    fn try_move_left(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if pos - self.left_end() < amt {
            None
        } else {
            pos -= amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    #[inline(always)]
    fn left_end(&self) -> Self::Position {
        IndexPosition(0)
    }

    #[inline(always)]
    fn right_end(&self) -> Self::Position {
        IndexPosition(self.bytelength())
    }

    #[inline(always)]
    fn pos_to_offset(&self, pos: Self::Position) -> usize {
        pos.0
    }

    fn find_bytes<Search: bytesearch::ByteSearcher>(
        &self,
        mut pos: Self::Position,
        search: &Search,
    ) -> Option<Self::Position> {
        let rem = self.slice(pos, self.right_end());
        let idx = search.find_in(rem)?;
        pos.0 += idx;
        Some(pos)
    }
}

#[derive(Debug, Copy, Clone)]
pub struct AsciiInput<'a> {
    input: &'a [u8],
}

impl<'a> AsciiInput<'a> {
    pub fn new(s: &'a str) -> Self {
        Self {
            input: s.as_bytes(),
        }
    }

    /// \return a byte at a given position.
    /// This asserts that we are not at the right end.
    #[inline(always)]
    fn getb(&self, pos: <Self as InputIndexer>::Position) -> u8 {
        debug_assert!(self.left_end() <= pos && pos < self.right_end());
        if cfg!(feature = "prohibit-unsafe") {
            self.contents()[self.pos_to_offset(pos)]
        } else {
            unsafe { *self.contents().get_unchecked(self.pos_to_offset(pos)) }
        }
    }

    #[inline(always)]
    fn debug_assert_valid_pos(&self, pos: <Self as InputIndexer>::Position) -> &Self {
        debug_assert!(self.left_end() <= pos && pos <= self.right_end());
        self
    }
}

impl<'a> InputIndexer for AsciiInput<'a> {
    type Position = IndexPosition;
    type Element = u8;
    type CharProps = matchers::ASCIICharProperties;

    #[inline(always)]
    fn contents(&self) -> &[u8] {
        self.input
    }

    #[inline(always)]
    fn slice(&self, start: Self::Position, end: Self::Position) -> &[u8] {
        self.debug_assert_valid_pos(start);
        self.debug_assert_valid_pos(end);
        &self.contents()[std::ops::Range {
            start: start.0,
            end: end.0,
        }]
    }

    #[inline(always)]
    fn subinput(&self, range: ops::Range<Self::Position>) -> AsciiInput<'a> {
        self.debug_assert_valid_pos(range.start);
        self.debug_assert_valid_pos(range.end);
        AsciiInput {
            input: &self.input[std::ops::Range {
                start: range.start.0,
                end: range.end.0,
            }],
        }
    }

    #[inline(always)]
    fn next_right(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        self.debug_assert_valid_pos(*pos);
        if *pos == self.right_end() {
            None
        } else {
            let c = self.getb(*pos);
            *pos += 1;
            self.debug_assert_valid_pos(*pos);
            Some(c)
        }
    }

    #[inline(always)]
    fn next_left(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        self.debug_assert_valid_pos(*pos);
        if *pos == self.left_end() {
            None
        } else {
            *pos -= 1;
            self.debug_assert_valid_pos(*pos);
            let c = self.getb(*pos);
            Some(c)
        }
    }

    #[inline(always)]
    fn next_right_pos(&self, pos: Self::Position) -> Option<Self::Position> {
        self.try_move_right(pos, 1)
    }

    #[inline(always)]
    fn next_left_pos(&self, pos: Self::Position) -> Option<Self::Position> {
        self.try_move_left(pos, 1)
    }

    #[inline(always)]
    fn peek_byte_right(&self, mut pos: Self::Position) -> Option<u8> {
        self.next_right(&mut pos)
    }

    #[inline(always)]
    fn peek_byte_left(&self, mut pos: Self::Position) -> Option<u8> {
        self.next_left(&mut pos)
    }

    fn left_end(&self) -> Self::Position {
        IndexPosition(0)
    }

    fn right_end(&self) -> Self::Position {
        IndexPosition(self.bytelength())
    }

    fn try_move_right(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if self.right_end() - pos < amt {
            None
        } else {
            pos += amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    fn try_move_left(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if pos - self.left_end() < amt {
            None
        } else {
            pos -= amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    fn pos_to_offset(&self, pos: Self::Position) -> usize {
        pos.0
    }

    fn find_bytes<Search: bytesearch::ByteSearcher>(
        &self,
        mut pos: Self::Position,
        search: &Search,
    ) -> Option<Self::Position> {
        let rem = self.slice(pos, self.right_end());
        let idx = search.find_in(rem)?;
        pos.0 += idx;
        Some(pos)
    }
}
