use crate::bytesearch::ByteSeq;
use crate::indexing::InputIndexer;

#[derive(Debug, Copy, Clone)]
pub struct Forward;

#[derive(Debug, Copy, Clone)]
pub struct Backward;

pub trait Direction: std::fmt::Debug + Copy + Clone {
    const FORWARD: bool;
    fn new() -> Self;
}

impl Direction for Forward {
    const FORWARD: bool = true;
    #[inline(always)]
    fn new() -> Self {
        Forward {}
    }
}

impl Direction for Backward {
    const FORWARD: bool = false;
    #[inline(always)]
    fn new() -> Self {
        Backward {}
    }
}

/// \return a slice of bytes of length \p len starting (or ending if not FORWARD) at \p pos.
/// Advance (retreat) pos by that many bytes.
#[inline(always)]
fn try_slice<'a, Input: InputIndexer, Dir: Direction>(
    input: &'a Input,
    _dir: Dir,
    pos: &mut Input::Position,
    len: usize,
) -> Option<&'a [u8]> {
    // Note we may exit here if there's not enough bytes remaining.
    let start;
    let end;
    if Dir::FORWARD {
        start = *pos;
        end = input.try_move_right(start, len)?;
        *pos = end;
    } else {
        end = *pos;
        start = input.try_move_left(end, len)?;
        *pos = start;
    }
    Some(input.slice(start, end))
}

/// \return whether we match some literal bytes.
/// If so, update the position. If not, the position is unspecified.
#[inline(always)]
pub fn try_match_lit<Input: InputIndexer, Dir: Direction, Bytes: ByteSeq>(
    input: &Input,
    dir: Dir,
    pos: &mut Input::Position,
    bytes: &Bytes,
) -> bool {
    let len = Bytes::LENGTH;
    debug_assert!(len > 0, "Should not have zero length");
    if let Some(subr_slice) = try_slice(input, dir, pos, len) {
        bytes.equals_known_len(subr_slice)
    } else {
        false
    }
}

/// If the subrange [start, end) is byte-for-byte equal to a range of the same length starting (if FORWARD is true) or ending (if FORWARD
/// is false) at \p pos, then return true and then advance (or retreat) the position.
/// On failure, return false and the position is unspecified.
pub fn subrange_eq<Input: InputIndexer, Dir: Direction>(
    input: &Input,
    dir: Dir,
    pos: &mut Input::Position,
    start: Input::Position,
    end: Input::Position,
) -> bool {
    if let Some(subr_slice) = try_slice(input, dir, pos, end - start) {
        subr_slice == input.slice(start, end)
    } else {
        false
    }
}

/// \return the next character, updating the position.
#[inline(always)]
pub fn next<Input: InputIndexer, Dir: Direction>(
    input: &Input,
    _dir: Dir,
    pos: &mut Input::Position,
) -> Option<Input::Element> {
    if Dir::FORWARD {
        input.next_right(pos)
    } else {
        input.next_left(pos)
    }
}

/// \return the next *byte*, or None if at the end, updating the position.
/// Note this may break UTF8 sequences.
#[inline(always)]
pub fn next_byte<Input: InputIndexer, Dir: Direction>(
    input: &Input,
    _dir: Dir,
    pos: &mut Input::Position,
) -> Option<u8> {
    let res;
    if Dir::FORWARD {
        res = input.peek_byte_right(*pos);
        *pos += if res.is_some() { 1 } else { 0 };
    } else {
        res = input.peek_byte_left(*pos);
        *pos -= if res.is_some() { 1 } else { 0 };
    }
    res
}
