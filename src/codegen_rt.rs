//! Runtime support for ahead-of-time compiled matchers (`regress::__codegen`).
//!
//! The `regress-macro` crate's `regex!` proc-macro runs the TDFA pipeline at
//! macro-expansion time and emits Rust source (see `automata::tdfa::rustgen`).
//! The generated code is a *verifier* — an unrolled anchored automaton — plus
//! serialized prefilter data; everything that drives a search over an input
//! lives here: the prefilter candidate loop (a transcription of
//! `TdfaProgram::find_at`), the match iterator (same semantics as
//! `executors::next_match_single_pass`, including the one-codepoint bump after
//! an empty match), and `Match` construction.
//!
//! This module is `#[doc(hidden)]` and semver-exempt: generated code and the
//! `regress` crate it links against must be the same version (`regress-macro`
//! pins it exactly).

use crate::api::Match;
use crate::automata::casefold_search::CaseFoldSearcher;
use crate::bytesearch::{ByteBitmap, ByteSearcher};
use memchr::memmem;
use std::sync::OnceLock;

/// The generated verify function: run the anchored automaton over `input`
/// starting at `start`, returning the match extent `(start, end)` and filling
/// `caps` (the `2 * num_groups` capture buffer, `usize::MAX` = group unset).
/// `caps` is written only on success.
pub type VerifyFn = fn(input: &[u8], start: usize, caps: &mut [usize]) -> Option<(usize, usize)>;

/// Lazily built `memmem::Finder` for a serialized literal. Const-constructible
/// so a `PrefilterSpec` can live in a `static`; the finder's prefilter tables
/// are built once on first search.
pub struct FinderCache(OnceLock<memmem::Finder<'static>>);

impl FinderCache {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn get(&self, bytes: &'static [u8]) -> &memmem::Finder<'static> {
        self.0.get_or_init(|| memmem::Finder::new(bytes))
    }
}

impl core::fmt::Debug for FinderCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("FinderCache")
    }
}

/// Serialized [`StartPredicate`](crate::insn::StartPredicate): the same
/// searchable start constraints, reconstructed from plain bytes so the emitter
/// can bake them into a `static`.
#[derive(Debug)]
pub enum PredicateSpec {
    ByteSet1([u8; 1]),
    ByteSet2([u8; 2]),
    ByteSet3([u8; 3]),
    ByteSeq {
        bytes: &'static [u8],
        finder: FinderCache,
    },
    ByteBracket([u16; 16]),
}

impl PredicateSpec {
    /// Find the next byte offset `>= from` in `input` where this predicate
    /// could start a match. Mirrors `StartPredicate::find_from`.
    fn find_from(&self, input: &[u8], from: usize) -> Option<usize> {
        if from > input.len() {
            return None;
        }
        let hay = &input[from..];
        let idx = match self {
            PredicateSpec::ByteSet1([a]) => memchr::memchr(*a, hay),
            PredicateSpec::ByteSet2([a, b]) => memchr::memchr2(*a, *b, hay),
            PredicateSpec::ByteSet3([a, b, c]) => memchr::memchr3(*a, *b, *c, hay),
            PredicateSpec::ByteSeq { bytes, finder } => finder.get(bytes).find(hay),
            PredicateSpec::ByteBracket(bits) => ByteBitmap::from_raw(*bits).find_in(hay),
        };
        idx.map(|i| from + i)
    }
}

/// Serialized `prefilter::LitWindow`: a required literal byte that must occur
/// within `[lo, hi]` bytes of any match start.
#[derive(Debug, Clone, Copy)]
pub struct LitWindowSpec {
    pub byte: u8,
    pub lo: usize,
    pub hi: usize,
}

