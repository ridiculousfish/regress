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

        // TODO: can probably exploit some of these.
        Node::Anchor { .. } => arbitrary,
        Node::WordBoundary { .. } => arbitrary,

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

/// \return the start predicate for a Regex.
pub fn predicate_for_re(re: &ir::Regex) -> StartPredicate {
    // Check if the regex is anchored to the start - if so, we can optimize
    // by avoiding string searching entirely. However, only do this when
    // multiline mode is disabled, since in multiline mode ^ can match
    // at the beginning of any line, not just the string start.
    if is_start_anchored(&re.node) && !re.flags.multiline {
        return StartPredicate::StartAnchored;
    }

    compute_start_predicate(&re.node)
        .unwrap_or(AbstractStartPredicate::Arbitrary)
        .resolve_to_insn()
}

#[cfg(feature = "utf16")]
pub use units::predicate_for_re as unit_predicate_for_re;

/// The start predicate in UTF-16 code units, for UTF-16 and UCS-2 input.
///
/// Every code point a match can begin with is resolved to the code unit it
/// begins with in UTF-16: itself below U+10000, its high surrogate above. That
/// holds for both inputs: UCS-2 never matches a code point above U+FFFF, and a
/// lone surrogate in UTF-16 input is its own code point.
#[cfg(feature = "utf16")]
mod units {
    use crate::bytesearch::ByteBitmap;
    use crate::codepointset::CodePointSet;
    use crate::insn::UnitStartPredicate;
    use crate::ir;
    use crate::ir::Node;
    #[cfg(not(feature = "std"))]
    use alloc::{vec, vec::Vec};

    /// Larger sets fall back to an arbitrary predicate.
    const MAX_SET_UNITS: usize = 256;

    /// The "IR" for a start predicate.
    enum Units {
        /// No predicate.
        Arbitrary,

        /// Non-empty sequence of code units.
        Sequence(Vec<u16>),

        /// Sorted, deduplicated, non-empty set of code units.
        Set(Vec<u16>),
    }

    struct Prefix {
        units: Units,

        /// Whether the node matches exactly `units`, so that the node after it
        /// in a catenation extends the sequence.
        exact: bool,
    }

    fn inexact(units: Units) -> Option<Prefix> {
        Some(Prefix {
            units,
            exact: false,
        })
    }

    fn set(mut units: Vec<u16>) -> Units {
        units.sort_unstable();
        units.dedup();
        if units.is_empty() || units.len() > MAX_SET_UNITS {
            Units::Arbitrary
        } else {
            Units::Set(units)
        }
    }

    /// \return the code unit a code point begins with in UTF-16.
    fn first_unit(cp: u32) -> u16 {
        if cp <= 0xFFFF {
            cp as u16
        } else {
            0xD800 | ((cp - 0x1_0000) >> 10).min(0x3FF) as u16
        }
    }

    fn push_units(cp: u32, out: &mut Vec<u16>) {
        if cp <= 0xFFFF {
            out.push(cp as u16);
        } else {
            out.push(first_unit(cp));
            out.push(0xDC00 | ((cp - 0x1_0000) & 0x3FF) as u16);
        }
    }

    fn first_units_of_set(cps: &CodePointSet) -> Units {
        let mut units = Vec::new();
        for iv in cps.intervals() {
            let mut cp = iv.first;
            while cp <= iv.last {
                if units.len() > MAX_SET_UNITS {
                    return Units::Arbitrary;
                }
                units.push(first_unit(cp));
                cp = if cp <= 0xFFFF {
                    cp + 1
                } else {
                    // Skip to the first code point with the next high surrogate.
                    (((cp - 0x1_0000) >> 10) + 1) * 0x400 + 0x1_0000
                };
            }
        }
        set(units)
    }

    /// \return the disjunction of two predicates.
    fn disjunction(x: Units, y: Units) -> Units {
        match (x, y) {
            (Units::Arbitrary, _) | (_, Units::Arbitrary) => Units::Arbitrary,
            (Units::Sequence(s1), Units::Sequence(s2)) => {
                let shared_len = s1.iter().zip(s2.iter()).take_while(|(a, b)| a == b).count();
                if shared_len > 0 {
                    Units::Sequence(s1[..shared_len].to_vec())
                } else {
                    set(vec![s1[0], s2[0]])
                }
            }
            (Units::Set(mut s1), Units::Set(s2)) => {
                s1.extend(s2);
                set(s1)
            }
            (Units::Set(mut s), Units::Sequence(q)) | (Units::Sequence(q), Units::Set(mut s)) => {
                s.push(q[0]);
                set(s)
            }
        }
    }

