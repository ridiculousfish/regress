//! Bytecode instructions for a compiled regex

use std::collections::HashMap;

use crate::api;
use crate::bytesearch::{AsciiBitmap, ByteArraySet, ByteBitmap};
use crate::types::{BracketContents, CaptureGroupID};
use crate::unicode::PropertyEscape;

type JumpTarget = u32;

/// The maximum size of a byte sequence instruction.
pub const MAX_BYTE_SEQ_LENGTH: usize = 16;

/// The maximum size of an array-type-byteset instruction.
pub const MAX_BYTE_SET_LENGTH: usize = 4;

/// The maximum size of an array-type-charset instruction.
/// This also happens to be the maximum number of characters in case-insensitive
/// equivalence classes.
pub const MAX_CHAR_SET_LENGTH: usize = 4;

#[derive(Debug, Clone)]
pub struct LoopFields {
    pub loop_id: u32,
    pub min_iters: usize,
    pub max_iters: usize,
    pub greedy: bool,
    pub exit: JumpTarget,
}

#[derive(Debug, Clone)]
/// The list of bytecode instructions.
pub enum Insn {
    /// The match was successful.
    Goal,

    /// Match a single char.
    Char(u32),

    /// Match a single char, case-insensitive.
    CharICase(u32),

    /// Match the start of a line (if multiline); emitted by '^'
    StartOfLine,

    /// Match the end of a line; emitted by '$'
    EndOfLine,

    /// Match any character except a line terminator; emitted by '.' only when
    /// the dot_all flag is set to true.
    MatchAny,

    /// Match any character except a line terminator; emitted by '.'
    MatchAnyExceptLineTerminator,

    /// Enter a loop from "outside".
    EnterLoop(LoopFields),

    /// Re-enter a loop.
    LoopAgain {
        begin: JumpTarget,
    },

    /// The next instruction is a "1Char" instruction which always matches one
    /// character. Attempt to match it [min, max] times.
    Loop1CharBody {
        min_iters: usize,
        max_iters: usize,
        greedy: bool,
    },

    /// Set the IP to a new value.
    Jump {
        target: JumpTarget,
    },

    /// The next instruction is the primary branch.
    /// If it fails to match, jump to secondary.
    Alt {
        secondary: JumpTarget,
    },

    /// Enter a capture group.
    BeginCaptureGroup(CaptureGroupID),

    /// Exit a capture group.
    EndCaptureGroup(CaptureGroupID),

    /// Clear a capture group.
    ResetCaptureGroup(CaptureGroupID),

    /// Perform a backreference match.
    BackRef(u32),

    /// Match the next character against a bracket.
    /// TODO: this is a very heavyweight instruction, consider breaking it up.
    Bracket(BracketContents),

    /// A simple bitmap bracket for ASCII.
    /// It contains a bitmap of the range [0, 127].
    AsciiBracket(AsciiBitmap),

    /// Perform a lookahead assertion.
    Lookahead {
        negate: bool,
        start_group: CaptureGroupID,
        end_group: CaptureGroupID,
        continuation: JumpTarget,
    },

    /// Perform a lookbehind assertion.
    Lookbehind {
        negate: bool,
        start_group: CaptureGroupID,
        end_group: CaptureGroupID,
        continuation: JumpTarget,
    },

    /// \w or \W word boundaries.
    WordBoundary {
        invert: bool,
    },

    /// Match any of the contained chars
    /// There is no length field; characters are simply duplicated as necessary.
    CharSet([u32; MAX_CHAR_SET_LENGTH]),

    /// Match the next byte against some possibilities.
    ByteSet2(ByteArraySet<[u8; 2]>),
    ByteSet3(ByteArraySet<[u8; 3]>),
    ByteSet4(ByteArraySet<[u8; 4]>),

    /// Match a sequence of literal bytes.
    ByteSeq1([u8; 1]),
    ByteSeq2([u8; 2]),
    ByteSeq3([u8; 3]),
    ByteSeq4([u8; 4]),
    ByteSeq5([u8; 5]),
    ByteSeq6([u8; 6]),
    ByteSeq7([u8; 7]),
    ByteSeq8([u8; 8]),
    ByteSeq9([u8; 9]),
    ByteSeq10([u8; 10]),
    ByteSeq11([u8; 11]),
    ByteSeq12([u8; 12]),
    ByteSeq13([u8; 13]),
    ByteSeq14([u8; 14]),
    ByteSeq15([u8; 15]),
    ByteSeq16([u8; 16]),

    // TODO: doc comment
    UnicodePropertyEscape {
        property_escape: PropertyEscape,
        negate: bool,
    },

    /// An instruction that always fails, which may be produced in weird cases
    /// like an inverted bracket which matches everything.
    JustFail,
}

/// The peeled prefix start predicate.
/// This is a fast way of locating the first potential match.
#[derive(Debug, Copy, Clone)]
pub enum StartPredicate {
    /// May match an arbitrary sequence.
    Arbitrary,

    /// Look for literal bytes.
    ByteSeq1([u8; 1]),
    ByteSeq2([u8; 2]),
    ByteSeq3([u8; 3]),
    ByteSeq4([u8; 4]),

    /// Look for any of the contained bytes.
    ByteSet2([u8; 2]),

    /// Look for a byte which matches the bitmap.
    ByteBracket(ByteBitmap),
}

#[derive(Debug, Clone)]
pub struct CompiledRegex {
    pub insns: Vec<Insn>,
    pub start_pred: StartPredicate,
    pub loops: u32,
    pub groups: u32,
    pub named_group_indices: HashMap<String, u16>,
    pub flags: api::Flags,
}
