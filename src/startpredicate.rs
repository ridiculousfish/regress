use crate::bytesearch::ByteBitmap;
use crate::codepointset;
use crate::insn::StartPredicate;
use crate::ir;
use crate::ir::Node;
use crate::util::utf8_first_byte;
use std::cmp;
use std::convert::TryInto;

/// Support for quickly finding potential match locations.

/// Convert the code point set to a first-byte bitmap.
/// That is, make a list of all of the possible first bytes of every contained
/// code point, and store that in a bitmap.
fn cps_to_first_byte_bitmap(input: &codepointset::CodePointSet) -> Box<ByteBitmap> {
    // Rather than walking over all contained codepoints, which could be very
    // expensive, we perform searches over all bytes.
    // Note that UTF-8 first-bytes increase monotone and contiguously.
    // Increasing a code point will either increase the first byte by 1 or 0.
    let mut bitmap = Box::new(ByteBitmap::default());
    let mut ivs = input.intervals();
    for target_byte in 0..=255 {
        let search = ivs.binary_search_by(|iv| {
            let left_byte = utf8_first_byte(iv.first);
            let right_byte = utf8_first_byte(iv.last);
            debug_assert!(left_byte <= right_byte);
            if left_byte <= target_byte && target_byte <= right_byte {
                cmp::Ordering::Equal
            } else if left_byte > target_byte {
                cmp::Ordering::Greater
            } else {
                debug_assert!(right_byte < target_byte, "Binary search logic is wrong");
                cmp::Ordering::Less
            }
        });
        if let Ok(idx) = search {
            bitmap.set(target_byte);
            // Since our bytes are strictly increasing, previous intervals will never hold
            // the next byte. Help out our binary search by removing previous intervals.
            ivs = &ivs[idx..];
        }
    }
    bitmap
}

/// The "IR" for a start predicate.
enum AbstractStartPredicate {
    /// No predicate.
    Arbitrary,

    /// Sequence of non-empty bytes.
    Sequence(Vec<u8>),

    /// Set of bytes.
    Set(Box<ByteBitmap>),
}

impl AbstractStartPredicate {
    /// \return the disjunction of two predicates.
    /// That is, a predicate that matches x OR y.
    fn disjunction(x: Self, y: Self) -> Self {
        match (x, y) {
            (Self::Arbitrary, _) => Self::Arbitrary,
            (_, Self::Arbitrary) => Self::Arbitrary,

            (Self::Sequence(s1), Self::Sequence(s2)) => {
                // Compute the length of the shared prefix.
                let shared_len = s1.iter().zip(s2.iter()).take_while(|(a, b)| a == b).count();
                debug_assert!(s1[..shared_len] == s2[..shared_len]);
                if shared_len > 0 {
                    // Use the shared prefix.
                    Self::Sequence(s1[..shared_len].to_vec())
                } else {
                    // Use a set of their first byte.
                    Self::Set(Box::new(ByteBitmap::new(&[s1[0], s2[0]])))
                }
            }

            (Self::Set(mut s1), Self::Set(s2)) => {
                s1.bitor(s2.as_ref());
                Self::Set(s1)
            }

            (Self::Set(mut s1), Self::Sequence(s2)) => {
                // Add first byte to set.
                s1.set(s2[0]);
                Self::Set(s1)
            }

            (Self::Sequence(s1), Self::Set(mut s2)) => {
                s2.set(s1[0]);
                Self::Set(s2)
            }
        }
    }

    /// Resolve ourselves to a concrete start predicate.
    fn resolve_to_insn(self) -> StartPredicate {
        match self {
            Self::Arbitrary => StartPredicate::Arbitrary,
            Self::Sequence(vals) => match vals.len() {
                0 => StartPredicate::Arbitrary,
                1 => StartPredicate::ByteSeq1(vals[..].try_into().unwrap()),
                2 => StartPredicate::ByteSeq2(vals[..].try_into().unwrap()),
                3 => StartPredicate::ByteSeq3(vals[..].try_into().unwrap()),
                _ => StartPredicate::ByteSeq4(vals[..4].try_into().unwrap()),
            },
            Self::Set(bm) => match bm.count_bits() {
                0 => StartPredicate::Arbitrary,
                1 => StartPredicate::ByteSeq1(bm.to_vec()[..].try_into().unwrap()),
                2 => StartPredicate::ByteSet2(bm.to_vec()[..].try_into().unwrap()),
                _ => StartPredicate::ByteBracket(*bm),
            },
        }
    }
}

