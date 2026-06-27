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
    /// The whole regex is exactly an *alternation of literals* (e.g.
    /// `Sherlock|Holmes|Watson`) — no captures, tail, or assertions. An
    /// aho-corasick Teddy (SIMD multi-substring) searcher matches all of them at
    /// once, and the matched needle's span IS the match, so like `WholeLiteral`
    /// there's no automaton. This is the regex crate's multi-literal path; it
    /// replaces the leaky "union of first bytes" start predicate the generic
    /// `Prefix` strategy would otherwise use for a literal alternation.
    #[cfg(feature = "prefilter-teddy")]
    MultiLiteral {
        searcher: aho_corasick::packed::Searcher,
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
        /// Optional cheap secondary filter: a byte that *must* occur within a
        /// bounded offset window of any match start. Rejects most prefilter
        /// candidates before paying the per-candidate verify call.
        lit_window: Option<LitWindow>,
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
        /// SIMD multi-substring (Teddy) prefilter over the case cross-product of
        /// a bounded leading prefix (e.g. `Sherl`/`ſherl`/…). When `Some`, it
        /// replaces the `searcher` scan: far more selective than a single rare
        /// byte. `None` (Teddy declined / unsupported target) keeps `searcher`.
        #[cfg(feature = "prefilter-teddy")]
        teddy: Option<aho_corasick::packed::Searcher>,
    },
    /// An *alternation whose every branch has a leading literal prefix* (e.g.
    /// `Sher[a-z]+|Hol[a-z]+`, with or without `/i`). A Teddy searcher over the
    /// union of the branches' leading case cross-products locates candidates;
    /// the anchored `forward` TDFA verifies the full branch (tail + captures)
    /// from each candidate start. Unlike `MultiLiteral` the literals are only a
    /// prefix, so the automaton verify is required.
    #[cfg(feature = "prefilter-teddy")]
    AltPrefix {
        forward: Tdfa,
        teddy: aho_corasick::packed::Searcher,
    },
    /// A required *interior or suffix* literal, with no usable prefix (e.g.
    /// `\w+\s+Holmes`, `(\w+)'(\w+)`). Find the literal with `memmem`, drive the
    /// `reverse` DFA (of the pattern *before* the literal) leftward from the
    /// literal's start to the leftmost match start, then run the `forward`
    /// anchored TDFA there for the real extent and captures (the part after the
    /// literal is verified forward).
    ReverseInner {
        forward: Tdfa,
        reverse: Dfa,
        literal: Box<memmem::Finder<'static>>,
    },
}

/// Error building a [`TdfaProgram`]: either the NFA or the TDFA stage failed
/// (budget/unsupported feature).
#[derive(Debug)]
pub enum BuildError {
    Nfa(crate::automata::nfa::Error),
    Tdfa(tdfa::Error),
}

/// Timing/counter breakdown for one `find_iter`-style pass through a
/// [`TdfaProgram`]. Intended for backend diagnostics and benchmark triage.
#[cfg(feature = "std")]
#[derive(Debug, Clone)]
pub struct TdfaDiagnostics {
    pub strategy: &'static str,
    pub jit_active: bool,
    pub matches: usize,
    pub search_calls: usize,
    pub search_hits: usize,
    pub verify_calls: usize,
    pub search_time: std::time::Duration,
    pub verify_time: std::time::Duration,
    pub total_time: std::time::Duration,
}

