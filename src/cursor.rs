use crate::bytesearch::ByteSeq;
use crate::indexing::InputIndexer;

#[derive(Debug, Copy, Clone)]
pub struct Forward;

#[derive(Debug, Copy, Clone)]
pub struct Backward;

pub trait Direction: core::fmt::Debug + Copy + Clone {
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

/// \return whether we match some literal bytes.
/// If so, update the position. If not, the position is unspecified.
#[inline(always)]
pub fn try_match_lit<Input: InputIndexer, Dir: Direction, Bytes: ByteSeq>(
    input: &Input,
    dir: Dir,
    pos: &mut Input::Position,
    bytes: &Bytes,
) -> bool {
    input.match_bytes(dir, pos, bytes)
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
