//! Literal-prefilter driven search for the TDFA backend.
//!
//! The plain TDFA executor builds an *unanchored* automaton (a lazy
//! `MatchAny*?` prefix, see [`Nfa::try_from_unanchored`]) and makes one linear
//! pass over the whole haystack — it never skips, so throughput is flat
//! regardless of how sparse the matches are.
//!
//! When the regex's match must *begin* with a literal or small byte set, we can
//! do much better: use `memchr`/`memmem` (SIMD) to jump straight to candidate
//! positions and run an **anchored** TDFA only there. [`TdfaProgram`] bundles
//! the chosen strategy; [`TdfaProgram::try_from_ir`] picks one from the regex's
//! start predicate.
//!
//! Strategies:
//! - [`Strategy::Scan`] — no usable literal: the original single-pass unanchored
//!   scan. This is also what start-anchored regexes use (their unanchored build
//!   already drops the `.*?` prefix and only tries offset 0).
//! - [`Strategy::Prefix`] — a prefix literal / byte set: `memchr`/`memmem` to the
//!   next candidate, then the anchored TDFA verifies (and extracts captures).
//!   `find` semantics are preserved because the predicate is a necessary
//!   condition on the match's first element, so the leftmost candidate that
//!   verifies is the leftmost match.

use crate::automata::dfa::Dfa;
use crate::automata::nfa::Nfa;
use crate::automata::nfa_backend::NfaMatch;
use crate::automata::reverse;
use crate::automata::tdfa::{self, Tdfa, TdfaStats};
use crate::automata::tdfa_backend::{self, PrefixSkip, Scratch};
use crate::insn::StartPredicate;
use crate::ir;
use crate::startpredicate;
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};
use memchr::memmem;

/// A built TDFA search program: an automaton plus the strategy used to drive it
/// over an input. This is the `Source` consumed by `TdfaExecutor`.
#[derive(Debug)]
pub struct TdfaProgram {
    strategy: Strategy,
    group_names: Box<[Box<str>]>,
}

#[derive(Debug)]
enum Strategy {
    /// No usable literal: one linear pass over the unanchored automaton.
    Scan { unanchored: Tdfa },
    /// Prefix literal / byte set: skip to candidates, anchored TDFA verifies.
    /// `skip` (set only for an exact `ByteSeq` literal with a trivially
    /// replayable traversal) warm-starts each verify past the literal.
    Prefix {
        anchored: Tdfa,
        prefilter: StartPredicate,
        skip: Option<PrefixSkip>,
    },
    /// Required *suffix* literal, no usable prefix (e.g. `\w+\s+Holmes`). Find
    /// the literal with `memmem`, drive the `reverse` DFA leftward from its end
    /// to the leftmost match start, then run the `forward` anchored TDFA there
    /// for the real extent and captures.
    ReverseInner {
        forward: Tdfa,
        reverse: Dfa,
        literal: Box<memmem::Finder<'static>>,
        lit_len: usize,
    },
}

/// Error building a [`TdfaProgram`]: either the NFA or the TDFA stage failed
/// (budget/unsupported feature).
#[derive(Debug)]
pub enum BuildError {
    Nfa(crate::automata::nfa::Error),
    Tdfa(tdfa::Error),
}

impl From<crate::automata::nfa::Error> for BuildError {
    fn from(e: crate::automata::nfa::Error) -> Self {
        BuildError::Nfa(e)
    }
}
impl From<tdfa::Error> for BuildError {
    fn from(e: tdfa::Error) -> Self {
        BuildError::Tdfa(e)
    }
}

/// Bytes that are too common in typical (prose) text for a single-byte-class
/// prefilter to be worth it: skipping to every one of them and running an
/// anchored verify that fails immediately costs more than a straight scan.
/// Lowercase ASCII letters and space dominate English text; uppercase letters,
/// digits, and punctuation are rare enough to make good prefilter bytes. A
/// multi-byte literal (`ByteSeq`) is always selective regardless, since
/// `memmem` matches the whole sequence.
fn byte_is_common(b: u8) -> bool {
    b == b' ' || b.is_ascii_lowercase()
}