#[cfg(feature = "std")]
impl TdfaDiagnostics {
    fn new(strategy: &'static str, jit_active: bool) -> Self {
        Self {
            strategy,
            jit_active,
            matches: 0,
            search_calls: 0,
            search_hits: 0,
            verify_calls: 0,
            search_time: std::time::Duration::ZERO,
            verify_time: std::time::Duration::ZERO,
            total_time: std::time::Duration::ZERO,
        }
    }
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

/// If the regex is *exactly* an alternation of literal byte sequences — no
/// captures, tail, or assertions (e.g. `Sherlock|Holmes|Watson`) — return the
/// alternatives in source (priority) order. Each is then a whole match, so a
/// Teddy multi-substring searcher's span IS the match and no automaton is needed
/// (the multi-literal analogue of [`whole_literal`]).
///
/// `None` unless there are 2–`MAX_LITERALS` non-empty literal alternatives and
/// nothing else: the structural whitelist (`Alt`/`ByteSequence`, plus trailing
/// zero-width `Goal`/`Empty`) guarantees no capture groups or anchors, so the
/// matched needle's `start..end` is the exact ECMAScript leftmost-first match.
#[cfg(feature = "prefilter-teddy")]
fn literal_alternation(re: &ir::Regex) -> Option<Vec<Vec<u8>>> {
    use ir::Node;
    // Past this many literals Teddy spills to its slower verify path; the
    // generic strategy is then no worse. (Practical alternations are small.)
    const MAX_LITERALS: usize = 64;

    // The optimizer wraps the top-level `Alt` in a `Cat` with a trailing `Goal`.
    let mut node = &re.node;
    if let Node::Cat(nodes) = node {
        let mut lit = None;
        for n in nodes {
            match n {
                Node::Goal | Node::Empty => {}
                other if lit.is_none() => lit = Some(other),
                _ => return None, // a second consuming node — not a bare alternation
            }
        }
        node = lit?;
    }

    // Flatten the right-nested `Alt(a, Alt(b, …))` into its literal leaves, in
    // left-to-right priority order. Any non-literal leaf disqualifies the whole
    // pattern (fall back to the generic strategy).
    fn collect(node: &Node, out: &mut Vec<Vec<u8>>) -> Option<()> {
        match node {
            Node::Alt(a, b) => {
                collect(a, out)?;
                collect(b, out)?;
                Some(())
            }
            Node::ByteSequence(b) if !b.is_empty() => {
                out.push(b.clone());
                Some(())
            }
            _ => None,
        }
    }
    let mut lits = Vec::new();
    collect(node, &mut lits)?;
    if lits.len() < 2 || lits.len() > MAX_LITERALS {
        return None;
    }
    Some(lits)
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

/// Bounded case cross-product of the literal's leading positions, for the
/// `prefilter-teddy` Teddy prefilter — e.g. `Sherlock`/i → `{Sherl, sherl,
/// ſherl, …}`. The per-position alternatives are read straight off the same
/// optimized-IR fold sets the automaton matches (`ByteSet`/`Char` = clean ASCII,
/// `CharSet` = includes the non-ASCII ſ fold), so the set is *exhaustive* for the
/// covered positions: every real match begins with one of these byte-strings, so
/// using them as the candidate filter misses nothing. The anchored automaton
/// still verifies the full pattern (and captures) from each candidate start, so
/// false positives are harmless.
///
/// Returns `None` when the pattern can't yield ≥2 usable leading positions, or
/// the cross-product would exceed the caps — the caller then keeps the
/// single-byte `CaseFoldSearcher`.
#[cfg(feature = "prefilter-teddy")]
fn casefold_prefix_variants(re: &ir::Regex) -> Option<Vec<Vec<u8>>> {
    use ir::Node;
    // Teddy is fastest with a *small* literal set: its packed buckets stay
    // selective up to ~32 needles, past which it spills into a slower verify
    // path (measured: a 24-needle 4-byte prefix scans ~13 GB/s, but 48 needles
    // drop to ~8 GB/s — slower than the single-byte memchr it replaces). So cap
    // at a 4-position prefix (mirroring the regex crate's choice) and 32 needles;
    // the `CaseFoldSearcher` fallback covers anything past that.
    const MAX_PATTERNS: usize = 32;
    const MAX_PREFIX_POSITIONS: usize = 4;

    let nodes: &[Node] = match &re.node {
        Node::Cat(n) => n,
        _ => return None,
    };
    leading_fold_variants(nodes, MAX_PREFIX_POSITIONS, MAX_PATTERNS)
}

/// Concrete byte-strings for the leading literal positions of `nodes` — the case
/// cross-product, 1 byte for an ASCII fold position, 1–2 for a `CharSet`'s
/// non-ASCII fold (ſ). Reads the fold alternatives straight off the optimized-IR
/// nodes the automaton matches, so the set is exhaustive for the covered
/// positions. Stops at the first non-literal node (the prefix up to it is still
/// a required key) and at the position/pattern caps. `None` if fewer than 2
/// positions are usable (too short to be a selective Teddy key).
#[cfg(feature = "prefilter-teddy")]
fn leading_fold_variants(
    nodes: &[ir::Node],
    max_positions: usize,
    max_patterns: usize,
) -> Option<Vec<Vec<u8>>> {
    use ir::Node;
    let mut posalts: Vec<Vec<Vec<u8>>> = Vec::new();
    'outer: for n in nodes {
        match n {
            Node::Goal | Node::Empty => {}
            Node::ByteSet(bytes) => {
                if bytes.iter().any(|&b| b >= 0x80) {
                    break;
                }
                posalts.push(bytes.iter().map(|&b| vec![b]).collect());
            }
            Node::ByteSequence(bytes) => {
                for &b in bytes {
                    if b >= 0x80 {
                        break 'outer;
                    }
                    posalts.push(vec![vec![b]]);
                }
            }
            Node::Char { c } if *c < 0x80 => {
                posalts.push(vec![vec![*c as u8]]);
            }
            Node::CharSet(chars) => {
                let mut alts: Vec<Vec<u8>> = Vec::with_capacity(chars.len());
                for &c in chars.iter() {
                    let ch = char::from_u32(c)?;
                    let mut buf = [0u8; 4];
                    alts.push(ch.encode_utf8(&mut buf).as_bytes().to_vec());
                }
                posalts.push(alts);
            }
            _ => break,
        }
    }

    // Cross-product the leading positions under both caps.
    let mut variants: Vec<Vec<u8>> = vec![Vec::new()];
    let mut used = 0;
    for alts in &posalts {
        if used >= max_positions || variants.len() * alts.len() > max_patterns {
            break;
        }
        variants = variants
            .iter()
            .flat_map(|prefix| {
                alts.iter().map(move |a| {
                    let mut v = prefix.clone();
                    v.extend_from_slice(a);
                    v
                })
            })
            .collect();
        used += 1;
    }
    (used >= 2).then_some(variants)
}

/// Teddy needles for an *alternation whose every branch has a leading literal
/// prefix* (e.g. `Sher[a-z]+|Hol[a-z]+`, with or without `/i`, or a
/// case-insensitive literal alternation `Sherlock|Holmes`/i): the union of each
/// branch's leading case cross-product. Every match of the alternation begins
/// with one of these byte-strings, so they form a complete candidate filter; the
/// anchored automaton then verifies the full branch (tail + captures) from each
/// candidate start.
///
/// `None` unless there are ≥2 branches and *every* branch yields a usable prefix
/// (a branch with no leading literal couldn't be prefiltered without missing its
/// matches). When the full-length union would exceed the cap, the prefix length
/// is shortened (down to 2 positions) so more branches still fit — shorter keys
/// are less selective but keep Teddy in its fast regime.
#[cfg(feature = "prefilter-teddy")]
fn alternation_prefix_variants(re: &ir::Regex) -> Option<Vec<Vec<u8>>> {
    use ir::Node;
    // Per-branch prefix length, and a union cap kept near Teddy's fast ceiling.
    const MAX_BRANCH_POSITIONS: usize = 4;
    const MIN_BRANCH_POSITIONS: usize = 2;
    const MAX_BRANCH_PATTERNS: usize = 32;
    const MAX_TOTAL_PATTERNS: usize = 48;

    // Unwrap the optimizer's `Cat([Alt, Goal])` wrapper to the bare `Alt`.
    let mut node = &re.node;
    if let Node::Cat(nodes) = node {
        let mut inner = None;
        for n in nodes {
            match n {
                Node::Goal | Node::Empty => {}
                other if inner.is_none() => inner = Some(other),
                _ => return None,
            }
        }
        node = inner?;
    }
    if !matches!(node, Node::Alt(..)) {
        return None;
    }

    // Flatten the right-nested `Alt` into its branch subtrees (not recursing into
    // a branch's own `Cat`).
    fn branches<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
        match node {
            Node::Alt(a, b) => {
                branches(a, out);
                branches(b, out);
            }
            other => out.push(other),
        }
    }
    let mut leaves = Vec::new();
    branches(node, &mut leaves);
    if leaves.len() < 2 {
        return None;
    }

