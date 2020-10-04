use crate::bytesearch::ByteSeq;
use crate::indexing::{ElementType, InputIndexer, Position};
use crate::matchers::CharProperties;
use crate::types::Range;
use std::hint::unreachable_unchecked;
use std::marker::PhantomData;

pub trait Cursorable: std::fmt::Debug + Copy + Clone {
    /// Whether this Cursor is tracking forward.
    const FORWARD: bool;

    /// The element type, typically char or u8.
    type Element: ElementType;

    /// The input indexer.
    type Input: InputIndexer<Element = Self::Element>;

    /// The CharProperties type.
    type CharProps: CharProperties<Element = Self::Element>;

    /// This cursor, going forward.
    type ForwardForm: Cursorable;

    /// This cursor, going forward.
    type BackwardForm: Cursorable;

    /// \return a subcursor of this cursor.
    fn subcursor(&self, r: Range) -> Self;

    /// \return the character to the right of the position.
    fn peek_right(&self, pos: Position) -> Option<Self::Element>;

    /// \return the character to the left of the position.
    fn peek_left(&self, pos: Position) -> Option<Self::Element>;

    /// \return the next character, updating the position.
    fn next(&self, pos: &mut Position) -> Option<Self::Element>;

    /// \return the next *byte*, or None if at the end, updating the position.
    /// Note this may break UTF8 sequences.
    fn next_byte(&self, pos: &mut Position) -> Option<u8>;

    /// \return how many bytes are remaining.
    fn remaining_len(&self, pos: Position) -> usize;

    /// \return the remaining bytes.
    fn remaining_bytes(&self, pos: Position) -> &[u8];

    /// \return whether the subrange \p range is byte-for-byte equal to a range
    /// of the same length starting (if FORWARD is true) or ending (if FORWARD
    /// is false) at \p pos.
    fn subrange_eq(&self, pos: &mut Position, range: Range) -> bool;

    /// \return whether we match some literal bytes.
    /// If so, update the position. If not, the position is unspecified.
    fn try_match_lit<Bytes: ByteSeq>(&self, pos: &mut Position, bytes: &Bytes) -> bool;

    /// Get this cursor, tracking forward.
    fn as_forward(&self) -> Self::ForwardForm;

    /// Get this cursor, tracking backward.
    fn as_backward(&self) -> Self::BackwardForm;

    /// Advance the position by the given amount.
    fn advance(&self, pos: &mut Position, amt: usize);

    /// Given that pos is a valid position, advance the position in our
    /// direction, by one char.
    fn advance_by_char_known_valid(&self, pos: &mut Position);

    /// Given that pos is a valid position, move the position opposite our
    /// direction, by one char.
    fn retreat_by_char_known_valid(&self, pos: &mut Position);
}

#[derive(Debug, Copy, Clone)]
pub struct Forward;

#[derive(Debug, Copy, Clone)]
pub struct Backward;

pub trait Direction: std::fmt::Debug + Copy + Clone {
    const FORWARD: bool;
}

impl Direction for Forward {
    const FORWARD: bool = true;
}

impl Direction for Backward {
    const FORWARD: bool = false;
}

#[derive(Debug, Copy, Clone)]
pub struct Cursor<Dir: Direction, Inp: InputIndexer> {
    /// The string contents.
    pub input: Inp,
    pd: PhantomData<Dir>,
}

impl<Dir: Direction, Inp: InputIndexer> Cursorable for Cursor<Dir, Inp> {
    const FORWARD: bool = Dir::FORWARD;

    type Input = Inp;
    type Element = Inp::Element;
    type CharProps = Inp::CharProps;
    type ForwardForm = Cursor<Forward, Inp>;
    type BackwardForm = Cursor<Backward, Inp>;

    fn remaining_len(&self, pos: Position) -> usize {
        if Self::FORWARD {
            self.input.bytelength() - pos.pos
        } else {
            pos.pos
        }
    }

    fn peek_right(&self, pos: Position) -> Option<Self::Element> {
        self.input.peek_right(pos.pos)
    }

    fn peek_left(&self, pos: Position) -> Option<Self::Element> {
        self.input.peek_left(pos.pos)
    }