/// Whether a start predicate is worth prefiltering on. `Arbitrary` /anchored
/// fall through to `Scan`; an unselective single-byte-class predicate also
/// falls through (prefiltering on it would be slower than scanning).
fn should_prefilter(pred: &StartPredicate) -> bool {
    match pred {
        // A literal sequence (always length >= 2) is selective.
        StartPredicate::ByteSeq(_) => true,
        // A small byte set is worth it only if none of its bytes is common.
        StartPredicate::ByteSet1(bs) => !bs.iter().any(|&b| byte_is_common(b)),
        StartPredicate::ByteSet2(bs) => !bs.iter().any(|&b| byte_is_common(b)),
        StartPredicate::ByteSet3(bs) => !bs.iter().any(|&b| byte_is_common(b)),
        StartPredicate::ByteBracket(bm) => !(0..=255u8).any(|b| byte_is_common(b) && bm.contains(b)),
        StartPredicate::Arbitrary | StartPredicate::StartAnchored => false,
    }
}

impl TdfaProgram {
    /// Build a program from the IR, choosing a prefilter strategy when the
    /// regex's match must begin with a literal/byte set, else falling back to
    /// the unanchored single-pass scan.
    ///
    /// Expects an **optimized** IR (call [`crate::optimizer::optimize`] first),
    /// matching the convention used by `emit` and `Nfa::try_from`. The
    /// optimizer is what lowers `Cat`-of-`Char` runs into `ByteSequence` /
    /// `ByteSet` literals; without it a literal like `Sherlock` stays a chain of
    /// `Char` nodes and yields no prefilter.
    pub fn try_from_ir(re: &ir::Regex) -> Result<Self, BuildError> {
        let pred = startpredicate::predicate_for_re(re);
        if should_prefilter(&pred) {
            // Anchored automaton: matches only at the offset handed to
            // `execute`, so a candidate that fails to match dies fast (no `.*?`
            // skip) and we advance to the next candidate.
            let nfa = Nfa::try_from(re)?;
            let mut anchored = Tdfa::try_from(&nfa)?;
            anchored.optimize();
            // For an exact literal prefix, precompute a warm start that skips
            // re-scanning it through the automaton (a no-op for byte-set/bracket
            // prefixes, which have no fixed literal).
            let skip = match &pred {
                StartPredicate::ByteSeq(finder) => {
                    tdfa_backend::compute_prefix_skip(&anchored, finder.needle())
                }
                _ => None,
            };
            let group_names = anchored.group_names().to_vec().into_boxed_slice();
            return Ok(Self {
                strategy: Strategy::Prefix {
                    anchored,
                    prefilter: pred,
                    skip,
                },
                group_names,
            });
        }

        // No usable prefix. If the regex ends in a required literal, try the
        // reverse-automaton strategy (find the suffix, walk backwards to the
        // start, forward-verify). Falls back to a plain scan when that isn't
        // applicable (e.g. zero-width assertions defeat the tag-free reverse
        // DFA — see `reverse::reverse_nfa`).
        if let Some(suffix) = startpredicate::required_suffix_literal(re) {
            if let Some(program) = Self::try_reverse_inner(re, suffix)? {
                return Ok(program);
            }
        }

        let nfa = Nfa::try_from_unanchored(re)?;
        let mut unanchored = Tdfa::try_from(&nfa)?;
        unanchored.optimize();
        let group_names = unanchored.group_names().to_vec().into_boxed_slice();
        Ok(Self {
            strategy: Strategy::Scan { unanchored },
            group_names,
        })
    }