    // Longest prefix length whose union fits the cap (more branches → shorter
    // keys). Bail if any branch has no usable prefix at all (can't prefilter it).
    for max_pos in (MIN_BRANCH_POSITIONS..=MAX_BRANCH_POSITIONS).rev() {
        let mut needles: Vec<Vec<u8>> = Vec::new();
        for leaf in &leaves {
            // A branch is a `Cat` of positions, or a single literal node.
            let branch_nodes: &[Node] = match leaf {
                Node::Cat(n) => n,
                other => core::slice::from_ref(other),
            };
            // A branch with no usable prefix can never be prefiltered — give up.
            let mut v = leading_fold_variants(branch_nodes, max_pos, MAX_BRANCH_PATTERNS)?;
            needles.append(&mut v);
        }
        needles.sort();
        needles.dedup();
        if needles.len() <= MAX_TOTAL_PATTERNS {
            return Some(needles);
        }
    }
    None
}

/// Build an aho-corasick Teddy (SIMD multi-substring) searcher over the case
/// cross-product. `None` when packed search isn't applicable (an unsupported
/// target, or the literal set declines) — the caller falls back to
/// `CaseFoldSearcher`.
#[cfg(feature = "prefilter-teddy")]
fn build_teddy(patterns: &[Vec<u8>]) -> Option<aho_corasick::packed::Searcher> {
    use aho_corasick::packed::{Config, MatchKind};
    Config::new()
        .match_kind(MatchKind::LeftmostFirst)
        .builder()
        .extend(patterns.iter())
        .build()
}

/// Whether Teddy is the right tool for this needle set, vs. a single-byte
/// `memchr`. Teddy scans ~2x slower per byte than `memchr`, so when *every*
/// needle shares one **rare** first byte (e.g. `Sherlock|Street` → both `S`), a
/// `memchr` on that byte plus a cheap verify (the byte is rare, so few hits)
/// wins — let the generic `Prefix` strategy handle it. With diverse or common
/// first bytes, Teddy's selective multi-byte scan earns back its slower throughput.
#[cfg(feature = "prefilter-teddy")]
fn teddy_preferred(needles: &[Vec<u8>]) -> bool {
    let Some(&first) = needles.first().and_then(|n| n.first()) else {
        return false;
    };
    let shared_rare_prefix =
        !byte_is_common(first) && needles.iter().all(|n| n.first() == Some(&first));
    !shared_rare_prefix
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
        StartPredicate::ByteBracket(bm) => {
            !(0..=255u8).any(|b| byte_is_common(b) && bm.contains(b))
        }
        StartPredicate::Arbitrary | StartPredicate::StartAnchored => false,
    }
}

/// A required literal byte that must occur within `[lo, hi]` bytes of any match
/// start — a cheap secondary filter for an unselective prefix prefilter.
#[derive(Debug, Clone, Copy)]
struct LitWindow {
    byte: u8,
    lo: usize,
    hi: usize,
}

impl LitWindow {
    /// Sound necessary condition: a match starting at `start` must have `byte`
    /// somewhere in `bytes[start+lo ..= start+hi]`. Returns `false` (reject)
    /// only when the byte is provably absent from that window.
    #[inline(always)]
    fn admits(&self, bytes: &[u8], start: usize) -> bool {
        let from = start + self.lo;
        if from >= bytes.len() {
            return false;
        }
        let to = (start + self.hi + 1).min(bytes.len());
        // The window is tiny (<= MAX_WINDOW), so a scalar scan beats `memchr`'s
        // per-call SIMD setup.
        bytes[from..to].contains(&self.byte)
    }
}

