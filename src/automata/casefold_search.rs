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

#[cfg(not(target_arch = "aarch64"))]
const ONES: u64 = 0x0101_0101_0101_0101;
#[cfg(not(target_arch = "aarch64"))]
const HIGHS: u64 = 0x8080_8080_8080_8080;

/// SWAR: high bit set in each byte-lane of `word` that equals `c` (the classic
/// "has-zero" test applied to `word ^ broadcast(c)`).
#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn eq_mask(word: u64, c: u8) -> u64 {
    let x = word ^ (ONES.wrapping_mul(c as u64));
    x.wrapping_sub(ONES) & !x & HIGHS
}

/// High bit set in each lane of `word` that is a member of `set` (1–4 bytes).
#[cfg(not(target_arch = "aarch64"))]
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

    /// NEON packed-pair candidate mask for the 16 positions starting at `i`:
    /// load 16 bytes at `i` (anchor 1) and at `i + delta` (anchor 2), match each
    /// against its set, AND, then pack the per-byte 0x00/0xFF result to 4 bits
    /// per byte via `vshrn`. Caller must ensure `i + delta + 16 <= haystack.len()`
    /// and masks the result with `0x1111…` to get one bit per matched lane.
    ///
    /// # Safety
    /// `i + delta + 16 <= haystack.len()`. NEON is baseline on aarch64.
    #[cfg(target_arch = "aarch64")]
    #[inline]
    unsafe fn neon_pair_bits(
        &self,
        haystack: &[u8],
        i: usize,
        delta: usize,
        set1: &[u8],
        set2: &[u8],
    ) -> u64 {
        use core::arch::aarch64::*;
        // SAFETY: caller guarantees `i + delta + 16 <= haystack.len()`, so both
        // 16-byte loads are in bounds; NEON is baseline on aarch64.
        unsafe {
            let va = vld1q_u8(haystack.as_ptr().add(i));
            let vb = vld1q_u8(haystack.as_ptr().add(i + delta));
            let mut ma = vdupq_n_u8(0);
            for &c in set1 {
                ma = vorrq_u8(ma, vceqq_u8(va, vdupq_n_u8(c)));
            }
            let mut mb = vdupq_n_u8(0);
            for &c in set2 {
                mb = vorrq_u8(mb, vceqq_u8(vb, vdupq_n_u8(c)));
            }
            let cand = vandq_u8(ma, mb);
            let narrowed = vshrn_n_u16::<4>(vreinterpretq_u16_u8(cand));
            vget_lane_u64::<0>(vreinterpret_u64_u8(narrowed))
        }
    }

    /// Packed-pair: AND the two anchor masks (anchors `o1 < o2`, `delta` apart)
    /// over a vector at a time so only true pairs surface, then verify each. Uses
    /// NEON (16 bytes/step) on aarch64, SWAR (8 bytes/step) elsewhere; identical
    /// results either way.
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

        // For each candidate lane bit set in `bits` (one per matched a1 position
        // starting at base `i`), verify and return the leftmost true match.
        macro_rules! drain_lanes {
            ($bits:expr, $stride:expr) => {{
                let mut bits = $bits;
                while bits != 0 {
                    let lane = (bits.trailing_zeros() as usize) / $stride;
                    let a1pos = i + lane;
                    if a1pos <= end_a1 && self.verify(haystack, a1pos - o1) {
                        return Some(a1pos - o1);
                    }
                    bits &= bits - 1; // clear lowest set bit (ascending → leftmost)
                }
            }};
        }

        // NEON: 16 candidate positions per step. NEON is baseline on aarch64, so
        // no runtime detection. Each lane of the compare is 0x00/0xFF; `vshrn` by
        // 4 packs it to 4 bits per byte, and masking `0x1111…` leaves one bit per
        // matched lane at bit `4*lane`.
        #[cfg(target_arch = "aarch64")]
        while i <= end_a1 && i + delta + 16 <= n {
            let bits = unsafe { self.neon_pair_bits(haystack, i, delta, set1, set2) }
                & 0x1111_1111_1111_1111u64;
            drain_lanes!(bits, 4);
            i += 16;
        }

        // SWAR: 8 candidate positions per step; one 0x80 bit per matched lane at
        // bit `8*lane + 7`.
        #[cfg(not(target_arch = "aarch64"))]
        while i <= end_a1 && i + delta + 8 <= n {
            let wa = u64::from_le_bytes(haystack[i..i + 8].try_into().unwrap());
            let wb = u64::from_le_bytes(haystack[i + delta..i + delta + 8].try_into().unwrap());
            let bits = set_mask(set1, wa) & set_mask(set2, wb);
            drain_lanes!(bits, 8);
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
