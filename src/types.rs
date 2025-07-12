use crate::codepointset::{CodePointSet, CodePointSetInner};
use crate::position::PositionType;
#[cfg(not(feature = "std"))]
use alloc::string::String;
use core::ops;

/// A group index is u16.
/// CaptureGroupID 0 corresponds to the first capture group.
pub type CaptureGroupID = u16;

/// The name of a named capture group.
pub type CaptureGroupName = String;

/// The maximum number of capture groups supported.
pub const MAX_CAPTURE_GROUPS: usize = 65535;

/// The maximum number of loops supported.
pub const MAX_LOOPS: usize = 65535;
pub type LoopID = u16;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CharacterClassType {
    Digits,
    Spaces,
    Words,
}

/// The stuff in a bracket.
#[derive(Debug, Clone)]
pub struct BracketContents {
    pub invert: bool,
    pub cps: CodePointSet,
}

impl BracketContents {
    /// \return whether the bracket \p bc matches the given character \p c,
    /// respecting case. Respects 'invert'.
    #[inline(always)]
    pub(crate) fn bracket(&self, cp: u32) -> bool {
        if self.cps.contains(cp) {
            return !self.invert;
        }
        self.invert
    }
}

impl From<BracketContentsInner<'_>> for BracketContents {
    fn from(inner: BracketContentsInner<'_>) -> Self {
        let BracketContentsInner { invert, cps } = inner;
        BracketContents {
            invert,
            cps: cps.into(),
        }
    }
}

/// The stuff in a bracket.
#[derive(Debug, Clone)]
pub struct BracketContentsInner<'b> {
    pub invert: bool,
    pub cps: CodePointSetInner<'b>,
}

impl<'b> BracketContentsInner<'b> {
    // Return true if the bracket is empty.
    pub fn is_empty(&self) -> bool {
        match self.invert {
            false => self.cps.is_empty(),
            true => self.cps.contains_all_codepoints(),
        }
    }
}

/// An instruction pointer.
pub type IP = usize;

/// Representation of a loop.
#[derive(Debug, Copy, Clone)]
pub struct LoopData<Position: PositionType> {
    pub iters: usize,
    pub entry: Position,
}

impl<Position: PositionType> LoopData<Position> {
    pub fn new(entry: Position) -> LoopData<Position> {
        LoopData { iters: 0, entry }
    }
}

/// Representation of a capture group.
#[derive(Debug, Copy, Clone)]
pub struct GroupData<Position: PositionType> {
    pub start: Option<Position>,
    pub end: Option<Position>,
}

impl<Position: PositionType> GroupData<Position> {
    pub fn new() -> GroupData<Position> {
        GroupData {
            start: None,
            end: None,
        }
    }

    pub fn start_matched(&self) -> bool {
        self.start.is_some()
    }

    pub fn end_matched(&self) -> bool {
        self.end.is_some()
    }

    pub fn as_range(&self) -> Option<ops::Range<Position>> {
        // Note: we may have only start_matched (if forwards) or end_matched (if
        // backwards) set.
        match (self.start, self.end) {
            (Some(start), Some(end)) => Some(ops::Range { start, end }),
            _ => None,
        }
    }

    /// Reset the group to "not entered."
    pub fn reset(&mut self) {
        self.start = None;
        self.end = None;
    }
}
