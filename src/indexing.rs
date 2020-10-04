use crate::matchers;
use crate::types::Range;
use crate::util::DebugCheckIndex;
use std::convert::TryInto;
use std::str;

// A position in our input string.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position(pub usize);

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

// A helper type that holds a string and allows indexing into it.
pub trait InputIndexer: std::fmt::Debug + Copy + Clone
where
    Self::CharProps: matchers::CharProperties<Element = Self::Element>,
{
    /// The char type, typically u8 or char.
    type Element: ElementType;

    /// The CharProperties to use for the given element.
    type CharProps: matchers::CharProperties<Element = Self::Element>;

    /// \return the byte contents.
    fn contents(&self) -> &[u8];

    /// \return the length of the contents, in bytes.
    fn bytelength(&self) -> usize {
        self.contents().len()
    }

    /// \return a slice of the contents.
    fn slice(&self, range: Range) -> &[u8];

    /// \return the char to the right (starting at) \p idx, or None if we are at
    /// the end.
    fn peek_right(&self, idx: Position) -> Option<Self::Element>;

    /// \return the char to the left (ending just before) \p idx, or None if we
    /// are at the start.
    fn peek_left(&self, idx: Position) -> Option<Self::Element>;

    /// \return the byte to the right (starting at) \p idx, or None if we are at
    /// the end.
    fn peek_byte_right(&self, idx: Position) -> Option<u8>;

    /// \return the byte to the left (ending just before) \p idx, or None if we
    /// are at the start.
    fn peek_byte_left(&self, idx: Position) -> Option<u8>;

    /// \return the index of the char after \p idx, or None if none.
    /// This will return the one-past-the-last index.
    fn index_after_inc(&self, idx: Position) -> Option<Position>;

    /// \return the index of the char before \p idx, or None if none.
    /// This will NOT return the one-past-the-last index.
    fn index_after_exc(&self, idx: Position) -> Option<Position>;

    /// Create a sub-input from a Range.
    fn subinput(&self, r: Range) -> Self;
}

/// \return the length of a UTF8 sequence starting with this byte.
#[inline(always)]
fn utf8_seq_len(b: u8) -> usize {
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

    /// \return a byte value at an index.
    #[inline(always)]
    fn get_byte(&self, idx: Position) -> u8 {
        debug_assert!(
            idx.0 < self.input.len() && is_seq_start(self.contents()[idx.0]),
            "Invalid index"
        );
        *self.contents().iat(idx.0)
    }

    /// \return a slice as a str.
    #[inline(always)]
    fn str_slice(&self, range: Range) -> &'a str {
        self.assert_is_boundary(Position(range.start));
        self.assert_is_boundary(Position(range.end));
        if cfg!(feature = "prohibit-unsafe") {
            &self.input[range]
        } else {
            unsafe { self.input.get_unchecked(range) }
        }
    }

    #[inline(always)]
    fn assert_is_boundary(&self, idx: Position) {
        debug_assert!(idx.0 == self.input.len() || is_seq_start(self.contents()[idx.0]))
    }
}

impl<'a> InputIndexer for Utf8Input<'a> {
    type Element = char;
    type CharProps = matchers::UTF8CharProperties;

    #[inline(always)]
    fn contents(&self) -> &[u8] {
        self.input.as_bytes()
    }

    #[inline(always)]
    fn slice(&self, range: Range) -> &[u8] {
        debug_assert!(range.start <= range.end && range.end <= self.contents().len());
        self.contents().iat(range)
    }

    #[inline(always)]
    fn peek_right(&self, idx: Position) -> Option<char> {
        self.assert_is_boundary(idx);
        self.str_slice(idx.0..self.bytelength()).chars().next()
    }

    #[inline(always)]
    fn peek_left(&self, idx: Position) -> Option<char> {
        self.assert_is_boundary(idx);
        self.str_slice(0..idx.0).chars().rev().next()
    }

    #[inline(always)]
    fn peek_byte_right(&self, idx: Position) -> Option<u8> {
        let c = self.contents();
        debug_assert!(idx.0 <= c.len(), "Index is out of bounds");
        if idx.0 == c.len() {
            None
        } else {
            Some(*c.iat(idx.0))
        }
    }

    #[inline(always)]
    fn peek_byte_left(&self, idx: Position) -> Option<u8> {
        let c = self.contents();
        debug_assert!(idx.0 <= c.len(), "Index is out of bounds");
        if idx.0 == 0 {
            None
        } else {
            Some(*c.iat(idx.0 - 1))
        }
    }

    #[inline(always)]
    fn index_after_inc(&self, idx: Position) -> Option<Position> {
        debug_assert!(idx.0 <= self.input.len(), "Invalid index");
        if idx.0 == self.input.len() {
            None
        } else {
            let res = idx.0 + utf8_seq_len(self.get_byte(idx));
            debug_assert!(res <= self.input.len(), "Should be in bounds");
            Some(Position(res))
        }
    }

    #[inline(always)]
    fn index_after_exc(&self, idx: Position) -> Option<Position> {
        debug_assert!(idx.0 <= self.input.len(), "Invalid index");
        let len = self.input.len();
        if idx.0 == len {
            None
        } else {
            let res = idx.0 + utf8_seq_len(self.get_byte(idx));
            debug_assert!(res <= self.input.len(), "Should be in bounds");
            if res < self.input.len() {
                Some(Position(res))
            } else {
                None
            }
        }
    }

    fn subinput(&self, r: Range) -> Self {
        Self {
            input: &self.input[r],
        }
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
}

impl<'a> InputIndexer for AsciiInput<'a> {
    type Element = u8;
    type CharProps = matchers::ASCIICharProperties;

    #[inline(always)]
    fn contents(&self) -> &[u8] {
        self.input
    }

    #[inline(always)]
    fn slice(&self, range: Range) -> &[u8] {
        debug_assert!(
            range.start <= range.end && range.end <= self.input.len(),
            "Slice out of bounds"
        );
        self.input.iat(range)
    }

    #[inline(always)]
    fn peek_right(&self, idx: Position) -> Option<Self::Element> {
        if idx.0 == self.input.len() {
            None
        } else {
            Some(*self.input.iat(idx.0))
        }
    }

    #[inline(always)]
    fn peek_left(&self, idx: Position) -> Option<Self::Element> {
        if idx.0 == 0 {
            None
        } else {
            Some(*self.input.iat(idx.0 - 1))
        }
    }

    #[inline(always)]
    fn peek_byte_right(&self, idx: Position) -> Option<u8> {
        self.peek_right(idx)
    }

    #[inline(always)]
    fn peek_byte_left(&self, idx: Position) -> Option<u8> {
        self.peek_left(idx)
    }

    fn index_after_inc(&self, mut idx: Position) -> Option<Position> {
        if idx.0 < self.input.len() {
            idx.0 += 1;
            Some(idx)
        } else {
            None
        }
    }

    #[inline(always)]
    fn index_after_exc(&self, mut idx: Position) -> Option<Position> {
        if idx.0 + 1 < self.input.len() {
            idx.0 += 1;
            Some(idx)
        } else {
            None
        }
    }

    fn subinput(&self, r: Range) -> Self {
        Self {
            input: self.input.iat(r),
        }
    }
}
