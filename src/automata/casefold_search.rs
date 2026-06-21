//! Case-insensitive substring search for an ASCII "fold-clean" run, used as the
//! prefilter for the case-insensitive literal strategy.
//!
//! The run is a sequence of per-character ASCII byte-sets (e.g. `herlock` icase →
//! `[{h,H},{e,E},{r,R},{l,L},{o,O},{c,C},{k,K}]`). We find positions where every
//! byte is in its set, **packed-pair** style: anchor on the two *rarest*
//! positions (lowest [`byte_frequencies`] rank), so candidate hits are sparse,
//! then verify the whole run.
//!
//! The rare anchor is scanned with `memchr`'s SIMD (`memchr`/`memchr2`/`memchr3`)
//! — portable to call, no intrinsics here — then the second anchor is an O(1)
//! membership check and the full run is verified. A custom SIMD packed-pair (AND
//! both anchor masks in-register) can replace `scan_anchor` later behind the same
//! interface.
//!
//! All bytes are ASCII (`< 0x80`): a non-ASCII haystack byte can never be in a
//! set, so it's skipped for free, and any all-ASCII match is on a UTF-8
//! codepoint boundary.

use crate::automata::byte_frequencies::rank;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use smallvec::SmallVec;

/// A per-position ASCII case-set (1–4 bytes, e.g. `{h,H}`).
type ByteSet = SmallVec<[u8; 4]>;

/// Rank threshold below which an anchor byte is "rare enough" that scanning for
/// it alone with `memchr` (few stops) beats the uniform SWAR packed-pair. Ranks
/// run 0..=255 (higher = more common); e.g. Sherlock's `k` ≈ 180 (use memchr),
/// Holmes'/the's letters ≈ 230+ (use the pair).
const RARE_RANK: u8 = 200;

#[derive(Debug, Clone)]
pub(crate) struct CaseFoldSearcher {
    /// One ASCII byte-set per run position; a match needs every byte in its set.
    sets: Vec<ByteSet>,
    /// The two anchor offsets, ordered `a1 < a2` (for the packed-pair scan).
    a1: usize,
    a2: usize,
    /// When `Some(scan)`, the rarest anchor is rare enough to drive the scan
    /// with `memchr` alone (and check the other anchor per hit); else `None`
    /// uses the SWAR packed-pair (AND both anchor masks).
    memchr_scan: Option<usize>,
}

/// First byte of `hay` that is in `set` (ASCII, 1–4 bytes), via memchr SIMD.
#[inline]
fn find_byteset(set: &[u8], hay: &[u8]) -> Option<usize> {
    match *set {
        [a] => memchr::memchr(a, hay),
        [a, b] => memchr::memchr2(a, b, hay),
        [a, b, c] => memchr::memchr3(a, b, c, hay),
        _ => hay.iter().position(|x| set.contains(x)),
    }
}

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGHS: u64 = 0x8080_8080_8080_8080;

/// SWAR: high bit set in each byte-lane of `word` that equals `c` (the classic
/// "has-zero" test applied to `word ^ broadcast(c)`).
#[inline]
fn eq_mask(word: u64, c: u8) -> u64 {
    let x = word ^ (ONES.wrapping_mul(c as u64));
    x.wrapping_sub(ONES) & !x & HIGHS
}

/// High bit set in each lane of `word` that is a member of `set` (1–4 bytes).
#[inline]
fn set_mask(set: &[u8], word: u64) -> u64 {
    let mut m = 0;
    for &c in set {
        m |= eq_mask(word, c);
    }
    m
}