impl LitWindowSpec {
    /// Mirrors `LitWindow::admits`: reject a candidate only when the byte is
    /// provably absent from its window.
    #[inline(always)]
    fn admits(&self, bytes: &[u8], start: usize) -> bool {
        let from = start + self.lo;
        if from >= bytes.len() {
            return false;
        }
        let to = (start + self.hi + 1).min(bytes.len());
        bytes[from..to].contains(&self.byte)
    }
}

/// Lazily built multi-substring searcher over a serialized needle set:
/// aho-corasick's packed (Teddy) searcher where the target supports it, else
/// a plain leftmost-first Aho-Corasick automaton (host and target CPU
/// features may differ; the fallback keeps semantics identical).
pub struct MultiCache(OnceLock<MultiSearcher>);

enum MultiSearcher {
    Teddy(aho_corasick::packed::Searcher),
    Ac(aho_corasick::AhoCorasick),
}

impl MultiCache {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn get(&self, needles: &[&[u8]]) -> &MultiSearcher {
        self.0.get_or_init(|| {
            let owned: Vec<Vec<u8>> = needles.iter().map(|n| n.to_vec()).collect();
            match crate::automata::prefilter::build_teddy(&owned) {
                Some(t) => MultiSearcher::Teddy(t),
                None => MultiSearcher::Ac(
                    aho_corasick::AhoCorasick::builder()
                        .match_kind(aho_corasick::MatchKind::LeftmostFirst)
                        .build(&owned)
                        .expect("aho-corasick build"),
                ),
            }
        })
    }

    /// Leftmost-first search at or after `from`; returns the match span.
    fn find_from(&self, needles: &[&[u8]], bytes: &[u8], from: usize) -> Option<(usize, usize)> {
        if from > bytes.len() {
            return None;
        }
        match self.get(needles) {
            MultiSearcher::Teddy(t) => t
                .find_in(bytes, aho_corasick::Span::from(from..bytes.len()))
                .map(|m| (m.start(), m.end())),
            MultiSearcher::Ac(ac) => ac
                .find(&bytes[from..])
                .map(|m| (from + m.start(), from + m.end())),
        }
    }
}

impl core::fmt::Debug for MultiCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MultiCache")
    }
}

/// Lazily built case-insensitive literal scanner: Teddy over the serialized
/// leading case cross-product when available, else the `CaseFoldSearcher`
/// packed-pair scan rebuilt from the serialized per-character case sets.
pub struct CaseFoldCache(OnceLock<CaseFoldEngine>);

enum CaseFoldEngine {
    Teddy(aho_corasick::packed::Searcher),
    Fold(CaseFoldSearcher),
}

impl CaseFoldCache {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }
}

impl core::fmt::Debug for CaseFoldCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CaseFoldCache")
    }
}

/// Serialized reverse DFA tables for the `ReverseInner` strategy. The walk
/// itself mirrors `reverse::reverse_find_start`.
#[derive(Debug)]
pub struct ReverseDfaSpec {
    pub start: u32,
    pub num_classes: usize,
    pub byte_to_class: &'static [u8; 256],
    pub transitions: &'static [u32],
    pub accepting: &'static [bool],
}

impl ReverseDfaSpec {
    /// Walk right-to-left from `end`, returning the leftmost accepting start
    /// `>= min_start`. A transcription of `reverse::reverse_find_start`
    /// (state 0 is the dead state).
    fn find_start(&self, input: &[u8], end: usize, min_start: usize) -> Option<usize> {
        const DEAD: u32 = 0;
        let mut state = self.start;
        let mut best = if self.accepting[state as usize] {
            Some(end)
        } else {
            None
        };
        let mut pos = end;
        while pos > min_start && state != DEAD {
            let class = self.byte_to_class[input[pos - 1] as usize] as usize;
            state = self.transitions[state as usize * self.num_classes + class];
            pos -= 1;
            if state == DEAD {
                break;
            }
            if self.accepting[state as usize] {
                best = Some(pos);
            }
        }
        best
    }
}

