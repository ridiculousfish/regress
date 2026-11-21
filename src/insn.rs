//! Bytecode instructions for a compiled regex

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

use crate::api;
use crate::bytesearch::{AsciiBitmap, ByteArraySet, ByteBitmap};
use crate::types::{BracketContents, CaptureGroupID, Loop1CharID, LoopID};
extern crate memchr;
use memchr::memmem;

type JumpTarget = u32;

/// The maximum size of an inline byte sequence instruction.
pub const MAX_INLINE_BYTE_SEQ_LENGTH: usize = 15;

/// The maximum size of an array-type-charset instruction.
/// This also happens to be the maximum number of characters in case-insensitive
/// equivalence classes.
pub const MAX_CHAR_SET_LENGTH: usize = 4;

#[derive(Debug, Clone)]
pub struct LoopFields {
    pub loop_id: LoopID,
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

    /// Match the start of a line (if multiline); emitted by '^'
    StartOfLine {
        multiline: bool,
    },

    /// Match the end of a line; emitted by '$'
    EndOfLine {
        multiline: bool,
    },

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
        /// Index of this instruction's IP in `CompiledRegex::loop1char_ips`.
        id: Loop1CharID,
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
    BackRef {
        group: u32,
        icase: bool,
    },

    /// Match the next character against the bracket contents, stored at the given index in the CompiledRegex.
    Bracket(u32),

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

    /// \w or \W word boundaries in Unicode case-insensitive mode.
    /// This considers characters that fold to ASCII word chars as word chars,
    /// per ES9 21.2.2.6.1.
    WordBoundaryUnicodeICase {
        invert: bool,
    },

    /// Match any of MAX_CHAR_SET_LENGTH chars: three packed as 21 bits each in
    /// \p lo, and the last in \p hi. See `pack_char_set`.
    /// There is no length field; characters are simply duplicated as necessary.
    CharSet {
        lo: u64,
        hi: u32,
    },

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

    // Match an out-of-line sequence of literal bytes,
    // indexing into the compiled regex's byte array as [start, end).
    ByteSeqN((u32, u32)),

    /// An instruction that always fails, which may be produced in weird cases
    /// like an inverted bracket which matches everything.
    JustFail,
}

/// Mask of a 21-bit lane of a packed `CharSet`.
const CHAR_SET_LANE: u64 = 0x1F_FFFF;

/// Make a `CharSet` instruction. A code point fits in 21 bits, so three of
/// them fit in a u64, with no lane crossing a word boundary.
pub fn pack_char_set(chars: [u32; MAX_CHAR_SET_LENGTH]) -> Insn {
    let [a, b, c, d] = chars;
    debug_assert!(chars.iter().all(|&c| c <= 0x10FFFF), "Not a code point");
    Insn::CharSet {
        lo: a as u64 | (b as u64) << 21 | (c as u64) << 42,
        hi: d,
    }
}

/// Unpack the chars of a `CharSet` instruction.
#[inline(always)]
pub fn unpack_char_set(lo: u64, hi: u32) -> [u32; MAX_CHAR_SET_LENGTH] {
    [
        (lo & CHAR_SET_LANE) as u32,
        (lo >> 21 & CHAR_SET_LANE) as u32,
        (lo >> 42) as u32,
        hi,
    ]
}

/// The peeled prefix start predicate.
/// This is a fast way of locating the first potential match.
#[derive(Debug, Clone)]
pub enum StartPredicate {
    /// May match an arbitrary sequence.
    Arbitrary,

    /// Look for the first instance of any of the given bytes.
    ByteSet1([u8; 1]),
    ByteSet2([u8; 2]),
    ByteSet3([u8; 3]),

    /// Look for a byte sequence.
    ByteSeq(Box<memmem::Finder<'static>>),

    /// Look for a byte which matches the bitmap.
    ByteBracket(ByteBitmap),

    /// The regex is anchored to the start of the line/string.
    /// This avoids string searching entirely and only tries matching at the beginning.
    StartAnchored,
}

/// The start predicate resolved to UTF-16 code units rather than UTF-8 bytes,
/// for UTF-16 and UCS-2 input, which cannot be searched for bytes.
#[cfg(feature = "utf16")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitStartPredicate {
    /// May match an arbitrary sequence.
    Arbitrary,

    /// Look for the first instance of any of the given code units.
    UnitSet1([u16; 1]),
    UnitSet2([u16; 2]),
    UnitSet3([u16; 3]),

    /// Look for a sequence of code units.
    UnitSeq(Box<[u16]>),

    /// Look for a code unit below 0x100 which matches the bitmap.
    Latin1Bracket(ByteBitmap),
}

#[derive(Debug, Clone)]
pub struct CompiledRegex {
    // Sequence of instructions.
    pub insns: Vec<Insn>,

    // The bracket contents, indexed by the value of the `Bracket` instruction.
    pub brackets: Vec<BracketContents>,

    // The IP of each `Loop1CharBody` instruction, indexed by its id. A
    // backtrack entry names a 1-char loop by id, which is smaller than an IP.
    pub loop1char_ips: Vec<u32>,

    // A vector of bytes for out-of-line byte matches.
    pub byte_literal_buffer: Vec<u8>,

    // Predicate to rapidly find the first potential match.
    pub start_pred: StartPredicate,

    // The same predicate in code units, for UTF-16 and UCS-2 input.
    // An anchored regex is still recognized by `start_pred`.
    #[cfg(feature = "utf16")]
    pub unit_start_pred: UnitStartPredicate,

    // Number of loops, used to populate loop data.
    pub loops: u32,

    // Number of capture groups, used to populate capture group data.
    pub groups: u32,

    // A list of capture group names. This is either:
    //   - Empty, if there were no named capture groups.
    //   - A list of names with length `groups`, corresponding to the capture
    //     group names in order. Groups without names have an empty string.
    pub group_names: Box<[Box<str>]>,

    // Flags controlling matching.
    pub flags: api::Flags,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_set_packing() {
        let pack = |chars| match pack_char_set(chars) {
            Insn::CharSet { lo, hi } => (lo, hi),
            other => panic!("unexpected {:?}", other),
        };

        assert_eq!(
            pack([0x10_2233, 0x04_5566, 0x07_8899, 0x0A_BBCC]),
            (0x1E22_648A_ACD0_2233, 0x0A_BBCC)
        );

        let values = [0, 0xFF, 0x100, 0xFFFF, 0x10000, 0x10FFFF];
        for a in values {
            for b in values {
                for c in values {
                    for d in values {
                        let chars = [a, b, c, d];
                        let (lo, hi) = pack(chars);
                        assert_eq!(unpack_char_set(lo, hi), chars);
                    }
                }
            }
        }
    }
}
