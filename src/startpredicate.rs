//! Support for quickly finding potential match locations.
use crate::bytesearch::ByteBitmap;
use crate::codepointset;
use crate::insn::StartPredicate;
use crate::ir;
use crate::ir::Node;
use crate::util::{add_utf8_first_bytes_to_bitmap, utf8_first_byte};
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};
use memchr::memmem;

/// Check if a node is anchored to the start of the line/string.
/// Returns true if the node begins with a StartOfLine anchor.
fn is_start_anchored(n: &Node) -> bool {
    match n {
        Node::Anchor {
            anchor_type: ir::AnchorType::StartOfLine,
            multiline,
        } => !multiline,
        Node::Cat(nodes) => {
            // For concatenation, check if the first node is start-anchored
            nodes.first().is_some_and(is_start_anchored)
        }
        Node::CaptureGroup { contents, .. } => is_start_anchored(contents),
        // For alternation, both arms must be start-anchored
        Node::Alt(left, right) => is_start_anchored(left) && is_start_anchored(right),
        // Other nodes are not anchored
        _ => false,
    }
}

/// Convert the code point set to a first-byte bitmap.
/// That is, make a list of all of the possible first bytes of every contained
/// code point, and store that in a bitmap.
fn cps_to_first_byte_bitmap(input: &codepointset::CodePointSet) -> Box<ByteBitmap> {
    let mut bitmap = Box::<ByteBitmap>::default();
    for iv in input.intervals() {
        add_utf8_first_bytes_to_bitmap(*iv, &mut bitmap);
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
                1 => StartPredicate::ByteSet1([vals[0]]),
                _ => StartPredicate::ByteSeq(Box::new(memmem::Finder::new(&vals).into_owned())),
            },
            Self::Set(bm) => match bm.count_bits() {
                0 => StartPredicate::Arbitrary,
                1 => StartPredicate::ByteSet1(bm.as_array()),
                2 => StartPredicate::ByteSet2(bm.as_array()),
                3 => StartPredicate::ByteSet3(bm.as_array()),
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
        Node::BackRef { .. } => arbitrary,

        Node::CharSet(chars) => {
            // Pick the first bytes out.
            let bytes = chars
                .iter()
                .map(|&c| utf8_first_byte(c))
                .collect::<Vec<_>>();
            Some(AbstractStartPredicate::Set(Box::new(ByteBitmap::new(
                &bytes,
            ))))
        }

        // StringSets come from TC39 "sequence properties" and are very rare - not worth optimizing.
        Node::StringSet { .. } => arbitrary,

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

        // Zero-width assertions: like `LookaroundAssertion` below, they consume
        // nothing, so they contribute no predicate and a leading one must be
        // skipped (return `None`) rather than poisoning a `Cat` to `Arbitrary`.
        // This lets `^Sherlock Holmes|Sherlock Holmes$` extract the shared
        // literal prefix. The anchor is still re-verified at each candidate.
        Node::Anchor { .. } => None,
        Node::WordBoundary { .. } => None,

        // Capture groups delegate to their contents.
        Node::CaptureGroup { contents, .. } => compute_start_predicate(contents),

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
    }
}

/// \return whether the entire regex is anchored to the start of input: it
/// begins with a non-multiline `^` (not inside an alternation) and multiline
/// mode is disabled. In multiline mode `^` matches at the start of any line, so
/// the regex is not anchored to the string start.
pub(crate) fn anchored_to_start(re: &ir::Regex) -> bool {
    is_start_anchored(&re.node) && !re.flags.multiline
}

/// If the regex ends in a mandatory literal byte sequence (length >= 2) with at
/// least one consuming element before it, return that literal. This is the
/// "required suffix" used by the reverse-automaton search: every match ends with
/// these bytes, so `memmem` for them finds candidate match ends.
///
/// Conservative on purpose — only a top-level `Cat` (optionally wrapped in a
/// capture group) whose **last** node is a `ByteSequence` qualifies. A pattern
/// that is itself a pure prefix/whole literal is left to the Phase-1 prefilter
/// (it has a usable prefix predicate, so this path isn't reached for it).
pub(crate) fn required_inner_literal(re: &ir::Regex) -> Option<(Vec<Node>, Vec<u8>)> {
    // A single-byte literal is only worth scanning for if it's uncommon (an
    // apostrophe, `@`, `/`…); a common one (space, lowercase letter) stops too
    // often. Multi-byte literals are always selective (memmem). Mirrors
    // `prefilter::byte_is_common`.
    fn worth_scanning(bytes: &[u8]) -> bool {
        match *bytes {
            [b] => !(b == b' ' || b.is_ascii_lowercase()),
            _ => bytes.len() >= 2,
        }
    }
    // Flatten nested `Cat` nodes into one atom sequence — concatenation is
    // associative, so a literal buried in `(?:…){n}`-unrolled sub-`Cat`s (e.g.
    // the `.` in `(?:[0-9]{1,3}\.){3}…`) is exposed at the top level. Everything
    // that isn't a `Cat` (brackets, loops, groups, literals) is an opaque atom.
    fn flatten<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
        match n {
            Node::Cat(nodes) => nodes.iter().for_each(|c| flatten(c, out)),
            other => out.push(other),
        }
    }

    // Unwrap a whole-pattern capture group, then flatten.
    let mut node = &re.node;
    while let Node::CaptureGroup { contents, .. } = node {
        node = contents;
    }
    let mut atoms = Vec::new();
    flatten(node, &mut atoms);

    // Pick the most selective (longest) `ByteSequence` that has at least one
    // consuming atom before it, so the reversed *prefix* (everything before the
    // literal) can be searched leftward to the match start. The part after the
    // literal — possibly nothing, the suffix case — is left to the forward verify.
    let mut best: Option<(usize, &Vec<u8>)> = None;
    let mut have_prefix_atom = false;
    for (k, atom) in atoms.iter().enumerate() {
        match atom {
            Node::Goal | Node::Empty => {}
            Node::ByteSequence(bytes) if have_prefix_atom && worth_scanning(bytes) => {
                if best.is_none_or(|(_, b)| bytes.len() > b.len()) {
                    best = Some((k, bytes));
                }
                have_prefix_atom = true;
            }
            _ => have_prefix_atom = true,
        }
    }
    let (k, lit) = best?;
    let prefix = atoms[..k].iter().map(|n| (*n).clone()).collect();
    Some((prefix, lit.clone()))
}

/// \return the start predicate for a Regex.
pub fn predicate_for_re(re: &ir::Regex) -> StartPredicate {
    // Check if the regex is anchored to the start - if so, we can optimize
    // by avoiding string searching entirely.
    if anchored_to_start(re) {
        return StartPredicate::StartAnchored;
    }

    compute_start_predicate(&re.node)
        .unwrap_or(AbstractStartPredicate::Arbitrary)
        .resolve_to_insn()
}