    /// Try to build a [`Strategy::ReverseInner`] for a regex ending in the
    /// required `suffix` literal. Returns `Ok(None)` (caller falls back to a
    /// scan) when the reverse automaton can't be built — currently when the
    /// pattern has zero-width assertions, which the tag-free reverse DFA can't
    /// honor, or when the reverse DFA exceeds its state budget. NFA/TDFA build
    /// failures propagate as `Err`.
    fn try_reverse_inner(re: &ir::Regex, suffix: Vec<u8>) -> Result<Option<Self>, BuildError> {
        let anchored_nfa = Nfa::try_from(re)?;
        let Some(reverse_nfa) = reverse::reverse_nfa(&anchored_nfa) else {
            return Ok(None);
        };
        let Ok(reverse) = Dfa::try_from(&reverse_nfa) else {
            return Ok(None);
        };
        let mut forward = Tdfa::try_from(&anchored_nfa)?;
        forward.optimize();
        let group_names = forward.group_names().to_vec().into_boxed_slice();
        let lit_len = suffix.len();
        let literal = Box::new(memmem::Finder::new(&suffix).into_owned());
        Ok(Some(Self {
            strategy: Strategy::ReverseInner {
                forward,
                reverse,
                literal,
                lit_len,
            },
            group_names,
        }))
    }

    /// Wrap an already-built (unanchored) TDFA as a plain linear-scan program,
    /// with no prefilter. Used by the micro-benchmarks that measure the
    /// automaton in isolation.
    pub fn scan(unanchored: Tdfa) -> Self {
        let group_names = unanchored.group_names().to_vec().into_boxed_slice();
        Self {
            strategy: Strategy::Scan { unanchored },
            group_names,
        }
    }

    /// Find the leftmost match at or after byte `offset`, returning the raw
    /// NFA-style match (range + captures), or `None`. `scratch` is the
    /// caller-owned (executor-owned) reusable mark buffer, so a `find_iter` over
    /// many matches allocates nothing per match. The executor adapter turns the
    /// result into a `Match`.
    pub(crate) fn find_at(
        &self,
        bytes: &[u8],
        offset: usize,
        scratch: &mut Scratch<u32>,
    ) -> Option<NfaMatch> {
        match &self.strategy {
            Strategy::Scan { unanchored } => {
                tdfa_backend::execute_reuse(unanchored, bytes, offset, scratch)
            }
            Strategy::Prefix {
                anchored,
                prefilter,
                skip,
            } => tdfa_backend::execute_prefiltered_reuse(
                anchored, bytes, offset, prefilter, scratch, *skip,
            ),
            Strategy::ReverseInner {
                forward,
                reverse,
                literal,
                lit_len,
            } => {
                let mut pos = offset;
                loop {
                    let i = literal.find(&bytes[pos..]).map(|k| pos + k)?;
                    let end = i + lit_len;
                    // Walk the reverse DFA back from the literal end to the
                    // leftmost start, then forward-verify there for the real
                    // extent + captures. The forward run is the source of truth
                    // (it fixes a greedy end and produces captures); the reverse
                    // only locates the start.
                    if let Some(s) = reverse::reverse_find_start(reverse, bytes, end, offset) {
                        if let Some(m) = tdfa_backend::execute_reuse(forward, bytes, s, scratch) {
                            return Some(m);
                        }
                    }
                    pos = i + 1;
                }
            }
        }
    }

    /// The mark-file width a reused [`Scratch`] for this program must have.
    pub(crate) fn mark_width(&self) -> usize {
        let tdfa = match &self.strategy {
            Strategy::Scan { unanchored } => unanchored,
            Strategy::Prefix { anchored, .. } => anchored,
            Strategy::ReverseInner { forward, .. } => forward,
        };
        tdfa_backend::mark_file_width(tdfa)
    }

    /// Capture-group names, indexed by group id (see `Tdfa::group_names`).
    pub fn group_names(&self) -> &[Box<str>] {
        &self.group_names
    }

    /// Test-only: whether this program uses the reverse-automaton strategy.
    #[cfg(test)]
    pub(crate) fn is_reverse_inner(&self) -> bool {
        matches!(self.strategy, Strategy::ReverseInner { .. })
    }

    /// Stats of the underlying automaton (for the benchmarks' size columns).
    pub fn stats(&self) -> TdfaStats {
        match &self.strategy {
            Strategy::Scan { unanchored } => unanchored.stats(),
            Strategy::Prefix { anchored, .. } => anchored.stats(),
            Strategy::ReverseInner { forward, .. } => forward.stats(),
        }
    }
}