/// Min/max byte width of `node`, or `None` if unbounded (`*`/`+`) or unknown
/// (backref/stringset). Upper bound is conservative (never under-estimates), so
/// it is safe to use for a sound offset window.
fn node_width(node: &ir::Node) -> Option<(usize, usize)> {
    use ir::Node::*;
    let mb = |cps_ascii: bool| if cps_ascii { (1, 1) } else { (1, 4) };
    Some(match node {
        Empty | Goal | Anchor { .. } | WordBoundary { .. } | LookaroundAssertion { .. } => (0, 0),
        Char { c } => mb(*c < 0x80),
        ByteSequence(b) => (b.len(), b.len()),
        ByteSet(_) => (1, 1),
        CharSet(cs) => mb(cs.iter().all(|&c| c < 0x80)),
        Bracket(bc) => mb(bracket_is_ascii(bc)),
        MatchAny | MatchAnyExceptLineTerminator => (1, 4),
        Cat(nodes) => {
            let (mut lo, mut hi) = (0usize, 0usize);
            for n in nodes {
                let (a, b) = node_width(n)?;
                lo += a;
                hi += b;
            }
            (lo, hi)
        }
        Alt(a, b) => {
            let (alo, ahi) = node_width(a)?;
            let (blo, bhi) = node_width(b)?;
            (alo.min(blo), ahi.max(bhi))
        }
        CaptureGroup { contents, .. } => node_width(contents)?,
        Loop { loopee, quant, .. } | Loop1CharBody { loopee, quant } => {
            let (blo, bhi) = node_width(loopee)?;
            let max = quant.max?; // unbounded → no usable window
            (quant.min * blo, max * bhi)
        }
        BackRef { .. } | StringSet { .. } => return None,
    })
}

fn bracket_is_ascii(bc: &crate::types::BracketContents) -> bool {
    !bc.invert && bc.cps.intervals().iter().all(|iv| iv.last < 0x80)
}

/// The first *mandatory* literal byte in `node` and its offset window `[lo, hi]`
/// from the node's start. Walks past fixed/bounded prefixes (digit runs, etc.),
/// accumulating their widths. Returns `None` when no mandatory literal is
/// reachable within a bounded distance.
fn first_required_byte(node: &ir::Node) -> Option<(u8, usize, usize)> {
    use ir::Node::*;
    match node {
        ByteSequence(b) => b.first().map(|&x| (x, 0, 0)),
        Char { c } if *c < 0x80 => Some((*c as u8, 0, 0)),
        Cat(nodes) => {
            let (mut lo, mut hi) = (0usize, 0usize);
            for n in nodes {
                if let Some((byte, blo, bhi)) = first_required_byte(n) {
                    return Some((byte, lo + blo, hi + bhi));
                }
                let (wlo, whi) = node_width(n)?;
                lo += wlo;
                hi += whi;
            }
            None
        }
        CaptureGroup { contents, .. } => first_required_byte(contents),
        // A loop runs its body at least `min` times; if `min >= 1` its first
        // (mandatory) iteration may carry the literal.
        Loop { loopee, quant, .. } | Loop1CharBody { loopee, quant } if quant.min >= 1 => {
            first_required_byte(loopee)
        }
        _ => None,
    }
}

