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
}

impl CompiledMatcher {
    pub const fn from_parts(
        prefilter: &'static PrefilterSpec,
        verify: VerifyFn,
        num_groups: usize,
        group_names: &'static [&'static str],
    ) -> Self {
        Self {
            prefilter,
            verify,
            num_groups,
            group_names,
        }
    }

    /// Searches `text` to find the first match.
    pub fn find(&self, text: &str) -> Option<Match> {
        self.find_iter(text).next()
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