/// Serialized search [`Strategy`](crate::automata::prefilter): how the driver
/// locates candidate positions before calling the generated verify function.
/// Drivers are transcriptions of the corresponding `TdfaProgram::find_at`
/// arms.
#[derive(Debug)]
pub enum PrefilterSpec {
    /// No usable literal: one pass of the unanchored verify automaton.
    Scan,
    /// Prefix literal / byte set: skip to candidates, anchored verify at each.
    /// Any warm-start prefix skip is baked into the generated verify function.
    Prefix {
        predicate: PredicateSpec,
        lit_window: Option<LitWindowSpec>,
    },
    /// The whole regex is exactly a literal: the `memmem` span IS the match,
    /// no automaton at all (the verify fn is an unused stub).
    WholeLiteral {
        bytes: &'static [u8],
        finder: FinderCache,
    },
    /// The whole regex is a literal alternation: the matched needle's span IS
    /// the match (leftmost-first), no automaton.
    MultiLiteral {
        needles: &'static [&'static [u8]],
        searcher: MultiCache,
    },
    /// Case-insensitive literal: scan for candidates (Teddy over the leading
    /// case cross-product, else the fold-clean-run scan), anchored verify.
    CaseFoldLiteral {
        /// Per-character ASCII case sets of the fold-clean run.
        sets: &'static [&'static [u8]],
        /// Byte-width range of the literal portion before the clean run.
        prefix_lo: usize,
        prefix_hi: usize,
        /// Leading case cross-product for Teddy; `None` when the host built
        /// none (the fold scan is then the only engine).
        teddy_needles: Option<&'static [&'static [u8]]>,
        cache: CaseFoldCache,
    },
    /// Alternation whose every branch has a literal prefix: Teddy over the
    /// union of branch prefixes locates candidates, anchored verify.
    AltPrefix {
        needles: &'static [&'static [u8]],
        searcher: MultiCache,
    },
    /// Required interior/suffix literal with no usable prefix: `memmem` the
    /// literal, reverse-DFA walk back to the leftmost start, forward verify.
    ReverseInner {
        literal: &'static [u8],
        finder: FinderCache,
        dfa: ReverseDfaSpec,
    },
}

/// An ahead-of-time compiled matcher: the value a `regex!(...)` expansion
/// evaluates to. Const-constructible so it can live in a `static`.
///
/// Methods mirror [`Regex`](crate::Regex): [`find`](Self::find),
/// [`find_iter`](Self::find_iter), and [`find_from`](Self::find_from) produce
/// the same [`Match`] values (byte ranges, captures, named groups) the
/// TDFA executor produces.
#[derive(Debug)]
pub struct CompiledMatcher {
    prefilter: &'static PrefilterSpec,
    verify: VerifyFn,
    num_groups: usize,
    group_names: &'static [&'static str],
    tier: MatcherTier,
}

/// Which code shape `emit_expansion` chose for a pattern, mirroring its
/// three-way branch. Not meaningful for correctness (all tiers agree on
/// matches by construction) — purely an introspection aid, e.g. for
/// benchmarks that want to report which shape ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatcherTier {
    /// No verify automaton at all: the prefilter span IS the match
    /// (`Strategy::WholeLiteral` / `MultiLiteral`).
    Literal,
    /// Fully unrolled `match`/`while` control flow (states/peels/moves as
    /// Rust source) — the default tier below `CODEGEN_UNROLL_MAX_STATES`.
    Unrolled,
    /// The automaton emitted as `static` data driven by the shared
    /// interpreter loop (`tdfa_backend::execute_reuse_warm`) — used past the
    /// unrolled-tier state threshold.
    Table,
}

