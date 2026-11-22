//! Bytecode instructions for a compiled regex

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

use crate::api;
use crate::bytesearch::{ByteArraySet, ByteBitmap, TruncatedAsciiBitmap};
use crate::types::{BracketContents, CaptureGroupID, LargeLoopBoundsID, Loop1CharID, LoopID};
use crate::util::DebugCheckIndex;
extern crate memchr;
use memchr::memmem;

type JumpTarget = u32;

/// The maximum size of an inline byte sequence instruction.
pub const MAX_INLINE_BYTE_SEQ_LENGTH: usize = 15;

/// The maximum size of an array-type-charset instruction.
/// This also happens to be the maximum number of characters in case-insensitive
/// equivalence classes.
pub const MAX_CHAR_SET_LENGTH: usize = 4;

/// The inclusive iteration bounds of a loop. An unbounded loop has a max of
/// usize::MAX.
#[derive(Debug, Copy, Clone)]
pub struct LoopBounds {
    pub min: usize,
    pub max: usize,
}

/// Loop bounds small enough to store in an instruction, which nearly all are.
/// Larger ones are stored in `CompiledRegex::large_loop_bounds`.
#[derive(Debug, Copy, Clone)]
pub struct InlineLoopBounds {
    min: u32,
    // Here u32::MAX is considered unbounded.
    // Loops with a literal max count of u32::MAX are stored out-of-line.
    max: u32,
}

impl InlineLoopBounds {
    /// The inline form of usize::MAX, the max of an unbounded loop.
    const UNBOUNDED: u32 = u32::MAX;

    fn pack(bound: usize) -> Option<u32> {
        if bound == usize::MAX {
            Some(Self::UNBOUNDED)
        } else {
            u32::try_from(bound).ok().filter(|&b| b != Self::UNBOUNDED)
        }
    }

    fn unpack(bound: u32) -> usize {
        if bound == Self::UNBOUNDED {
            usize::MAX
        } else {
            bound as usize
        }
    }
}

impl TryFrom<LoopBounds> for InlineLoopBounds {
    type Error = ();

    /// Fails if either bound is too large to store inline.
    fn try_from(bounds: LoopBounds) -> Result<Self, ()> {
        Ok(InlineLoopBounds {
            min: Self::pack(bounds.min).ok_or(())?,
            max: Self::pack(bounds.max).ok_or(())?,
        })
    }
}

impl From<InlineLoopBounds> for LoopBounds {
    #[inline(always)]
    fn from(bounds: InlineLoopBounds) -> Self {
        LoopBounds {
            min: InlineLoopBounds::unpack(bounds.min),
            max: InlineLoopBounds::unpack(bounds.max),
        }
    }
}

/// The fields of a loop, from its `EnterLoop` or `EnterLoopLarge` instruction.
#[derive(Debug, Clone)]
pub struct LoopFields {
    pub loop_id: LoopID,
    pub bounds: LoopBounds,
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

    /// Enter a loop from "outside". `CompiledRegex::loop_fields` reads this
    /// as a `LoopFields`.
    EnterLoop {
        loop_id: LoopID,
        greedy: bool,
        bounds: InlineLoopBounds,
        exit: JumpTarget,
    },

    /// An `EnterLoop` whose bounds are too large to store inline. They are at
    /// this index in `CompiledRegex::large_loop_bounds`.
    EnterLoopLarge {
        loop_id: LoopID,
        greedy: bool,
        bounds: LargeLoopBoundsID,
        exit: JumpTarget,
    },

    /// Re-enter a loop.
    LoopAgain {
        begin: JumpTarget,
    },

    /// The next instruction is a "1Char" instruction which always matches one
    /// character. Attempt to match it [min, max] times. A 1-char loop with
    /// bounds too large to store inline is emitted as an ordinary loop.
    Loop1CharBody {
        /// Index of this instruction's IP in `CompiledRegex::loop1char_ips`.
        id: Loop1CharID,
        bounds: InlineLoopBounds,
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

    /// Match an ASCII bracket whose members all lie in
    /// `TruncatedAsciiBitmap::VALID_RANGE`.
    AsciiBracket(TruncatedAsciiBitmap),

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

// Instructions are squeezed hard. Don't grow these without a good reason.
const _: () = assert!(core::mem::size_of::<Insn>() == 16);

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

    // The loop bounds too large to store in an instruction, which are rare.
    pub large_loop_bounds: Vec<LoopBounds>,

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

impl CompiledRegex {
    /// \return the bounds at \p id in `large_loop_bounds`.
    #[inline(always)]
    fn large_loop_bounds(&self, id: LargeLoopBoundsID) -> LoopBounds {
        core::hint::cold_path();
        *self.large_loop_bounds.iat(id as usize)
    }

    /// \return the fields of the loop whose `EnterLoop` or `EnterLoopLarge`
    /// instruction is at \p ip.
    #[inline(always)]
    pub fn loop_fields(&self, ip: usize) -> LoopFields {
        match *self.insns.iat(ip) {
            Insn::EnterLoop {
                loop_id,
                greedy,
                bounds,
                exit,
            } => LoopFields {
                loop_id,
                bounds: bounds.into(),
                greedy,
                exit,
            },
            Insn::EnterLoopLarge {
                loop_id,
                greedy,
                bounds,
                exit,
            } => LoopFields {
                loop_id,
                bounds: self.large_loop_bounds(bounds),
                greedy,
                exit,
            },
            _ => rs_unreachable!("Should be an EnterLoop instruction"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_u32_max_loop_bound_is_large() {
        let inline = |min, max| InlineLoopBounds::try_from(LoopBounds { min, max }).ok();
        let max = u32::MAX as usize;

        // An unbounded loop's usize::MAX is stored inline, as u32::MAX.
        let unbounded = inline(0, usize::MAX).expect("Unbounded should be inline");
        assert_eq!(LoopBounds::from(unbounded).max, usize::MAX);

        // So a literal count of u32::MAX must not be stored inline, where it
        // would read back as unbounded. It goes out-of-line, as would any
        // larger count.
        assert!(inline(0, max).is_none());
        assert!(inline(max, usize::MAX).is_none());
        assert!(inline(0, max + 1).is_none());

        // One less is the largest inline count, and reads back exactly.
        let largest = LoopBounds::from(inline(max - 1, max - 1).expect("Should be inline"));
        assert_eq!((largest.min, largest.max), (max - 1, max - 1));
    }

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