/// Compute any start-predicate for a node..
/// If this returns None, then the instruction is conceptually zero-width (e.g.
/// lookahead assertion) and does not contribute to the predicate.
/// If this returns StartPredicate::Arbitrary, then there is no predicate.
fn compute_start_predicate(n: &Node) -> Option<AbstractStartPredicate> {
    let arbitrary = Some(AbstractStartPredicate::Arbitrary);
    match n {
        Node::ByteSequence(bytevec) => Some(AbstractStartPredicate::Sequence(bytevec.clone())),
        Node::ByteSet(bytes) => Some(AbstractStartPredicate::Set(Box::new(ByteBitmap::new(
            bytes,
        )))),

        Node::Empty => arbitrary,
        Node::Goal => arbitrary,
        Node::BackRef(..) => arbitrary,

        Node::CharSet(chars) => {
            // Pick the first bytes out.
            let bytes = chars
                .iter()
                .map(|&c| utf8_first_byte(c as u32))
                .collect::<Vec<_>>();
            Some(AbstractStartPredicate::Set(Box::new(ByteBitmap::new(
                &bytes,
            ))))
        }

        // We assume that most char nodes have been optimized to ByteSeq or AnyBytes2, so skip
        // these.
        // TODO: we could support icase through bitmap of de-folded first bytes.
        Node::Char { .. } => arbitrary,

        // Cats return the first non-None value, if any.
        Node::Cat(nodes) => nodes.iter().filter_map(compute_start_predicate).next(),

        // MatchAny (aka .) is too common to do a fast prefix search for.
        Node::MatchAny => arbitrary,

        // MatchAnyExceptLineTerminator (aka .) is too common to do a fast prefix search for.
        Node::MatchAnyExceptLineTerminator => arbitrary,

        // TODO: can probably exploit some of these.
        Node::Anchor(..) => arbitrary,
        Node::WordBoundary { .. } => arbitrary,

        // Capture groups delegate to their contents.
        Node::CaptureGroup(child, ..) | Node::NamedCaptureGroup(child, ..) => {
            compute_start_predicate(child)
        }

        // Zero-width assertions are one of the few instructions that impose no start predicate.
        Node::LookaroundAssertion { .. } => None,

        Node::Loop { loopee, quant, .. } => {
            // TODO: we could try to join two predicates if the loop were optional.
            if quant.min > 0 {
                compute_start_predicate(loopee)
            } else {
                arbitrary
            }
        }

        Node::Loop1CharBody { loopee, quant } => {
            // TODO: we could try to join two predicates if the loop were optional.
            if quant.min > 0 {
                compute_start_predicate(loopee)
            } else {
                arbitrary
            }
        }

        // This one is interesting - we compute the disjunction of the predicates of our two arms.
        Node::Alt(left, right) => {
            if let (Some(x), Some(y)) = (
                compute_start_predicate(left),
                compute_start_predicate(right),
            ) {
                Some(AbstractStartPredicate::disjunction(x, y))
            } else {
                // This indicates that one of our branches could match the empty string.
                arbitrary
            }
        }

        // Brackets get a bitmap.
        Node::Bracket(bc) => {
            // If our bracket is inverted, construct the set of code points not contained.
            let storage;
            let cps = if bc.invert {
                storage = bc.cps.inverted();
                &storage
            } else {
                &bc.cps
            };
            let bitmap = cps_to_first_byte_bitmap(cps);
            Some(AbstractStartPredicate::Set(bitmap))
        }

        // TODO: not sure if we can do something here
        Node::UnicodePropertyEscape { .. } => arbitrary,
    }
}

/// \return the start predicate for a Regex.
pub fn predicate_for_re(re: &ir::Regex) -> StartPredicate {
    compute_start_predicate(&re.node)
        .unwrap_or(AbstractStartPredicate::Arbitrary)
        .resolve_to_insn()
}