impl CompiledMatcher {
    pub const fn from_parts(
        prefilter: &'static PrefilterSpec,
        verify: VerifyFn,
        num_groups: usize,
        group_names: &'static [&'static str],
        tier: MatcherTier,
    ) -> Self {
        Self {
            prefilter,
            verify,
            num_groups,
            group_names,
            tier,
        }
    }

    /// Which code shape this matcher was emitted as. See [`MatcherTier`].
    pub const fn tier(&self) -> MatcherTier {
        self.tier
    }

    /// Searches `text` to find the first match.
    pub fn find(&self, text: &str) -> Option<Match> {
        self.find_iter(text).next()
    }

    /// Like [`find_iter`](Self::find_iter), but each match borrows its
    /// captures from a buffer reused across the whole scan instead of
    /// allocating a [`Match`] (`Vec` of captures, plus re-cloning
    /// `group_names`) per match. Mirrors
    /// [`TdfaMatches`](crate::automata::tdfa_backend)'s relationship to the
    /// `Match`-producing executors — use this when profiling/benchmarking
    /// match throughput in isolation from `Match` construction cost, which
    /// otherwise dominates on capture-heavy patterns.
    pub fn find_iter_raw<'r, 't>(&'r self, text: &'t str) -> RawMatches<'r, 't> {
        self.find_from_raw(text, 0)
    }

    /// Like [`find_iter_raw`](Self::find_iter_raw), starting at byte offset `start`.
    pub fn find_from_raw<'r, 't>(&'r self, text: &'t str, start: usize) -> RawMatches<'r, 't> {
        RawMatches {
            matcher: self,
            text,
            caps: vec![usize::MAX; 2 * self.num_groups],
            position: (start <= text.len()).then_some(start),
        }
    }

    /// Returns an iterator over the matches in `text`.
    pub fn find_iter<'r, 't>(&'r self, text: &'t str) -> CompiledMatches<'r, 't> {
        self.find_from(text, 0)
    }

    /// Returns an iterator over the matches in `text`, starting at byte
    /// offset `start`.
    pub fn find_from<'r, 't>(&'r self, text: &'t str, start: usize) -> CompiledMatches<'r, 't> {
        CompiledMatches {
            matcher: self,
            text,
            caps: vec![usize::MAX; 2 * self.num_groups],
            position: (start <= text.len()).then_some(start),
        }
    }

    /// Find the leftmost match at or after `offset`: drive the prefilter to
    /// candidate positions and verify at each. A transcription of
    /// `TdfaProgram::find_at` with the interpreter replaced by the generated
    /// verify function.
    fn find_at(&self, bytes: &[u8], offset: usize, caps: &mut [usize]) -> Option<(usize, usize)> {
        match self.prefilter {
            PrefilterSpec::Scan => (self.verify)(bytes, offset, caps),
            PrefilterSpec::Prefix {
                predicate,
                lit_window,
            } => {
                let mut pos = offset;
                loop {
                    let cand = predicate.find_from(bytes, pos)?;
                    // Cheap secondary filter: skip the verify call when the
                    // required interior literal can't be in range.
                    if lit_window.is_none_or(|w| w.admits(bytes, cand)) {
                        if let Some(m) = (self.verify)(bytes, cand, caps) {
                            return Some(m);
                        }
                    }
                    pos = cand + 1;
                }
            }
            PrefilterSpec::WholeLiteral { bytes: lit, finder } => {
                if offset > bytes.len() {
                    return None;
                }
                let i = finder.get(lit).find(&bytes[offset..]).map(|k| offset + k)?;
                Some((i, i + lit.len()))
            }
            PrefilterSpec::MultiLiteral { needles, searcher } => {
                searcher.find_from(needles, bytes, offset)
            }
            PrefilterSpec::AltPrefix { needles, searcher } => {
                // Each hit is a branch prefix start; verify the full branch.
                let mut pos = offset;
                loop {
                    let (cand, _) = searcher.find_from(needles, bytes, pos)?;
                    if let Some(m) = (self.verify)(bytes, cand, caps) {
                        return Some(m);
                    }
                    pos = cand + 1;
                }
            }
            PrefilterSpec::CaseFoldLiteral {
                sets,
                prefix_lo,
                prefix_hi,
                teddy_needles,
                cache,
            } => {
                let engine = cache.0.get_or_init(|| {
                    if let Some(needles) = teddy_needles {
                        let owned: Vec<Vec<u8>> = needles.iter().map(|n| n.to_vec()).collect();
                        if let Some(t) = crate::automata::prefilter::build_teddy(&owned) {
                            return CaseFoldEngine::Teddy(t);
                        }
                    }
                    let sets = sets.iter().map(|s| s.iter().copied().collect()).collect();
                    CaseFoldEngine::Fold(
                        CaseFoldSearcher::new(sets)
                            .expect("serialized case-fold sets rebuild the searcher"),
                    )
                });
                match engine {
                    // Teddy hits are concrete literal starts: verify there.
                    CaseFoldEngine::Teddy(t) => {
                        let mut pos = offset;
                        loop {
                            if pos > bytes.len() {
                                return None;
                            }
                            let m =
                                t.find_in(bytes, aho_corasick::Span::from(pos..bytes.len()))?;
                            if let Some(found) = (self.verify)(bytes, m.start(), caps) {
                                return Some(found);
                            }
                            pos = m.start() + 1;
                        }
                    }
                    // Fold-scan hits are clean-run starts: try each candidate
                    // match start in the pre-run byte-width window, ascending.
                    CaseFoldEngine::Fold(searcher) => {
                        let mut pos = offset;
                        loop {
                            let j = searcher.find(bytes, pos)?;
                            if let Some(s_hi) = j.checked_sub(*prefix_lo) {
                                let s_lo = j.saturating_sub(*prefix_hi).max(offset);
                                let mut s = s_lo;
                                while s <= s_hi {
                                    if let Some(m) = (self.verify)(bytes, s, caps) {
                                        return Some(m);
                                    }
                                    s += 1;
                                }
                            }
                            pos = j + 1;
                        }
                    }
                }
            }
            PrefilterSpec::ReverseInner { literal, finder, dfa } => {
                let mut pos = offset;
                loop {
                    if pos > bytes.len() {
                        return None;
                    }
                    let i = finder.get(literal).find(&bytes[pos..]).map(|k| pos + k)?;
                    // Walk the reversed prefix back to the leftmost start,
                    // then forward-verify there for the real extent+captures.
                    if let Some(s) = dfa.find_start(bytes, i, offset) {
                        if let Some(m) = (self.verify)(bytes, s, caps) {
                            return Some(m);
                        }
                    }
                    pos = i + 1;
                }
            }
        }
    }
}

