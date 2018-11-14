use crate::codepointset::CodePointSet;
use crate::cursor::Position;

pub type Range = std::ops::Range<usize>;

/// A group index is u16.
/// CaptureGroupID 0 corresponds to the first capture group.
pub type CaptureGroupID = u16;

/// The maximum number of capture groups supported.
pub const MAX_CAPTURE_GROUPS: usize = 65535;

/// The maximum number of loops supported.
pub const MAX_LOOPS: usize = 65535;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CharacterClassType {
    Digits,
    Spaces,
    Words,
}

/// The stuff in a bracket.
#[derive(Debug, Clone, Default)]
pub struct BracketContents {
    pub invert: bool,
    pub cps: CodePointSet,
}

/// An instruction pointer.
pub type IP = usize;

/// Representation of a loop.
#[derive(Debug, Copy, Clone)]
pub struct LoopData {
    pub iters: usize,
    pub entry: Position,
}

impl LoopData {
    pub fn new() -> LoopData {
        LoopData {
            iters: 0,
            entry: Position { pos: 0 },
        }
    }
}

/// Representation of a capture group.
#[derive(Debug, Copy, Clone)]
pub struct GroupData {
    pub start: Position,
    pub end: Position,
}

impl GroupData {
    pub const NOT_MATCHED: usize = std::usize::MAX;

    pub fn new() -> GroupData {
        GroupData {
            start: Position {
                pos: GroupData::NOT_MATCHED,
            },
            end: Position {
                pos: GroupData::NOT_MATCHED,
            },
        }
    }

    pub fn start_matched(&self) -> bool {
        self.start.pos != GroupData::NOT_MATCHED
    }

    pub fn end_matched(&self) -> bool {
        self.end.pos != GroupData::NOT_MATCHED
    }

    pub fn as_range(&self) -> Option<Range> {
        // Note: we may have only start_matched (if forwards) or end_matched (if
        // backwards) set.
        if self.start_matched() && self.end_matched() {
            Some(self.start.pos..self.end.pos)
        } else {
            None
        }
    }

    pub fn reset(&mut self) {
        self.start.pos = GroupData::NOT_MATCHED;
        self.end.pos = GroupData::NOT_MATCHED;
    }
}
