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
        // A multi-byte `ByteBracket` start predicate (e.g. `[0-9]`) has no SIMD
        // `memchr` and scans scalar; a required interior/suffix literal (single
        // rare byte via `memchr`, or `memmem`) beats it. `ByteSeq`/`ByteSet1..3`
        // *do* use SIMD memchr, so they stay ahead of the reverse-inner search.
        let weak_pred = !should_prefilter(&pred) || matches!(pred, StartPredicate::ByteBracket(_));
        if !weak_pred {
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

        // A usable (if scalar) byte-class predicate still beats a full scan.
        if should_prefilter(&pred) {
            return Self::build_prefix(re, pred);
        }

        let nfa = Nfa::try_from_unanchored(re)?;
        let mut unanchored = Tdfa::try_from(&nfa)?;
        unanchored.optimize();
        let group_names = unanchored.group_names().to_vec().into_boxed_slice();
        Ok(Self::from_parts(Strategy::Scan { unanchored }, group_names))
    }

    /// Build a [`Strategy::Prefix`] program: an anchored verify automaton driven
    /// by the start-predicate prefilter. For an exact literal prefix it also
    /// precomputes a warm start that skips re-scanning the literal through the
    /// automaton (a no-op for byte-set/bracket prefixes, which have no fixed
    /// literal). The anchored automaton matches only at the offset handed to
    /// `execute`, so a candidate that fails dies fast and we advance.
    fn build_prefix(re: &ir::Regex, pred: StartPredicate) -> Result<Self, BuildError> {
        let nfa = Nfa::try_from(re)?;
        let mut anchored = Tdfa::try_from(&nfa)?;
        anchored.optimize();
        let skip = match &pred {
            StartPredicate::ByteSeq(finder) => {
                tdfa_backend::compute_prefix_skip(&anchored, finder.needle())
            }
            _ => None,
        };
        let group_names = anchored.group_names().to_vec().into_boxed_slice();
        Ok(Self::from_parts(
            Strategy::Prefix {
                anchored,
                prefilter: pred,
                skip,
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
        let automaton = match &self.strategy {
            Strategy::Prefix { anchored, .. } => Some(anchored),
            Strategy::CaseFoldLiteral { forward, .. } => Some(forward),
            #[cfg(feature = "prefilter-teddy")]
            Strategy::AltPrefix { forward, .. } => Some(forward),
            Strategy::ReverseInner { forward, .. } => Some(forward),
            // The unanchored single-pass scan: the JIT's capture tier handles
            // its `.*?`-stamped start.
            Strategy::Scan { unanchored } => Some(unanchored),
            // Whole-literal / multi-literal need no automaton.
            Strategy::WholeLiteral { .. } => None,
            #[cfg(feature = "prefilter-teddy")]
            Strategy::MultiLiteral { .. } => None,
        };
        if let Some(tdfa) = automaton {
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