/// Iterator over the matches of a [`CompiledMatcher`]. Reuses one capture
/// buffer across matches (zero allocation per match beyond the `Match` itself).
#[derive(Debug)]
pub struct CompiledMatches<'r, 't> {
    matcher: &'r CompiledMatcher,
    text: &'t str,
    caps: Vec<usize>,
    /// Next search offset; `None` when exhausted.
    position: Option<usize>,
}

impl Iterator for CompiledMatches<'_, '_> {
    type Item = Match;

    fn next(&mut self) -> Option<Match> {
        let offset = self.position?;
        let bytes = self.text.as_bytes();
        let (start, end) = self.matcher.find_at(bytes, offset, &mut self.caps)?;
        // Advance past the match; a zero-width match bumps one codepoint to
        // make forward progress (same as `next_match_single_pass`).
        self.position = if end == start {
            self.text[end..].chars().next().map(|c| end + c.len_utf8())
        } else {
            Some(end)
        };
        Some(make_match(start, end, &self.caps, self.matcher.group_names))
    }
}

/// Build a [`Match`] from a verify result, applying the TDFA executor's
/// capture conversion: an unset pair (`usize::MAX` start) becomes `None`.
fn make_match(
    start: usize,
    end: usize,
    caps: &[usize],
    group_names: &'static [&'static str],
) -> Match {
    Match {
        range: start..end,
        captures: caps
            .chunks_exact(2)
            .map(|c| if c[0] == usize::MAX { None } else { Some(c[0]..c[1]) })
            .collect(),
        group_names: group_names.iter().map(|&s| Box::from(s)).collect(),
    }
}