/// Top-level: a required interior literal byte at a bounded, non-zero offset,
/// usable as a cheap secondary filter behind an unselective prefix prefilter.
fn leading_required_byte(re: &ir::Regex) -> Option<LitWindow> {
    // The cap keeps the inline window scan short; `hi >= 1` ensures the literal
    // is past the prefilter's own first byte (offset 0).
    const MAX_WINDOW: usize = 16;
    let mut node = &re.node;
    while let ir::Node::CaptureGroup { contents, .. } = node {
        node = contents;
    }
    let (byte, lo, hi) = first_required_byte(node)?;
    if hi >= 1 && hi <= MAX_WINDOW {
        Some(LitWindow { byte, lo, hi })
    } else {
        None
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

        // A literal alternation (`Sherlock|Holmes|…`): a Teddy multi-substring
        // searcher matches every alternative at once and the span is the match —
        // no automaton, and far more selective than the generic `Prefix`
        // strategy's union-of-first-bytes predicate. Skipped if Teddy declines.
        #[cfg(feature = "prefilter-teddy")]
        if let Some(lits) = literal_alternation(re) {
            // Skip when a single rare `memchr` byte beats Teddy (→ generic Prefix).
            if teddy_preferred(&lits) {
                if let Some(searcher) = build_teddy(&lits) {
                    return Ok(Self::from_parts(
                        Strategy::MultiLiteral { searcher },
                        Box::new([]),
                    ));
                }
            }
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
                // A Teddy SIMD prefilter over the leading case cross-product,
                // when the pattern admits one; else `None` keeps `searcher`.
                #[cfg(feature = "prefilter-teddy")]
                let teddy = casefold_prefix_variants(re).and_then(|pats| build_teddy(&pats));
                return Ok(Self::from_parts(
                    Strategy::CaseFoldLiteral {
                        forward,
                        searcher,
                        prefix_lo: run.prefix_lo,
                        prefix_hi: run.prefix_hi,
                        #[cfg(feature = "prefilter-teddy")]
                        teddy,
                    },
                    group_names,
                ));
            }
        }

        // An alternation whose every branch has a leading literal prefix
        // (`Sher[a-z]+|Hol[a-z]+`, incl. `/i`): a Teddy searcher over the union
        // of the branches' leading case cross-products locates candidates, then
        // the anchored automaton verifies. Replaces the generic strategy's leaky
        // union-of-first-bytes predicate (or a plain `Scan` under `/i`).
        #[cfg(feature = "prefilter-teddy")]
        if let Some(needles) = alternation_prefix_variants(re) {
            // Skip when a single rare `memchr` byte beats Teddy (→ generic Prefix).
            if teddy_preferred(&needles) {
                if let Some(teddy) = build_teddy(&needles) {
                    let nfa = Nfa::try_from(re)?;
                    let mut forward = Tdfa::try_from(&nfa)?;
                    forward.optimize();
                    let group_names = forward.group_names().to_vec().into_boxed_slice();
                    return Ok(Self::from_parts(
                        Strategy::AltPrefix { forward, teddy },
                        group_names,
                    ));
                }
            }
        }

        let pred = startpredicate::predicate_for_re(re);
        // A selective start predicate is a fast prefilter: `ByteSeq`/`ByteSet1..3`
        // via SIMD memchr, and a `ByteBracket` (e.g. `[0-9]`) via the SIMD nibble
        // classifier (`ByteBitmap::find_in`). Use it directly.
        if should_prefilter(&pred) {
            // Exception: a `ByteBracket` (a character *class*, not a specific
            // byte) overlapping a leading loop is a poor prefilter — every byte
            // of a run becomes a candidate, so a dead run costs O(n²) (e.g.
            // `[0-9]+foo` over digit-heavy input). A required multi-byte literal
            // elsewhere is a `memmem` prefilter that stays selective on *any*
            // input, so prefer the reverse-inner strategy when one exists and
            // builds. (A single-byte interior literal like the `.` in `ip` is no
            // better than the class scan, so the `len >= 2` gate keeps those on
            // the prefix path.)
            if matches!(pred, StartPredicate::ByteBracket(_)) {
                if let Some((prefix, literal)) = startpredicate::required_inner_literal(re) {
                    if literal.len() >= 2 {
                        if let Some(program) = Self::try_reverse_inner(re, prefix, literal)? {
                            return Ok(program);
                        }
                    }
                }
            }
            return Self::build_prefix(re, pred);
        }

        // No fast prefix prefilter. If the regex has a required interior/suffix
        // literal, try the reverse-automaton strategy (find the literal, walk the
        // reversed prefix backwards to the start, forward-verify). Falls back when
        // that isn't applicable (e.g. zero-width assertions defeat the tag-free
        // reverse DFA — see `reverse::reverse_nfa`).
        if let Some((prefix, literal)) = startpredicate::required_inner_literal(re) {
            if let Some(program) = Self::try_reverse_inner(re, prefix, literal)? {
                return Ok(program);
            }
        }

        let nfa = Nfa::try_from_unanchored(re)?;
        let mut unanchored = Tdfa::try_from(&nfa)?;
        unanchored.optimize();
        let group_names = unanchored.group_names().to_vec().into_boxed_slice();
        Ok(Self::from_parts(Strategy::Scan { unanchored }, group_names))
    }

    /// Build a [`Strategy::Prefix`] program: an anchored verify automaton driven
    /// by the start-predicate prefilter. It also precomputes a warm start that
    /// skips re-scanning the prefilter-matched prefix through the automaton — an
    /// exact literal (`ByteSeq`), or a single byte class such as `[0-9]` (always
    /// one byte) — when doing so writes no marks. The anchored automaton matches
    /// only at the offset handed to `execute`, so a candidate that fails dies fast
    /// and we advance.
    fn build_prefix(re: &ir::Regex, pred: StartPredicate) -> Result<Self, BuildError> {
        let nfa = Nfa::try_from(re)?;
        let mut anchored = Tdfa::try_from(&nfa)?;
        anchored.optimize();
        let skip = match &pred {
            StartPredicate::ByteSeq(finder) => {
                tdfa_backend::compute_prefix_skip(&anchored, finder.needle())
            }
            // A byte-class prefilter matches exactly one byte; warm-start past it
            // when every admissible byte shares one mark-free start transition.
            StartPredicate::ByteSet1(bs) => {
                tdfa_backend::compute_byteclass_skip(&anchored, bs.iter().copied())
            }
            StartPredicate::ByteSet2(bs) => {
                tdfa_backend::compute_byteclass_skip(&anchored, bs.iter().copied())
            }
            StartPredicate::ByteSet3(bs) => {
                tdfa_backend::compute_byteclass_skip(&anchored, bs.iter().copied())
            }
            StartPredicate::ByteBracket(bm) => {
                tdfa_backend::compute_byteclass_skip(&anchored, bm.iter())
            }
            StartPredicate::Arbitrary | StartPredicate::StartAnchored => None,
        };
        // A non-`ByteSeq` prefilter (a byte set/bracket like `[0-9]`) is often
        // unselective; a required interior literal at a bounded distance (the `.`
        // in `(?:[0-9]{1,3}\.){3}…`) lets us reject most candidates with a cheap
        // inline window scan before the per-candidate verify call.
        let lit_window = match &pred {
            StartPredicate::ByteSeq(_) => None,
            _ => leading_required_byte(re),
        };
        let group_names = anchored.group_names().to_vec().into_boxed_slice();
        Ok(Self::from_parts(
            Strategy::Prefix {
                anchored,
                prefilter: pred,
                skip,
                lit_window,
            },
            group_names,
        ))
    }

    /// Try to build a [`Strategy::ReverseInner`] for a regex with a required
    /// `literal` whose preceding `prefix` nodes are searched in reverse. Returns
    /// `Ok(None)` (caller falls back to a scan) when the reverse automaton can't
    /// be built — when the prefix has zero-width assertions the tag-free reverse
    /// DFA can't honor, when the reverse DFA exceeds its state budget, or when
    /// the prefix sub-pattern doesn't form a buildable NFA. NFA/TDFA build
    /// failures for the *whole* pattern propagate as `Err`.
    fn try_reverse_inner(
        re: &ir::Regex,
        prefix: Vec<ir::Node>,
        literal: Vec<u8>,
    ) -> Result<Option<Self>, BuildError> {
        // Reverse DFA of the pattern *before* the literal (captures irrelevant —
        // the reverse is tag-free), used to walk leftward to the match start.
        // Re-append the `Goal` terminator the slice dropped (the NFA builder
        // requires a pattern to end in it).
        let mut prefix = prefix;
        prefix.push(ir::Node::Goal);
        let prefix_re = ir::Regex {
            node: ir::Node::Cat(prefix),
            flags: re.flags,
        };
        let Ok(prefix_nfa) = Nfa::try_from(&prefix_re) else {
            return Ok(None);
        };
        let Some(reverse_nfa) = reverse::reverse_nfa(&prefix_nfa) else {
            return Ok(None);
        };
        let Ok(reverse) = Dfa::try_from(&reverse_nfa) else {
            return Ok(None);
        };
        let mut forward = Tdfa::try_from(&Nfa::try_from(re)?)?;
        forward.optimize();
        let group_names = forward.group_names().to_vec().into_boxed_slice();
        let literal = Box::new(memmem::Finder::new(&literal).into_owned());
        Ok(Some(Self::from_parts(
            Strategy::ReverseInner {
                forward,
                reverse,
                literal,
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
        // The verify automaton plus, for the `Prefix` strategy, the prefix-skip
        // baked into the compiled prologue (warm-start past the prefilter-matched
        // prefix). Other strategies have no such skip.
        let target = match &self.strategy {
            Strategy::Prefix { anchored, skip, .. } => Some((anchored, *skip)),
            Strategy::CaseFoldLiteral { forward, .. } => Some((forward, None)),
            #[cfg(feature = "prefilter-teddy")]
            Strategy::AltPrefix { forward, .. } => Some((forward, None)),
            Strategy::ReverseInner { forward, .. } => Some((forward, None)),
            // The unanchored single-pass scan: the JIT's capture tier handles
            // its `.*?`-stamped start.
            Strategy::Scan { unanchored } => Some((unanchored, None)),
            // Whole-literal / multi-literal need no automaton.
            Strategy::WholeLiteral { .. } => None,
            #[cfg(feature = "prefilter-teddy")]
            Strategy::MultiLiteral { .. } => None,
        };
        if let Some((tdfa, skip)) = target {
            self.jit = JittedTdfa::compile(tdfa, skip).ok();
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
            #[cfg(feature = "prefilter-teddy")]
            Strategy::MultiLiteral { searcher } => {
                // The matched needle (leftmost-first) is the entire match.
                let m = searcher.find_in(bytes, aho_corasick::Span::from(offset..bytes.len()))?;
                Some(NfaMatch {
                    range: m.start()..m.end(),
                    captures: Vec::new(),
                })
            }
            #[cfg(feature = "prefilter-teddy")]
            Strategy::AltPrefix { forward, teddy } => {
                // Each Teddy hit is a branch prefix start; the automaton verifies
                // the full branch (tail + captures) from there.
                let mut pos = offset;
                loop {
                    let m = teddy.find_in(bytes, aho_corasick::Span::from(pos..bytes.len()))?;
                    if let Some(found) = self.verify_at(forward, bytes, m.start(), scratch) {
                        return Some(found);
                    }
                    pos = m.start() + 1;
                }
            }
            Strategy::CaseFoldLiteral {
                forward,
                searcher,
                prefix_lo,
                prefix_hi,
                #[cfg(feature = "prefilter-teddy")]
                teddy,
            } => {
                // Teddy path: each hit is the concrete literal start (the leading
                // ſ/S/s is part of the matched needle), so there's no prefix-width
                // window — verify the full pattern straight from `m.start()`.
                #[cfg(feature = "prefilter-teddy")]
                if let Some(teddy) = teddy {
                    use aho_corasick::Span;
                    let mut pos = offset;
                    loop {
                        let m = teddy.find_in(bytes, Span::from(pos..bytes.len()))?;
                        if let Some(found) = self.verify_at(forward, bytes, m.start(), scratch) {
                            return Some(found);
                        }
                        pos = m.start() + 1;
                    }
                }
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
            Strategy::Scan { unanchored } => self.verify_at(unanchored, bytes, offset, scratch),
            Strategy::Prefix {
                anchored,
                prefilter,
                skip,
                lit_window,
            } => {
                // JIT path: verify each prefilter candidate with native code.
                // The warm-start `skip` is baked into the compiled prologue, so
                // `jit.run` resumes past the matched prefix automatically.
                #[cfg(feature = "tdfa-jit")]
                if let Some(jit) = &self.jit {
                    let mut pos = offset;
                    loop {
                        let cand = prefilter.find_from(bytes, pos)?;
                        // Cheap secondary filter: skip the verify call when the
                        // required interior literal can't be in range.
                        if lit_window.is_none_or(|w| w.admits(bytes, cand)) {
                            if let Some(m) = jit.run(anchored, bytes, cand, scratch) {
                                return Some(m);
                            }
                        }
                        pos = cand + 1;
                    }
                }
                if let Some(w) = lit_window {
                    let mut pos = offset;
                    loop {
                        let cand = prefilter.find_from(bytes, pos)?;
                        if w.admits(bytes, cand) {
                            if let Some(m) = tdfa_backend::execute_reuse_warm(
                                anchored, bytes, cand, scratch, *skip,
                            ) {
                                return Some(m);
                            }
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
            } => {
                let mut pos = offset;
                loop {
                    let i = literal.find(&bytes[pos..]).map(|k| pos + k)?;
                    // Walk the reversed prefix back from the literal's start to
                    // the leftmost match start, then forward-verify there for the
                    // real extent + captures. The forward run is the source of
                    // truth (it fixes a greedy end, matches the part after the
                    // literal, and produces captures); the reverse only locates
                    // the start.
                    if let Some(s) = reverse::reverse_find_start(reverse, bytes, i, offset) {
                        if let Some(m) = self.verify_at(forward, bytes, s, scratch) {
                            return Some(m);
                        }
                    }
                    pos = i + 1;
                }
            }
        }
    }

    #[cfg(feature = "std")]
    fn jit_is_active(&self) -> bool {
        #[cfg(feature = "tdfa-jit")]
        {
            self.jit.is_some()
        }
        #[cfg(not(feature = "tdfa-jit"))]
        {
            false
        }
    }

    #[cfg(feature = "std")]
    fn strategy_name(&self) -> &'static str {
        match &self.strategy {
            Strategy::WholeLiteral { .. } => "whole-literal",
            #[cfg(feature = "prefilter-teddy")]
            Strategy::MultiLiteral { .. } => "multi-literal",
            Strategy::Scan { .. } => "scan",
            Strategy::Prefix { .. } => "prefix",
            Strategy::CaseFoldLiteral { .. } => "casefold-literal",
            #[cfg(feature = "prefilter-teddy")]
            Strategy::AltPrefix { .. } => "alt-prefix",
            Strategy::ReverseInner { .. } => "reverse-inner",
        }
    }

    /// Run a complete `find_iter`-style pass and return a phase breakdown.
    /// Normal matching paths are unchanged; this is only called by benchmarks or
    /// tools that explicitly ask for diagnostics.
    #[cfg(feature = "std")]
    pub fn diagnostics(&self, text: &str) -> TdfaDiagnostics {
        let bytes = text.as_bytes();
        let mut scratch = Scratch::new(self.mark_width());
        let mut diag = TdfaDiagnostics::new(self.strategy_name(), self.jit_is_active());
        let mut offset = 0usize;
        while offset <= bytes.len() {
            let start = std::time::Instant::now();
            let m = self.find_at_diagnostic(bytes, offset, &mut scratch, &mut diag);
            diag.total_time += start.elapsed();
            let Some(m) = m else {
                break;
            };
            diag.matches += 1;
            if m.range.end == m.range.start {
                offset = next_utf8_offset(text, m.range.end);
            } else {
                offset = m.range.end;
            }
        }
        diag
    }

    #[cfg(feature = "std")]
    fn find_at_diagnostic(
        &self,
        bytes: &[u8],
        offset: usize,
        scratch: &mut Scratch<u32>,
        diag: &mut TdfaDiagnostics,
    ) -> Option<NfaMatch> {
        match &self.strategy {
            Strategy::WholeLiteral { literal, len } => {
                diag.search_calls += 1;
                let start = std::time::Instant::now();
                let i = literal.find(&bytes[offset..]).map(|k| offset + k);
                diag.search_time += start.elapsed();
                let i = i?;
                diag.search_hits += 1;
                Some(NfaMatch {
                    range: i..i + len,
                    captures: Vec::new(),
                })
            }
            #[cfg(feature = "prefilter-teddy")]
            Strategy::MultiLiteral { searcher } => {
                diag.search_calls += 1;
                let start = std::time::Instant::now();
                let m = searcher.find_in(bytes, aho_corasick::Span::from(offset..bytes.len()));
                diag.search_time += start.elapsed();
                let m = m?;
                diag.search_hits += 1;
                Some(NfaMatch {
                    range: m.start()..m.end(),
                    captures: Vec::new(),
                })
            }
            #[cfg(feature = "prefilter-teddy")]
            Strategy::AltPrefix { forward, teddy } => {
                let mut pos = offset;
                loop {
                    diag.search_calls += 1;
                    let start = std::time::Instant::now();
                    let found = teddy.find_in(bytes, aho_corasick::Span::from(pos..bytes.len()));
                    diag.search_time += start.elapsed();
                    let m = found?;
                    diag.search_hits += 1;
                    diag.verify_calls += 1;
                    let start = std::time::Instant::now();
                    let v = self.verify_at(forward, bytes, m.start(), scratch);
                    diag.verify_time += start.elapsed();
                    if v.is_some() {
                        return v;
                    }
                    pos = m.start() + 1;
                }
            }
            Strategy::CaseFoldLiteral {
                forward,
                searcher,
                prefix_lo,
                prefix_hi,
                #[cfg(feature = "prefilter-teddy")]
                teddy,
            } => {
                #[cfg(feature = "prefilter-teddy")]
                if let Some(teddy) = teddy {
                    use aho_corasick::Span;
                    let mut pos = offset;
                    loop {
                        diag.search_calls += 1;
                        let start = std::time::Instant::now();
                        let found = teddy.find_in(bytes, Span::from(pos..bytes.len()));
                        diag.search_time += start.elapsed();
                        let m = found?;
                        diag.search_hits += 1;
                        diag.verify_calls += 1;
                        let start = std::time::Instant::now();
                        let v = self.verify_at(forward, bytes, m.start(), scratch);
                        diag.verify_time += start.elapsed();
                        if v.is_some() {
                            return v;
                        }
                        pos = m.start() + 1;
                    }
                }
                let mut pos = offset;
                loop {
                    diag.search_calls += 1;
                    let start = std::time::Instant::now();
                    let found = searcher.find(bytes, pos);
                    diag.search_time += start.elapsed();
                    let j = found?;
                    diag.search_hits += 1;
                    if let Some(s_hi) = j.checked_sub(*prefix_lo) {
                        let s_lo = j.saturating_sub(*prefix_hi).max(offset);
                        let mut s = s_lo;
                        while s <= s_hi {
                            diag.verify_calls += 1;
                            let start = std::time::Instant::now();
                            let m = self.verify_at(forward, bytes, s, scratch);
                            diag.verify_time += start.elapsed();
                            if m.is_some() {
                                return m;
                            }
                            s += 1;
                        }
                    }
                    pos = j + 1;
                }
            }
            Strategy::Scan { unanchored } => {
                diag.verify_calls += 1;
                let start = std::time::Instant::now();
                let m = self.verify_at(unanchored, bytes, offset, scratch);
                diag.verify_time += start.elapsed();
                m
            }
            Strategy::Prefix {
                anchored,
                prefilter,
                ..
            } => {
                let mut pos = offset;
                loop {
                    diag.search_calls += 1;
                    let start = std::time::Instant::now();
                    let cand = prefilter.find_from(bytes, pos);
                    diag.search_time += start.elapsed();
                    let cand = cand?;
                    diag.search_hits += 1;
                    diag.verify_calls += 1;
                    let start = std::time::Instant::now();
                    let m = self.verify_at(anchored, bytes, cand, scratch);
                    diag.verify_time += start.elapsed();
                    if m.is_some() {
                        return m;
                    }
                    pos = cand + 1;
                }
            }
            Strategy::ReverseInner {
                forward,
                reverse,
                literal,
            } => {
                let mut pos = offset;
                loop {
                    diag.search_calls += 1;
                    let start = std::time::Instant::now();
                    let i = literal.find(&bytes[pos..]).map(|k| pos + k);
                    diag.search_time += start.elapsed();
                    let i = i?;
                    diag.search_hits += 1;
                    let start = std::time::Instant::now();
                    let s = reverse::reverse_find_start(reverse, bytes, i, offset);
                    diag.search_time += start.elapsed();
                    if let Some(s) = s {
                        diag.verify_calls += 1;
                        let start = std::time::Instant::now();
                        let m = self.verify_at(forward, bytes, s, scratch);
                        diag.verify_time += start.elapsed();
                        if m.is_some() {
                            return m;
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
            #[cfg(feature = "prefilter-teddy")]
            Strategy::MultiLiteral { .. } => return 3,
            Strategy::Scan { unanchored } => unanchored,
            Strategy::Prefix { anchored, .. } => anchored,
            Strategy::CaseFoldLiteral { forward, .. } => forward,
            #[cfg(feature = "prefilter-teddy")]
            Strategy::AltPrefix { forward, .. } => forward,
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

    /// Test-only: whether the case-fold-literal strategy is driving candidates
    /// with the Teddy prefilter (vs. the `CaseFoldSearcher` fallback). Guards
    /// against a silent fallback regression when `prefilter-teddy` is on.
    #[cfg(all(test, feature = "prefilter-teddy"))]
    pub(crate) fn uses_teddy(&self) -> bool {
        matches!(
            self.strategy,
            Strategy::CaseFoldLiteral { teddy: Some(_), .. }
        )
    }

    /// Test-only: whether this program uses the multi-literal (Teddy
    /// alternation) strategy.
    #[cfg(all(test, feature = "prefilter-teddy"))]
    pub(crate) fn is_multi_literal(&self) -> bool {
        matches!(self.strategy, Strategy::MultiLiteral { .. })
    }

    /// Test-only: whether this program uses the alternation-prefix (Teddy +
    /// automaton verify) strategy.
    #[cfg(all(test, feature = "prefilter-teddy"))]
    pub(crate) fn is_alt_prefix(&self) -> bool {
        matches!(self.strategy, Strategy::AltPrefix { .. })
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
            #[cfg(feature = "prefilter-teddy")]
            Strategy::MultiLiteral { .. } => TdfaStats {
                num_states: 0,
                num_marks: 0,
                total_commands: 0,
                copy_commands: 0,
                currentpos_commands: 0,
            },
            Strategy::Scan { unanchored } => unanchored.stats(),
            Strategy::Prefix { anchored, .. } => anchored.stats(),
            Strategy::CaseFoldLiteral { forward, .. } => forward.stats(),
            #[cfg(feature = "prefilter-teddy")]
            Strategy::AltPrefix { forward, .. } => forward.stats(),
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

#[cfg(feature = "std")]
fn next_utf8_offset(text: &str, offset: usize) -> usize {
    if offset >= text.len() {
        return text.len() + 1;
    }
    let mut next = offset + 1;
    while next < text.len() && !text.is_char_boundary(next) {
        next += 1;
    }
    next
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

    /// See [`TdfaProgram::diagnostics`].
    #[cfg(feature = "std")]
    pub fn diagnostics(&self, text: &str) -> TdfaDiagnostics {
        self.0.diagnostics(text)
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