    /// Compute any start predicate for a node, as `compute_start_predicate`
    /// does for bytes. None means the node is zero-width.
    fn compute(n: &Node) -> Option<Prefix> {
        let arbitrary = inexact(Units::Arbitrary);
        match n {
            Node::Char { c } => {
                let mut units = Vec::new();
                push_units(*c, &mut units);
                Some(Prefix {
                    units: Units::Sequence(units),
                    exact: true,
                })
            }

            // Literal bytes are only formed without UTF-16 support.
            Node::ByteSequence(bytes) => match core::str::from_utf8(bytes) {
                Ok(s) if !s.is_empty() => Some(Prefix {
                    units: Units::Sequence(s.encode_utf16().collect()),
                    exact: true,
                }),
                _ => arbitrary,
            },
            Node::ByteSet(bytes) if bytes.is_ascii() => {
                inexact(set(bytes.iter().map(|&b| b.into()).collect()))
            }
            Node::ByteSet(..) => arbitrary,

            Node::CharSet(chars) => inexact(set(chars.iter().map(|&c| first_unit(c)).collect())),

            Node::Bracket(bc) => {
                let storage;
                let cps = if bc.invert {
                    storage = bc.cps.inverted();
                    &storage
                } else {
                    &bc.cps
                };
                inexact(first_units_of_set(cps))
            }

            Node::Empty
            | Node::Goal
            | Node::BackRef { .. }
            | Node::StringSet { .. }
            | Node::MatchAny
            | Node::MatchAnyExceptLineTerminator
            | Node::Anchor { .. }
            | Node::WordBoundary { .. } => arbitrary,

            Node::CaptureGroup { contents, .. } => compute(contents),

            Node::LookaroundAssertion { .. } => None,

            Node::Loop { loopee, quant, .. } | Node::Loop1CharBody { loopee, quant } => {
                if quant.min > 0 {
                    compute(loopee).map(|p| Prefix { exact: false, ..p })
                } else {
                    arbitrary
                }
            }

            Node::Alt(left, right) => match (compute(left), compute(right)) {
                (Some(x), Some(y)) => inexact(disjunction(x.units, y.units)),
                _ => arbitrary,
            },

            // Cats take the first non-None value, and an exact sequence goes on
            // to take the sequences of the nodes after it.
            Node::Cat(nodes) => {
                let mut rest = nodes.iter();
                let mut prefix = rest.by_ref().find_map(compute)?;
                if let Units::Sequence(seq) = &mut prefix.units {
                    while prefix.exact {
                        let Some(node) = rest.next() else { break };
                        match compute(node) {
                            // Zero-width; the sequence carries on past it.
                            None => {}
                            Some(Prefix {
                                units: Units::Sequence(more),
                                exact,
                            }) => {
                                seq.extend(more);
                                prefix.exact = exact;
                            }
                            Some(_) => prefix.exact = false,
                        }
                    }
                }
                Some(prefix)
            }
        }
    }

    /// \return the code unit start predicate for a Regex.
    pub fn predicate_for_re(re: &ir::Regex) -> UnitStartPredicate {
        match compute(&re.node).map_or(Units::Arbitrary, |p| p.units) {
            Units::Arbitrary => UnitStartPredicate::Arbitrary,
            Units::Sequence(seq) if seq.len() == 1 => UnitStartPredicate::UnitSet1([seq[0]]),
            Units::Sequence(seq) => UnitStartPredicate::UnitSeq(seq.into_boxed_slice()),
            Units::Set(units) => match *units.as_slice() {
                [a] => UnitStartPredicate::UnitSet1([a]),
                [a, b] => UnitStartPredicate::UnitSet2([a, b]),
                [a, b, c] => UnitStartPredicate::UnitSet3([a, b, c]),
                _ if units.iter().all(|&u| u <= 0xFF) => {
                    let bytes: Vec<u8> = units.iter().map(|&u| u as u8).collect();
                    UnitStartPredicate::Latin1Bracket(ByteBitmap::new(&bytes))
                }
                _ => UnitStartPredicate::Arbitrary,
            },
        }
    }

    #[cfg(test)]
    mod tests {
        use crate::api::{Flags, Regex};
        use crate::bytesearch::ByteBitmap;
        use crate::insn::{CompiledRegex, UnitStartPredicate};
        use crate::{emit, optimizer, parse};
        #[cfg(not(feature = "std"))]
        use alloc::vec::Vec;

        fn compile(pattern: &str, flags: &str) -> Option<CompiledRegex> {
            let flags = Flags::from(flags);
            let mut ire = parse::try_parse(pattern.chars().map(u32::from), flags).ok()?;
            if !flags.no_opt {
                optimizer::optimize(&mut ire);
            }
            Some(emit::emit(&ire))
        }

        fn predicate(pattern: &str, flags: &str) -> UnitStartPredicate {
            compile(pattern, flags).unwrap().unit_start_pred
        }

        fn seq(s: &str) -> UnitStartPredicate {
            UnitStartPredicate::UnitSeq(s.encode_utf16().collect())
        }