/// A match produced by [`CompiledMatcher::find_iter_raw`], borrowing its
/// captures from the iterator's reused buffer. Zero allocation per match —
/// mirrors [`TdfaMatch`](crate::automata::tdfa_backend::TdfaMatch). Unlike
/// [`Match`], there is no `group_names` here (named-group lookup needs an
/// owned `Match`; use [`CompiledMatcher::find_iter`] for that).
#[derive(Debug)]
pub struct CompiledMatch<'a> {
    /// The full match range.
    pub range: crate::api::Range,
    captures: &'a [usize],
}

impl CompiledMatch<'_> {
    /// Number of capture groups (not counting the full match).
    pub fn num_captures(&self) -> usize {
        self.captures.len() / 2
    }

    /// Capture group `i` (0-indexed). `None` if the group did not participate.
    pub fn capture(&self, i: usize) -> Option<crate::api::Range> {
        let s = self.captures[2 * i];
        if s == usize::MAX {
            None
        } else {
            Some(s..self.captures[2 * i + 1])
        }
    }
}

/// A lending iterator over [`CompiledMatch`]es (see
/// [`CompiledMatcher::find_iter_raw`]). Each match borrows from `self`: drop
/// it before calling [`next`](Self::next) again.
#[derive(Debug)]
pub struct RawMatches<'r, 't> {
    matcher: &'r CompiledMatcher,
    text: &'t str,
    caps: Vec<usize>,
    position: Option<usize>,
}

impl RawMatches<'_, '_> {
    /// Advance to the next match. Returns `None` when exhausted.
    pub fn next(&mut self) -> Option<CompiledMatch<'_>> {
        let offset = self.position?;
        let bytes = self.text.as_bytes();
        let (start, end) = self.matcher.find_at(bytes, offset, &mut self.caps)?;
        self.position = if end == start {
            self.text[end..].chars().next().map(|c| end + c.len_utf8())
        } else {
            Some(end)
        };
        Some(CompiledMatch { range: start..end, captures: &self.caps })
    }
}

// ---------------------------------------------------------------------------
// Table tier: static automaton tables + the shared interpreter loop.
// ---------------------------------------------------------------------------

pub use crate::automata::tdfa::{
    ACCEL_NONE, FinalCommand, InputMark, MarkValue, MoveOp, PosStampLoopFlat, ScanFast,
    ScanSkipFlat,
};
pub use crate::automata::tdfa_backend::PrefixSkip;
use crate::automata::tdfa::{StateGuards, TagCommand};
use crate::automata::tdfa_backend::{self, Scratch, TdfaTables};

/// Advance `pos` through `input` while bytes stay in the self-loop set
/// described by `fast`/`byte_bitmap` — the interpreter's accelerated scan
/// (SIMD range masks, `memchr`, etc. depending on what the set classifies
/// as), exposed for the unrolled tier's peeled self-loops. A `regex!`
/// expansion's peeled state calls this instead of a from-scratch scalar
/// byte loop, so it gets the same acceleration the table tier and
/// interpreter already share via `tdfa_backend::scan_fast`.
pub fn scan_fast(fast: &ScanFast, byte_bitmap: &[u64; 4], input: &[u8], pos: usize) -> usize {
    tdfa_backend::scan_fast(fast, byte_bitmap, input, pos)
}