impl CaseFoldSearcher {
    /// Build a searcher for `sets` (the clean run). Returns `None` if the run is
    /// shorter than 2 (need two anchors). Anchors are the two positions whose
    /// **most common** byte is rarest overall.
    pub(crate) fn new(sets: Vec<ByteSet>) -> Option<Self> {
        if sets.len() < 2 || sets.iter().any(|s| s.is_empty()) {
            return None;
        }
        // Score a position by the *max* rank over its case-set — a position is
        // only as rare as its commonest byte. Lower score = better anchor.
        let score = |s: &ByteSet| s.iter().map(|&b| rank(b)).max().unwrap_or(u8::MAX);
        let mut rare = 0;
        for i in 1..sets.len() {
            if score(&sets[i]) < score(&sets[rare]) {
                rare = i;
            }
        }
        let mut rare2 = if rare == 0 { 1 } else { 0 };
        for i in 0..sets.len() {
            if i != rare && score(&sets[i]) < score(&sets[rare2]) {
                rare2 = i;
            }
        }
        // If the rarest anchor is rare enough, scan for it alone with memchr.
        let memchr_scan = (score(&sets[rare]) < RARE_RANK).then_some(rare);
        Some(Self {
            sets,
            a1: rare.min(rare2),
            a2: rare.max(rare2),
            memchr_scan,
        })
    }

    /// Verify the whole run matches at run-start `q` (caller ensures
    /// `q + run_len <= haystack.len()`).
    #[inline]
    fn verify(&self, haystack: &[u8], q: usize) -> bool {
        self.sets
            .iter()
            .enumerate()
            .all(|(k, set)| set.contains(&haystack[q + k]))
    }

    /// Find the start offset of the leftmost run occurrence at or after `from`,
    /// or `None`. Dispatches to memchr-single-anchor (rare anchor) or the SWAR
    /// packed-pair (common anchors).
    pub(crate) fn find(&self, haystack: &[u8], from: usize) -> Option<usize> {
        let run_len = self.sets.len();
        let n = haystack.len();
        if from + run_len > n {
            return None;
        }
        match self.memchr_scan {
            Some(scan) => self.find_memchr(haystack, from, scan),
            None => self.find_packed(haystack, from),
        }
    }

    /// memchr-driven scan on the rare anchor at run-offset `scan`: jump to each
    /// of its bytes (SIMD), then verify the whole run there.
    fn find_memchr(&self, haystack: &[u8], from: usize, scan: usize) -> Option<usize> {
        let n = haystack.len();
        let set = &self.sets[scan];
        let mut cursor = from + scan; // the scan anchor's byte position for runs ≥ from
        loop {
            if cursor >= n {
                return None;
            }
            let hit = find_byteset(set, &haystack[cursor..])? + cursor;
            let q = hit - scan; // run start; hit ≥ from + scan ≥ scan
            if q + self.sets.len() <= n && self.verify(haystack, q) {
                return Some(q);
            }
            cursor = hit + 1;
        }
    }

    /// SWAR packed-pair: scan 8 bytes per step, AND the two anchor masks
    /// (anchors `o1 < o2`, `delta` apart) so only true pairs surface, then verify.
    fn find_packed(&self, haystack: &[u8], from: usize) -> Option<usize> {
        let run_len = self.sets.len();
        let n = haystack.len();
        let (o1, o2) = (self.a1, self.a2);
        let delta = o2 - o1;
        let (set1, set2) = (&self.sets[o1], &self.sets[o2]);

        // `a1`'s byte position ranges over `[from + o1, n - run_len + o1]`.
        let start_a1 = from + o1;
        let end_a1 = n - run_len + o1; // inclusive

        let mut i = start_a1;
        // SWAR while both 8-byte loads (A at i, B at i+delta) are in bounds.
        while i <= end_a1 && i + delta + 8 <= n {
            let wa = u64::from_le_bytes(haystack[i..i + 8].try_into().unwrap());
            let wb = u64::from_le_bytes(haystack[i + delta..i + delta + 8].try_into().unwrap());
            let mut cand = set_mask(set1, wa) & set_mask(set2, wb);
            while cand != 0 {
                let lane = (cand.trailing_zeros() / 8) as usize;
                let a1pos = i + lane;
                if a1pos <= end_a1 && self.verify(haystack, a1pos - o1) {
                    return Some(a1pos - o1);
                }
                cand &= cand - 1; // clear lowest set lane (ascending → leftmost)
            }
            i += 8;
        }
        // Scalar tail.
        while i <= end_a1 {
            if set1.contains(&haystack[i])
                && set2.contains(&haystack[i + delta])
                && self.verify(haystack, i - o1)
            {
                return Some(i - o1);
            }
            i += 1;
        }
        None
    }
}
