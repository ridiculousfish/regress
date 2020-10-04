use crate::bytesearch::ByteSeq;
use crate::indexing::{ElementType, InputIndexer, Position};
use crate::types::Range;
use std::hint::unreachable_unchecked;
use std::marker::PhantomData;

pub trait Cursorable: InputIndexer {
    /// Whether this Cursor is tracking forward.
    const FORWARD: bool;

    /// This cursor, going forward.
    type ForwardForm: Cursorable;

    /// This cursor, going forward.
    type BackwardForm: Cursorable;

    /// \return a subcursor of this cursor.
    fn subcursor(&self, r: Range) -> Self;

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
    input: Inp,
    pd: PhantomData<Dir>,
}

impl<Dir: Direction, Inp: InputIndexer> Cursorable for Cursor<Dir, Inp> {
    const FORWARD: bool = Dir::FORWARD;

    type ForwardForm = Cursor<Forward, Inp>;
    type BackwardForm = Cursor<Backward, Inp>;

    fn remaining_len(&self, pos: Position) -> usize {
        if Self::FORWARD {
            self.input.bytelength() - pos.0
        } else {
            pos.0
        }
    }

    #[inline(always)]
    fn next_byte(&self, pos: &mut Position) -> Option<u8> {
        let mc = if Self::FORWARD {
            self.input.peek_byte_right(*pos)
        } else {
            self.input.peek_byte_left(*pos)
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
            self.input.slice(pos.0..self.input.bytelength())
        } else {
            self.input.slice(0..pos.0)
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
                (pos.0)..(pos.0 + len)
            } else {
                (pos.0 - len)..pos.0
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
            pos.0 += amt;
        } else {
            pos.0 -= amt;
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
                pos.0 += c.bytelength()
            } else {
                pos.0 -= c.bytelength()
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
                pos.0 -= c.bytelength();
            } else {
                pos.0 += c.bytelength();
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
            self.input.slice(pos.0..pos.0 + range_len)
        } else {
            self.input.slice(pos.0 - range_len..pos.0)
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

// Annoying delegation boilerplate since there's no inheritance.
impl<Dir: Direction, Inp: InputIndexer> InputIndexer for Cursor<Dir, Inp> {
    type Element = Inp::Element;
    type CharProps = Inp::CharProps;

    fn contents(&self) -> &[u8] {
        self.input.contents()
    }
    fn slice(&self, range: Range) -> &[u8] {
        self.input.slice(range)
    }
    fn peek_right(&self, idx: Position) -> Option<Self::Element> {
        self.input.peek_right(idx)
    }
    fn peek_left(&self, idx: Position) -> Option<Self::Element> {
        self.input.peek_left(idx)
    }
    fn peek_byte_right(&self, idx: Position) -> Option<u8> {
        self.input.peek_byte_right(idx)
    }
    fn peek_byte_left(&self, idx: Position) -> Option<u8> {
        self.input.peek_byte_left(idx)
    }
    fn index_after_inc(&self, idx: Position) -> Option<Position> {
        self.input.index_after_inc(idx)
    }
    fn index_after_exc(&self, idx: Position) -> Option<Position> {
        self.input.index_after_exc(idx)
    }
    fn subinput(&self, r: Range) -> Self {
        Cursor {
            input: self.input.subinput(r),
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