    #[inline(always)]
    fn next_byte(&self, pos: &mut Position) -> Option<u8> {
        let mc = if Self::FORWARD {
            self.input.peek_byte_right(pos.pos)
        } else {
            self.input.peek_byte_left(pos.pos)
        };
        if mc.is_some() {
            self.advance(pos, 1);
        }
        mc
    }

    fn next(&self, pos: &mut Position) -> Option<Self::Element> {
        let mc = if Self::FORWARD {
            self.peek_right(*pos)
        } else {
            self.peek_left(*pos)
        };
        if let Some(c) = mc {
            self.advance(pos, c.bytelength());
        }
        mc
    }

    fn remaining_bytes(&self, pos: Position) -> &[u8] {
        if Self::FORWARD {
            self.input.slice(pos.pos..self.input.bytelength())
        } else {
            self.input.slice(0..pos.pos)
        }
    }

    #[inline(always)]
    fn try_match_lit<Bytes: ByteSeq>(&self, pos: &mut Position, bytes: &Bytes) -> bool {
        let len = Bytes::LENGTH;
        debug_assert!(len > 0, "Should not have zero length");
        if len > self.remaining_len(*pos) {
            false
        } else {
            let r = if Self::FORWARD {
                (pos.pos)..(pos.pos + len)
            } else {
                (pos.pos - len)..pos.pos
            };
            let s1 = self.input.slice(r);
            if s1.len() != len {
                if cfg!(feature = "prohibit-unsafe") {
                    unreachable!();
                } else {
                    unsafe { unreachable_unchecked() }
                }
            } else {
                self.advance(pos, len);
                bytes.equals_known_len(s1)
            }
        }
    }

    fn advance(&self, pos: &mut Position, amt: usize) {
        debug_assert!(amt <= self.remaining_len(*pos), "Advanced out of bounds");
        if Self::FORWARD {
            pos.pos += amt;
        } else {
            pos.pos -= amt;
        }
    }

    #[allow(clippy::collapsible_if)]
    fn advance_by_char_known_valid(&self, pos: &mut Position) {
        let mc = if Self::FORWARD {
            self.peek_right(*pos)
        } else {
            self.peek_left(*pos)
        };
        if let Some(c) = mc {
            if Self::FORWARD {
                pos.pos += c.bytelength()
            } else {
                pos.pos -= c.bytelength()
            }
        } else {
            if cfg!(feature = "prohibit-unsafe") {
                unreachable!("Position was invalid");
            } else {
                unsafe { unreachable_unchecked() }
            }
        }
    }

    #[allow(clippy::collapsible_if)]
    fn retreat_by_char_known_valid(&self, pos: &mut Position) {
        let mc = if Self::FORWARD {
            self.peek_left(*pos)
        } else {
            self.peek_right(*pos)
        };
        if let Some(c) = mc {
            if Self::FORWARD {
                pos.pos -= c.bytelength();
            } else {
                pos.pos += c.bytelength();
            }
        } else {
            if cfg!(feature = "prohibit-unsafe") {
                unreachable!("Position was invalid");
            } else {
                unsafe { unreachable_unchecked() }
            }
        }
    }

    fn subrange_eq(&self, pos: &mut Position, range: Range) -> bool {
        let range_len = range.end - range.start;
        if self.remaining_len(*pos) < range_len {
            return false;
        }
        let range_slice = self.input.slice(range);
        let pos_slice = if Self::FORWARD {
            self.input.slice(pos.pos..pos.pos + range_len)
        } else {
            self.input.slice(pos.pos - range_len..pos.pos)
        };
        if range_slice == pos_slice {
            self.advance(pos, range_len);
            true
        } else {
            false
        }
    }

    fn subcursor(&self, range: Range) -> Self {
        Self {
            input: self.input.subinput(range),
            pd: PhantomData,
        }
    }

    fn as_forward(&self) -> Self::ForwardForm {
        Self::ForwardForm {
            input: self.input,
            pd: PhantomData,
        }
    }

    fn as_backward(&self) -> Self::BackwardForm {
        Self::BackwardForm {
            input: self.input,
            pd: PhantomData,
        }
    }
}

/// \return a Forward cursor to start matching the given input.
pub fn starting_cursor<Input: InputIndexer>(input: Input) -> Cursor<Forward, Input> {
    Cursor {
        input,
        pd: PhantomData,
    }
}
