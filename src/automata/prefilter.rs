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

use crate::automata::casefold_search::CaseFoldSearcher;
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
use smallvec::SmallVec;

/// A built TDFA search program: an automaton plus the strategy used to drive it
/// over an input. This is the `Source` consumed by `TdfaExecutor`.
#[derive(Debug)]
pub struct TdfaProgram {
    strategy: Strategy,
    group_names: Box<[Box<str>]>,
    /// Optional native-code compilation of the strategy's anchored verify
    /// automaton (the capture-free tier). `None` for the plain interpreter
    /// program; populated by [`enable_jit`](Self::enable_jit) for the
    /// `tdfa-jit` backend, and left `None` when the automaton isn't JIT-able
    /// (the interpreter then runs). The compiled automaton corresponds to the
    /// strategy's single anchored verify automaton (`Prefix.anchored`,
    /// `CaseFoldLiteral.forward`, or `ReverseInner.forward`).
    #[cfg(feature = "tdfa-jit")]
    jit: Option<crate::automata::tdfa::jit::JittedTdfa>,
}

#[derive(Debug)]
enum Strategy {
    /// The whole regex is exactly a literal (no captures, tail, or assertions):
    /// the `memmem` span IS the match, so we skip the automaton entirely — just
    /// `memmem` + build the `Match`. This is the regex-crate pure-literal path.
    WholeLiteral {
        literal: Box<memmem::Finder<'static>>,
        len: usize,
    },
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
    /// Case-insensitive literal (e.g. `Sherlock`/i) — a chain of per-character
    /// case sets, so no fixed `ByteSequence` and the first set has a common byte.
    /// Search the longest *fold-clean* ASCII run (`herlock`) case-insensitively,
    /// then run the `forward` anchored TDFA over the small start window before it
    /// (the pre-run literal may be 1–2 bytes per problematic char, e.g. `S`/`ſ`),
    /// which verifies the full pattern incl. the width-changing ſ/Kelvin folds.
    CaseFoldLiteral {
        forward: Tdfa,
        searcher: CaseFoldSearcher,
        /// Min/max byte width of the literal portion before the clean run.
        prefix_lo: usize,
        prefix_hi: usize,
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

/// If the regex is *exactly* a literal byte sequence — no captures, no tail, no
/// anchors/assertions — return those bytes. Then the match is simply the
/// `memmem` span and no automaton is needed at all. The optimizer leaves such a
/// pattern as a `ByteSequence` (optionally inside a `Cat` with only trailing
/// zero-width `Goal`/`Empty` markers).
fn whole_literal(re: &ir::Regex) -> Option<Vec<u8>> {
    use ir::Node;
    fn lit(n: &Node) -> Option<Vec<u8>> {
        match n {
            Node::ByteSequence(b) if !b.is_empty() => Some(b.clone()),
            Node::Cat(nodes) => {
                let mut found: Option<Vec<u8>> = None;
                for node in nodes {
                    match node {
                        Node::Goal | Node::Empty => {}
                        Node::ByteSequence(b) if found.is_none() && !b.is_empty() => {
                            found = Some(b.clone());
                        }
                        // A second literal, or any consuming/grouping/anchoring
                        // node, means it isn't a bare whole literal.
                        _ => return None,
                    }
                }
                found
            }
            _ => None,
        }
    }
    lit(&re.node)
}

/// The fold-clean run extracted from a case-insensitive literal: the run's
/// per-position ASCII case-sets, plus the min/max byte width of the literal
/// portion *before* it.
struct CleanRunInfo {
    sets: Vec<SmallVec<[u8; 4]>>,
    prefix_lo: usize,
    prefix_hi: usize,
}

/// UTF-8 byte width of a codepoint.
fn cp_width(c: u32) -> usize {
    if c < 0x80 {
        1
    } else if c < 0x800 {
        2
    } else if c < 0x1_0000 {
        3
    } else {
        4
    }
}

/// If the regex is a pure **case-fold literal** — a `Cat` of per-character
/// byte/char sets with no loops/alternations/anchors/captures — return its
/// longest contiguous *fold-clean* ASCII run (length ≥ 2) and the byte-width
/// range of the literal before it. Returns `None` otherwise (→ other strategies
/// / `Scan`). A bare `ByteSequence` literal is handled by [`whole_literal`].
///
/// "Fold-clean" = the position's fold set is all single-byte ASCII. The
/// optimizer already encoded each position's fold set as the node itself
/// (`ByteSet`/coalesced `ByteSequence` = clean ASCII; `CharSet` = includes the
/// non-ASCII, width-changing `s`→ſ / `k`→Kelvin fold), so we read it straight
/// off the IR — conformant by construction with what the automaton matches.
fn casefold_clean_run(re: &ir::Regex) -> Option<CleanRunInfo> {
    use ir::Node;
    /// A clean ASCII case-set (width 1), or a width-`wmin..=wmax` problem char.
    enum Pos {
        Clean(SmallVec<[u8; 4]>),
        Problem { wmin: usize, wmax: usize },
    }

    let nodes: &[Node] = match &re.node {
        Node::Cat(n) => n,
        _ => return None,
    };
    let mut positions: Vec<Pos> = Vec::new();
    for n in nodes {
        match n {
            Node::Goal | Node::Empty => {}
            Node::ByteSet(bytes) => {
                if bytes.iter().any(|&b| b >= 0x80) {
                    return None;
                }
                positions.push(Pos::Clean(bytes.iter().copied().collect()));
            }
            Node::ByteSequence(bytes) => {
                for &b in bytes {
                    if b >= 0x80 {
                        return None; // non-ASCII literal char
                    }
                    positions.push(Pos::Clean(SmallVec::from_slice(&[b])));
                }
            }
            Node::CharSet(chars) => {
                let wmin = chars.iter().map(|&c| cp_width(c)).min()?;
                let wmax = chars.iter().map(|&c| cp_width(c)).max()?;
                positions.push(Pos::Problem { wmin, wmax });
            }
            Node::Char { c } if *c < 0x80 => {
                positions.push(Pos::Clean(SmallVec::from_slice(&[*c as u8])));
            }
            // Any consuming / structural node: not a pure literal.
            _ => return None,
        }
    }

    // Longest contiguous clean run.
    let (mut best_start, mut best_len) = (0usize, 0usize);
    let mut i = 0;
    while i < positions.len() {
        if matches!(positions[i], Pos::Clean(_)) {
            let start = i;
            while i < positions.len() && matches!(positions[i], Pos::Clean(_)) {
                i += 1;
            }
            if i - start > best_len {
                best_len = i - start;
                best_start = start;
            }
        } else {
            i += 1;
        }
    }
    if best_len < 2 {
        return None;
    }

    // Byte-width range of the literal before the run.
    let (mut lo, mut hi) = (0usize, 0usize);
    for p in &positions[..best_start] {
        match p {
            Pos::Clean(_) => {
                lo += 1;
                hi += 1;
            }
            Pos::Problem { wmin, wmax } => {
                lo += wmin;
                hi += wmax;
            }
        }
    }
    // Keep the per-hit verify window tiny.
    const MAX_PREFIX_SPREAD: usize = 4;
    if hi - lo > MAX_PREFIX_SPREAD {
        return None;
    }

    let sets = positions[best_start..best_start + best_len]
        .iter()
        .map(|p| match p {
            Pos::Clean(s) => s.clone(),
            Pos::Problem { .. } => unreachable!("run is all Clean"),
        })
        .collect();
    Some(CleanRunInfo {
        sets,
        prefix_lo: lo,
        prefix_hi: hi,
    })
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
    /// Assemble a program from its parts, leaving the optional JIT compilation
    /// off (the default interpreter program). The `tdfa-jit` backend enables it
    /// afterwards with [`enable_jit`](Self::enable_jit).
    fn from_parts(strategy: Strategy, group_names: Box<[Box<str>]>) -> Self {
        Self {
            strategy,
            group_names,
            #[cfg(feature = "tdfa-jit")]
            jit: None,
        }
    }

    pub fn try_from_ir(re: &ir::Regex) -> Result<Self, BuildError> {
        // Fastest path: a bare literal needs no automaton — the `memmem` span is
        // the whole match.
        if let Some(bytes) = whole_literal(re) {
            let len = bytes.len();
            let literal = Box::new(memmem::Finder::new(&bytes).into_owned());
            return Ok(Self::from_parts(
                Strategy::WholeLiteral { literal, len },
                Box::new([]),
            ));
        }

        // Case-insensitive literal (per-character case sets): search its longest
        // fold-clean ASCII run and verify the full pattern with the anchored
        // automaton. Tried before the start-predicate gate, which would reject
        // the common-first-byte icase predicate.
        if let Some(run) = casefold_clean_run(re) {
            if let Some(searcher) = CaseFoldSearcher::new(run.sets) {
                let nfa = Nfa::try_from(re)?;
                let mut forward = Tdfa::try_from(&nfa)?;
                forward.optimize();
                let group_names = forward.group_names().to_vec().into_boxed_slice();
                return Ok(Self::from_parts(
                    Strategy::CaseFoldLiteral {
                        forward,
                        searcher,
                        prefix_lo: run.prefix_lo,
                        prefix_hi: run.prefix_hi,
                    },
                    group_names,
                ));
            }
        }

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
            return Ok(Self::from_parts(
                Strategy::Prefix {
                    anchored,
                    prefilter: pred,
                    skip,
                },
                group_names,
            ));
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
        Ok(Self::from_parts(
            Strategy::Scan { unanchored },
            group_names,
        ))
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
        Ok(Some(Self::from_parts(
            Strategy::ReverseInner {
                forward,
                reverse,
                literal,
                lit_len,
            },
            group_names,
        )))
    }

    /// Wrap an already-built (unanchored) TDFA as a plain linear-scan program,
    /// with no prefilter. Used by the micro-benchmarks that measure the
    /// automaton in isolation.
    pub fn scan(unanchored: Tdfa) -> Self {
        let group_names = unanchored.group_names().to_vec().into_boxed_slice();
        Self::from_parts(Strategy::Scan { unanchored }, group_names)
    }

    /// Like [`try_from_ir`](Self::try_from_ir), but additionally JIT-compiles
    /// the strategy's anchored verify automaton when possible (the `tdfa-jit`
    /// backend). Falls back to the interpreter for any automaton the JIT can't
    /// yet handle, so this never fails for a JIT reason.
    #[cfg(feature = "tdfa-jit")]
    pub fn try_from_ir_jit(re: &ir::Regex) -> Result<Self, BuildError> {
        let mut prog = Self::try_from_ir(re)?;
        prog.enable_jit();
        Ok(prog)
    }

    /// Compile the strategy's single anchored verify automaton to native code,
    /// storing it in `self.jit`. A no-op (leaving the interpreter path) when the
    /// strategy has no such automaton or it isn't JIT-able.
    #[cfg(feature = "tdfa-jit")]
    fn enable_jit(&mut self) {
        use crate::automata::tdfa::jit::JittedTdfa;
        let anchored = match &self.strategy {
            Strategy::Prefix { anchored, .. } => Some(anchored),
            Strategy::CaseFoldLiteral { forward, .. } => Some(forward),
            Strategy::ReverseInner { forward, .. } => Some(forward),
            // Whole-literal needs no automaton; the unanchored Scan isn't the
            // capture-free tier the JIT compiles.
            Strategy::WholeLiteral { .. } | Strategy::Scan { .. } => None,
        };
        if let Some(tdfa) = anchored {
            self.jit = JittedTdfa::compile(tdfa).ok();
        }
    }

    /// Anchored verify at `s`: native code when this program was JIT-compiled,
    /// else the interpreter. `tdfa` is always the strategy's anchored verify
    /// automaton — the same one `enable_jit` compiled — so the two paths agree.
    #[inline]
    fn verify_at(
        &self,
        tdfa: &Tdfa,
        bytes: &[u8],
        s: usize,
        scratch: &mut Scratch<u32>,
    ) -> Option<NfaMatch> {
        #[cfg(feature = "tdfa-jit")]
        if let Some(jit) = &self.jit {
            return jit.run(tdfa, bytes, s, scratch);
        }
        tdfa_backend::execute_reuse(tdfa, bytes, s, scratch)
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
            Strategy::WholeLiteral { literal, len } => {
                let i = literal.find(&bytes[offset..]).map(|k| offset + k)?;
                // The literal is the entire match: no captures, no automaton.
                Some(NfaMatch {
                    range: i..i + len,
                    captures: Vec::new(),
                })
            }
            Strategy::CaseFoldLiteral {
                forward,
                searcher,
                prefix_lo,
                prefix_hi,
            } => {
                let mut pos = offset;
                loop {
                    let j = searcher.find(bytes, pos)?; // clean-run start
                    // Candidate match starts: s = j - w for each possible
                    // pre-run byte width w ∈ [prefix_lo, prefix_hi], i.e.
                    // s ∈ [j - prefix_hi, j - prefix_lo], clamped to ≥ offset.
                    // Try ascending (leftmost s first); the automaton verifies the
                    // full pattern, folding the width-changing ſ/Kelvin correctly.
                    if let Some(s_hi) = j.checked_sub(*prefix_lo) {
                        let s_lo = j.saturating_sub(*prefix_hi).max(offset);
                        let mut s = s_lo;
                        while s <= s_hi {
                            if let Some(m) = self.verify_at(forward, bytes, s, scratch) {
                                return Some(m);
                            }
                            s += 1;
                        }
                    }
                    pos = j + 1;
                }
            }
            Strategy::Scan { unanchored } => {
                tdfa_backend::execute_reuse(unanchored, bytes, offset, scratch)
            }
            Strategy::Prefix {
                anchored,
                prefilter,
                skip,
            } => {
                // JIT path: verify each prefilter candidate with native code.
                // (The warm-start `skip` is an interpreter-only optimization, so
                // the JIT re-scans the literal prefix — still correct.)
                #[cfg(feature = "tdfa-jit")]
                if let Some(jit) = &self.jit {
                    let mut pos = offset;
                    loop {
                        let cand = prefilter.find_from(bytes, pos)?;
                        if let Some(m) = jit.run(anchored, bytes, cand, scratch) {
                            return Some(m);
                        }
                        pos = cand + 1;
                    }
                }
                tdfa_backend::execute_prefiltered_reuse(
                    anchored, bytes, offset, prefilter, scratch, *skip,
                )
            }
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
                        if let Some(m) = self.verify_at(forward, bytes, s, scratch) {
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
            // No automaton; the executor's scratch is never touched. Any
            // non-zero width is fine.
            Strategy::WholeLiteral { .. } => return 3,
            Strategy::Scan { unanchored } => unanchored,
            Strategy::Prefix { anchored, .. } => anchored,
            Strategy::CaseFoldLiteral { forward, .. } => forward,
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

    /// Test-only: whether this program uses the case-fold-literal strategy.
    #[cfg(test)]
    pub(crate) fn is_casefold_literal(&self) -> bool {
        matches!(self.strategy, Strategy::CaseFoldLiteral { .. })
    }

    /// Stats of the underlying automaton (for the benchmarks' size columns).
    pub fn stats(&self) -> TdfaStats {
        match &self.strategy {
            // No automaton was built.
            Strategy::WholeLiteral { .. } => TdfaStats {
                num_states: 0,
                num_marks: 0,
                total_commands: 0,
                copy_commands: 0,
                currentpos_commands: 0,
            },
            Strategy::Scan { unanchored } => unanchored.stats(),
            Strategy::Prefix { anchored, .. } => anchored.stats(),
            Strategy::CaseFoldLiteral { forward, .. } => forward.stats(),
            Strategy::ReverseInner { forward, .. } => forward.stats(),
        }
    }

    /// Whether the strategy's anchored verify automaton was successfully
    /// JIT-compiled (so `find_at` runs native code). `false` means the
    /// interpreter is used — either because the JIT wasn't enabled, or because
    /// the automaton isn't in the supported tier.
    #[cfg(feature = "tdfa-jit")]
    pub fn jit_active(&self) -> bool {
        self.jit.is_some()
    }
}

/// The `tdfa-jit` backend's program: a [`TdfaProgram`] with its anchored verify
/// automaton JIT-compiled to native code where possible. A distinct type so it
/// is selected explicitly (its own `Executor`, its own `--exec tdfa-jit` name,
/// its own benchmark column) — but it reuses all of `TdfaProgram`'s strategy
/// selection and prefilter machinery. Falls back to the interpreter for any
/// automaton outside the supported JIT tier, so it matches `TdfaProgram`'s
/// results exactly. This is the `Source` consumed by `TdfaJitExecutor`.
#[cfg(feature = "tdfa-jit")]
#[derive(Debug)]
pub struct TdfaJitProgram(TdfaProgram);

#[cfg(feature = "tdfa-jit")]
impl TdfaJitProgram {
    /// Build from IR, JIT-compiling where possible. Errors only for the same
    /// automaton-build reasons as [`TdfaProgram::try_from_ir`] (never for a JIT
    /// reason — an un-JIT-able automaton just keeps the interpreter).
    pub fn try_from_ir(re: &ir::Regex) -> Result<Self, BuildError> {
        Ok(Self(TdfaProgram::try_from_ir_jit(re)?))
    }

    /// See [`TdfaProgram::find_at`].
    pub(crate) fn find_at(
        &self,
        bytes: &[u8],
        offset: usize,
        scratch: &mut Scratch<u32>,
    ) -> Option<NfaMatch> {
        self.0.find_at(bytes, offset, scratch)
    }

    /// See [`TdfaProgram::mark_width`].
    pub(crate) fn mark_width(&self) -> usize {
        self.0.mark_width()
    }

    /// See [`TdfaProgram::group_names`].
    pub fn group_names(&self) -> &[Box<str>] {
        self.0.group_names()
    }

    /// Whether native code is actually in use (vs. interpreter fallback).
    pub fn jit_active(&self) -> bool {
        self.0.jit_active()
    }

    /// Stats of the underlying automaton (for the benchmarks' size columns).
    pub fn stats(&self) -> TdfaStats {
        self.0.stats()
    }
}