/// The table tier's automaton: every table the executor reads, borrowed from
/// `static` data the `regex!` expansion carries. The emitter guarantees the
/// same invariants a built `Tdfa` upholds (shapes, premultiplication, CSR
/// validity) plus the tier's restrictions: compiled moves present, no
/// zero-width guards of any kind (`has_perbyte_guards`/`has_eoi_accepts`
/// would be false — such patterns are rejected at expansion time).
#[derive(Debug)]
pub struct StaticTdfa {
    pub num_classes: usize,
    pub num_marks: usize,
    pub num_states: usize,
    pub num_capture_groups: usize,
    pub has_captures: bool,
    pub start_fixed: bool,
    pub start_anchored: u32,
    pub start_unanchored: u32,
    pub byte_to_class: &'static [u8; 256],
    pub transitions: &'static [u32],
    pub trans_flags: &'static [u8],
    pub exec_transitions: &'static [u32],
    pub accepting: &'static [bool],
    pub accept_fallback: &'static [bool],
    pub mv_cells: &'static [u32],
    pub mv_arena: &'static [MoveOp],
    pub entry_moves_anchored: &'static [MoveOp],
    pub entry_moves_unanchored: &'static [MoveOp],
    pub finals_cells: &'static [u32],
    /// Flat `(tag, mark)` `u32` pairs — `FinalCommand::src` is always
    /// `MarkValue::Copy` in a finals list (never `CurrentPos`, matching the
    /// interpreter's own `finalize` invariant), so this is a lossless raw
    /// form. Decoded once into `finals_cache` on first use rather than a
    /// literal `[FinalCommand; N]`: `FinalCommand` holds an enum, whose
    /// layout isn't guaranteed the way a plain-data blob cast needs.
    pub finals_raw: &'static [u32],
    pub finals_cache: &'static OnceLock<Vec<FinalCommand>>,
    pub psl_index: &'static [u32],
    pub psl_table: &'static [PosStampLoopFlat],
    pub scan_skip_index: &'static [u32],
    pub scan_skip_table: &'static [ScanSkipFlat],
    pub stamp_arena: &'static [u16],
    pub psl_ascii_bms: &'static [u64],
    pub prefix_skip: Option<PrefixSkip>,
}

impl TdfaTables for StaticTdfa {
    fn num_classes(&self) -> usize { self.num_classes }
    fn num_marks(&self) -> usize { self.num_marks }
    fn num_states(&self) -> usize { self.num_states }
    fn has_captures(&self) -> bool { self.has_captures }
    fn has_moves(&self) -> bool { true }
    fn has_perbyte_guards(&self) -> bool { false }
    fn has_eoi_accepts(&self) -> bool { false }
    fn word_icase(&self) -> bool { false }
    fn start_fixed(&self) -> bool { self.start_fixed }
    fn start(&self, start: usize) -> u32 {
        if start == 0 { self.start_anchored } else { self.start_unanchored }
    }
    fn byte_to_class(&self) -> &[u8; 256] { self.byte_to_class }
    fn transitions(&self) -> &[u32] { self.transitions }
    fn trans_flags(&self) -> &[u8] { self.trans_flags }
    fn exec_transitions(&self) -> &[u32] { self.exec_transitions }
    fn accepting(&self) -> &[bool] { self.accepting }
    fn accept_fallback(&self) -> &[bool] { self.accept_fallback }
    fn moves_raw(&self) -> (&[u32], &[MoveOp]) { (self.mv_cells, self.mv_arena) }
    fn transition_commands(&self, _idx: usize) -> &[TagCommand] { &[] }
    fn entry_moves(&self, start: usize) -> &[MoveOp] {
        if start == 0 { self.entry_moves_anchored } else { self.entry_moves_unanchored }
    }
    fn entry_commands(&self, _start: usize) -> &[TagCommand] { &[] }
    fn finals(&self, state: u32) -> &[FinalCommand] {
        let arena = self.finals_cache.get_or_init(|| {
            self.finals_raw
                .chunks_exact(2)
                .map(|c| FinalCommand { tag: c[0], src: MarkValue::Copy(InputMark(c[1])) })
                .collect()
        });
        crate::automata::tdfa::csr_iat(self.finals_cells, arena, state as usize)
    }
    fn guards(&self, _state: u32) -> Option<&StateGuards> { None }
    fn psl_tables(&self) -> (&[u32], &[PosStampLoopFlat]) { (self.psl_index, self.psl_table) }
    fn scan_skip_tables(&self) -> (&[u32], &[ScanSkipFlat]) {
        (self.scan_skip_index, self.scan_skip_table)
    }
    fn stamp_arena(&self) -> &[u16] { self.stamp_arena }
    fn psl_ascii_bms(&self) -> &[u64] { self.psl_ascii_bms }
}