        #[test]
        fn predicates() {
            use UnitStartPredicate::*;
            assert_eq!(predicate("abc", ""), seq("abc"));
            assert_eq!(predicate("abc", "u"), seq("abc"));
            assert_eq!(
                predicate(
                    r"(?<![\p{L}\p{N}])(?:needle)(?:'s|’s|s'|s)?(?![\p{L}\p{N}])",
                    "u"
                ),
                seq("needle")
            );
            assert_eq!(predicate(r"the\s+harbor", "u"), seq("the"));
            assert_eq!(predicate(r"a(?=b)bc+d", ""), seq("abc"));
            assert_eq!(predicate("😀x", "u"), seq("😀x"));
            assert_eq!(predicate("😀|😃", "u"), UnitSet1([0xD83D]));
            assert_eq!(predicate("abc|abd", ""), seq("ab"));
            assert_eq!(predicate("a|b", ""), UnitSet2([0x61, 0x62]));
            assert_eq!(predicate("k", "i"), UnitSet2([0x4B, 0x6B]));
            assert_eq!(predicate("k", "iu"), UnitSet3([0x4B, 0x6B, 0x212A]));
            assert_eq!(predicate("(x)+y", ""), UnitSet1([0x78]));
            assert_eq!(
                predicate("[A-Za-z]x", ""),
                Latin1Bracket(ByteBitmap::new(
                    &(b'A'..=b'Z').chain(b'a'..=b'z').collect::<Vec<_>>()
                ))
            );
            assert_eq!(predicate("[\u{10000}-\u{10FFFF}]", "u"), Arbitrary);
            assert_eq!(predicate("[😀-😂]", "u"), UnitSet1([0xD83D]));
            assert_eq!(predicate("x*", ""), Arbitrary);
            assert_eq!(predicate("x|", ""), Arbitrary);
            assert_eq!(predicate("[^a]", ""), Arbitrary);
            assert_eq!(predicate(r"\p{L}", "u"), Arbitrary);
            assert_eq!(predicate(".", ""), Arbitrary);
            assert_eq!(predicate(r"(?=a)", ""), Arbitrary);
        }

        /// Search every start offset with and without the predicate.
        fn assert_same_matches(pattern: &str, flags: &str, texts: &[Vec<u16>]) {
            let Some(cr) = compile(pattern, flags) else {
                return;
            };
            let mut unassisted = cr.clone();
            unassisted.unit_start_pred = UnitStartPredicate::Arbitrary;
            let (assisted, unassisted) = (Regex::from(cr), Regex::from(unassisted));
            let spans = |m: crate::Match| (m.range, m.captures);

            for text in texts {
                for start in 0..=text.len() {
                    assert_eq!(
                        assisted
                            .find_from_utf16(text, start)
                            .map(spans)
                            .collect::<Vec<_>>(),
                        unassisted
                            .find_from_utf16(text, start)
                            .map(spans)
                            .collect::<Vec<_>>(),
                        "utf16 /{pattern}/{flags} from {start} over {text:x?}"
                    );
                    assert_eq!(
                        assisted
                            .find_from_ucs2(text, start)
                            .map(spans)
                            .collect::<Vec<_>>(),
                        unassisted
                            .find_from_ucs2(text, start)
                            .map(spans)
                            .collect::<Vec<_>>(),
                        "ucs2 /{pattern}/{flags} from {start} over {text:x?}"
                    );
                }
            }
        }

        #[test]
        fn predicates_never_hide_a_match() {
            let patterns = [
                "abc",
                "a|b",
                "abc|abd",
                "k",
                "s",
                "ß",
                "é",
                "\u{212A}",
                "[A-Za-z]c",
                "[^a]b",
                "x*",
                "x+",
                "(a)\\1",
                "(?<=a)b",
                "a(?=b)b",
                "(?<![\\p{L}\\p{N}])(?:ab)(?:'s|s)?(?![\\p{L}\\p{N}])",
                "^a",
                "a$",
                "\\bab",
                ".b",
                "(?:ab)+c",
                "😀",
                "😀a",
                "a😀",
                "[😀-😂]",
                "\\uD83D",
                "\\uDE00",
                "\\uD83D\\uDE00",
                "a\\uDE00",
                "[\\uDC00-\\uDFFF]",
                "[\\uD800-\\uDBFF]a",
                "\\u{1F600}",
            ];
            let alphabet: [u16; 16] = [
                0x61, 0x62, 0x63, 0x6B, 0x4B, 0x73, 0x78, 0x27, 0x20, 0xE9, 0xDF, 0x212A, 0xD83D,
                0xDE00, 0xDE02, 0xDC00,
            ];
            let mut texts: Vec<Vec<u16>> = ["", "abc", "a😀b😃", "xxabdabc kK\u{212A}"]
                .iter()
                .map(|s| s.encode_utf16().collect())
                .collect();
            let mut seed = 0x2545_f491_4f6c_dd1d_u64;
            for len in 0..200 {
                let text = (0..len % 12)
                    .map(|_| {
                        seed = seed
                            .wrapping_mul(6_364_136_223_846_793_005)
                            .wrapping_add(1_442_695_040_888_963_407);
                        alphabet[(seed >> 33) as usize % alphabet.len()]
                    })
                    .collect();
                texts.push(text);
            }
            for pattern in patterns {
                for flags in ["", "i", "u", "iu"] {
                    assert_same_matches(pattern, flags, &texts);
                }
            }
        }
    }
}