/// Drive the shared interpreter loop over static tables — the table tier's
/// verify function (same contract as [`VerifyFn`]). The per-search scratch is
/// thread-local and reused across calls; it is rebuilt only when a
/// differently-sized automaton last used this thread (interleaving two table
/// matchers on one thread re-sizes per switch — acceptable churn for keeping
/// the verify signature a plain fn pointer).
pub fn table_verify(
    t: &StaticTdfa,
    input: &[u8],
    start: usize,
    caps: &mut [usize],
) -> Option<(usize, usize)> {
    use std::cell::RefCell;
    thread_local! {
        static SCRATCH: RefCell<Option<(usize, usize, Scratch)>> = const { RefCell::new(None) };
    }
    let width = tdfa_backend::mark_file_width(t);
    SCRATCH.with(|cell| {
        let mut slot = cell.borrow_mut();
        let rebuild = !matches!(&*slot, Some((w, g, _)) if *w == width && *g == t.num_capture_groups);
        if rebuild {
            *slot = Some((
                width,
                t.num_capture_groups,
                Scratch::new(width, t.num_capture_groups),
            ));
        }
        let (_, _, scratch) = slot.as_mut().expect("scratch just installed");
        let m = tdfa_backend::execute_reuse_warm(t, input, start, scratch, t.prefix_skip)?;
        let n = caps.len().min(scratch.norm_buf.len());
        caps[..n].copy_from_slice(&scratch.norm_buf[..n]);
        Some((m.range.start, m.range.end))
    })
}

/// 8-aligned storage for byte-string-encoded tables. A byte-string literal is
/// a *single token* through the proc-macro bridge, where an equivalent array
/// literal is hundreds of thousands — the difference between seconds and
/// minutes of `regex!` expansion for large automata.
#[repr(C, align(8))]
pub struct AlignedBytes<const N: usize>(pub [u8; N]);

macro_rules! le_cast {
    ($name:ident, $ty:ty) => {
        /// Reinterpret little-endian bytes as a typed table slice. Const, so
        /// `StaticTdfa` initializers stay `static`-evaluable. Compile-fails on
        /// big-endian targets (emit numeric literals there instead — the
        /// emitter currently assumes an LE build host and target).
        pub const fn $name<const N: usize>(b: &'static AlignedBytes<N>) -> &'static [$ty] {
            assert!(cfg!(target_endian = "little"), "table tier requires a little-endian target");
            assert!(N % core::mem::size_of::<$ty>() == 0);
            // SAFETY: alignment guaranteed by AlignedBytes(align 8) and every
            // bit pattern is a valid $ty; length is in-bounds by construction.
            unsafe {
                core::slice::from_raw_parts(
                    b.0.as_ptr().cast::<$ty>(),
                    N / core::mem::size_of::<$ty>(),
                )
            }
        }
    };
}
le_cast!(le_u16s, u16);
le_cast!(le_u32s, u32);
le_cast!(le_u64s, u64);
// Sound: MoveOp is `#[repr(C)]` two `u16`s with no padding/niches, so any
// blob the emitter writes (dst.to_le_bytes() ++ src.to_le_bytes() per entry)
// reinterprets validly. `MoveOp` is on the per-byte hot path, so this cast —
// not a lazy per-search rebuild — is what keeps it zero-cost.
le_cast!(le_moveops, MoveOp);
