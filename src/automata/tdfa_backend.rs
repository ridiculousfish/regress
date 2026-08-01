//! TDFA execution backend.
//!
//! The hot path applies each transition's mark update as a short, precompiled
//! sequence of in-place moves (`buf[dst] = buf[src]`): the determinizer turns
//! every transition's `TagCommandList` into a [`MoveOp`](crate::automata::tdfa::MoveOp)
//! list over a mark file laid out as `[marks[0..num_marks], clear, current_pos,
//! scratch]`, ordered (with a `scratch` lane to break copy cycles) so the
//! simultaneous-assignment semantics hold while writing in place. Only the lanes
//! that change are touched — no width-proportional copy, no double buffer.
//!
//! Marks are `usize` byte offsets; `usize::MAX` is the NO_MATCH sentinel.

use crate::automata::anchors::boundary_signature;
use crate::automata::dfa::{DEAD_STATE, Dfa};
use crate::automata::nfa::FULL_MATCH_START;
use crate::automata::nfa_backend::NfaMatch;
use crate::automata::tdfa::{
    EXEC_ACCEPT_FLAG, EXEC_STATE_MASK, FinalCommand, MarkValue, MoveOp, StateGuards,
    TDFA_DEAD_STATE, TagCommand, Tdfa, csr_iat,
    ACCEL_NONE, PosStampLoopFlat, ScanFast, ScanSkipFlat, SCAN_MAX_RANGES,
    NO_PRUNE, TF_ACCEPT, TF_ACCEPTS, TF_FALLBACK, TF_SWITCHES,
};
use crate::insn::StartPredicate;
use crate::util::DebugCheckIndex;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ops::Range;
extern crate memchr;
use smallvec::SmallVec;

/// SSE2 scan: advance `pos` while each byte is in the union of the given
/// (lo, hi) ranges.  Stops at the first byte outside all ranges or at
/// `input.len()`.  Handles the 16-byte SIMD loop; callers append a scalar
/// tail for the remaining ≤15 bytes.
#[cfg(all(target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
#[inline(always)]
fn scan_ascii_ranges_sse2(
    input: &[u8],
    mut pos: usize,
    count: u8,
    pairs: &[u8; 2 * SCAN_MAX_RANGES],
    bm0: u64,
    bm1: u64,
) -> usize {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is the x86-64 baseline; all accesses are within `input`.
    unsafe {
        let zero = _mm_setzero_si128();
        while pos + 16 <= input.len() {
            let v = _mm_loadu_si128(input.as_ptr().add(pos) as *const __m128i);
            // Build membership mask: 0xFF per lane if in any range, else 0x00.
            let mut member = _mm_setzero_si128();
            for i in 0..count as usize {
                let lo = pairs[2 * i];
                let hi = pairs[2 * i + 1];
                // range_mask = 0xFF where lo <= v <= hi.
                // psubusb(v, hi)==0  ↔  v<=hi (unsigned saturating subtract)
                // psubusb(lo, v)==0  ↔  v>=lo
                let rm = if lo == hi {
                    _mm_cmpeq_epi8(v, _mm_set1_epi8(lo as i8))
                } else if lo == 0 {
                    _mm_cmpeq_epi8(_mm_subs_epu8(v, _mm_set1_epi8(hi as i8)), zero)
                } else {
                    let hi_ok = _mm_cmpeq_epi8(_mm_subs_epu8(v, _mm_set1_epi8(hi as i8)), zero);
                    let lo_ok = _mm_cmpeq_epi8(_mm_subs_epu8(_mm_set1_epi8(lo as i8), v), zero);
                    _mm_and_si128(hi_ok, lo_ok)
                };
                member = _mm_or_si128(member, rm);
            }
            // pmovmskb: bit i set ↔ lane i MSB set ↔ member[i]==0xFF (in set).
            let bits = _mm_movemask_epi8(member) as u32;
            if bits != 0xFFFF {
                // First lane not in set: lowest clear bit in bits.
                return pos + (bits ^ 0xFFFF).trailing_zeros() as usize;
            }
            pos += 16;
        }
    }
    // Scalar tail for the final partial 16-byte chunk (0–15 bytes).  Using
    // bm0/bm1 avoids the caller re-entering a scalar loop on the SIMD exit
    // position (one wasted iteration per match when SIMD finds the stop byte
    // inside a full chunk).
    while pos < input.len() {
        let b = input[pos] as usize;
        if b >= 0x80 { break; }
        let word = if b < 0x40 { bm0 } else { bm1 };
        if (word >> (b & 63)) & 1 == 0 { break; }
        pos += 1;
    }
    pos
}

/// Complement of [`scan_ascii_ranges_sse2`]: advances `pos` to the first byte
/// that IS in any of the `count` (lo, hi) ranges (scan continues while NOT in
/// ranges).  Non-ASCII bytes (≥ 0x80) are never in any ASCII range, so they
/// are always skipped.
#[cfg(all(target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
#[inline(always)]
fn scan_ascii_ranges_stop_sse2(
    input: &[u8],
    mut pos: usize,
    count: u8,
    pairs: &[u8; 2 * SCAN_MAX_RANGES],
    bm0: u64,
    bm1: u64,
) -> usize {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is the x86-64 baseline; all accesses are within `input`.
    unsafe {
        let zero = _mm_setzero_si128();
        while pos + 16 <= input.len() {
            let v = _mm_loadu_si128(input.as_ptr().add(pos) as *const __m128i);
            let mut member = _mm_setzero_si128();
            for i in 0..count as usize {
                let lo = pairs[2 * i];
                let hi = pairs[2 * i + 1];
                let rm = if lo == hi {
                    _mm_cmpeq_epi8(v, _mm_set1_epi8(lo as i8))
                } else if lo == 0 {
                    _mm_cmpeq_epi8(_mm_subs_epu8(v, _mm_set1_epi8(hi as i8)), zero)
                } else {
                    let hi_ok = _mm_cmpeq_epi8(_mm_subs_epu8(v, _mm_set1_epi8(hi as i8)), zero);
                    let lo_ok = _mm_cmpeq_epi8(_mm_subs_epu8(_mm_set1_epi8(lo as i8), v), zero);
                    _mm_and_si128(hi_ok, lo_ok)
                };
                member = _mm_or_si128(member, rm);
            }
            // Stop at the first lane where a stop byte was found (member bit set).
            let bits = _mm_movemask_epi8(member) as u32 & 0xFFFF;
            if bits != 0 {
                return pos + bits.trailing_zeros() as usize;
            }
            pos += 16;
        }
    }
    // Scalar tail for the final partial 16-byte chunk.  Non-ASCII bytes are
    // self-loop bytes (continue); ASCII stop bytes (bit==0) end the scan.
    while pos < input.len() {
        let b = input[pos] as usize;
        if b < 0x80 {
            let word = if b < 0x40 { bm0 } else { bm1 };
            if (word >> (b & 63)) & 1 == 0 { break; }
        }
        pos += 1;
    }
    pos
}

/// Advance `pos` through `input` while bytes remain in the self-loop set
/// described by `fast` (and, for the `Bitmap` variant, `byte_bitmap`).
/// Returns the first position outside the set, or `input.len()` if the entire
/// remaining input is in-set. `pub(crate)` so `codegen_rt.rs` can wrap it for
/// the AoT tier's peeled self-loops (`regex!`-generated code calls the
/// wrapper via `__codegen::scan_fast`) — the exact same accelerated scan the
/// interpreter and table tier already use.
pub(crate) fn scan_fast(fast: &ScanFast, byte_bitmap: &[u64; 4], input: &[u8], pos: usize) -> usize {
    match fast {
        ScanFast::Memchr { count, bytes } => match count {
            0 => input.len(),
            1 => memchr::memchr(bytes[0], &input[pos..])
                .map(|i| pos + i)
                .unwrap_or(input.len()),
            2 => memchr::memchr2(bytes[0], bytes[1], &input[pos..])
                .map(|i| pos + i)
                .unwrap_or(input.len()),
            _ => memchr::memchr3(bytes[0], bytes[1], bytes[2], &input[pos..])
                .map(|i| pos + i)
                .unwrap_or(input.len()),
        },
        ScanFast::AsciiBarrier { count, bytes } => {
            let b0 = bytes[0];
            let b1 = bytes[1];
            let end = match count {
                0 => input[pos..].iter().position(|&b| b >= 0x80),
                1 => input[pos..].iter().position(|&b| b >= 0x80 || b == b0),
                _ => input[pos..].iter().position(|&b| b >= 0x80 || b == b0 || b == b1),
            };
            end.map(|i| pos + i).unwrap_or(input.len())
        }
        ScanFast::AsciiRanges { count, pairs, bm0, bm1 } => {
            #[cfg(all(target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
            let p = scan_ascii_ranges_sse2(input, pos, *count, pairs, *bm0, *bm1);
            #[cfg(not(all(target_arch = "x86_64", not(feature = "prohibit-unsafe"))))]
            let p = {
                let mut p = pos;
                while p < input.len() {
                    let b = *input.iat(p) as usize;
                    if b >= 0x80 { break; }
                    let word = if b < 0x40 { *bm0 } else { *bm1 };
                    if (word >> (b & 63)) & 1 == 0 { break; }
                    p += 1;
                }
                p
            };
            p
        }
        ScanFast::BitmapAscii { bm0, bm1 } => {
            let mut p = pos;
            while p < input.len() {
                let b = *input.iat(p) as usize;
                if b >= 0x80 { break; }
                let word = if b < 0x40 { *bm0 } else { *bm1 };
                if (word >> (b & 63)) & 1 == 0 { break; }
                p += 1;
            }
            p
        }
        ScanFast::AsciiRangesStop { count, pairs, bm0, bm1 } => {
            #[cfg(all(target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
            let p = scan_ascii_ranges_stop_sse2(input, pos, *count, pairs, *bm0, *bm1);
            #[cfg(not(all(target_arch = "x86_64", not(feature = "prohibit-unsafe"))))]
            let p = {
                let mut p = pos;
                while p < input.len() {
                    let b = *input.iat(p) as usize;
                    if b < 0x80 {
                        let word = if b < 0x40 { *bm0 } else { *bm1 };
                        if (word >> (b & 63)) & 1 == 0 { break; }
                    }
                    p += 1;
                }
                p
            };
            p
        }
        ScanFast::Bitmap => {
            let mut p = pos;
            while p < input.len() {
                let b = *input.iat(p) as usize;
                if (byte_bitmap[b >> 6] >> (b & 63)) & 1 == 0 { break; }
                p += 1;
            }
            p
        }
    }
}

/// Resolve an accelerator's `(offset, len)` stamp range in the shared arena.
#[inline(always)]
fn stamp_slice(arena: &[u16], stamp: (u32, u32)) -> &[u16] {
    &arena[stamp.0 as usize..(stamp.0 + stamp.1) as usize]
}

/// Run the pos-stamp PSL scan from `start`; delegates to [`scan_fast`].
#[inline(always)]
fn scan_pos_stamp(psl: &PosStampLoopFlat, input: &[u8], start: usize) -> usize {
    scan_fast(&psl.fast, &psl.byte_bitmap, input, start)
}

/// Compile-time switches that let [`execute_generic`] drop cold sites it can
/// statically prove the current automaton can't hit. Each realized combination
/// is one monomorphization; the dispatcher in [`execute`] picks one per scan
/// from the [`Tdfa`]'s contents, so the guarded branches const-fold away and
/// the hot loop carries no runtime check.
pub(crate) trait TdfaExecConfig {
    /// Some state carries a zero-width guard that must be evaluated *per byte*:
    /// any `switch` (multiline `^`, `\b`/`\B`) or any `accept` that can fire
    /// mid-input (multiline `$`). When false, the entry + per-byte `apply_guards`
    /// calls are not emitted, and the capture-free fast loop is eligible.
    /// Non-multiline `$` accepts do NOT set this — they fire only at EOI, handled
    /// by the once-per-run pass via the runtime `Tdfa::has_eoi_accepts` check.
    const HAS_PERBYTE_GUARDS: bool;
    /// The TDFA has compiled `MoveOp` sequences (i.e. `Tdfa::has_moves()` is
    /// true). When true the executor uses the fast `transition_moves` table;
    /// when false it falls back to the scalar command interpreter. Hoisting this
    /// into a const eliminates the per-byte `use_moves` branch and its register
    /// spill in the hot byte loop.
    const HAS_MOVES: bool;
    /// Precomputed `skip_marks = !has_captures && start_fixed`. When true the
    /// mark file is not maintained (capture-free fast path, usually followed by
    /// the exec-transitions loop). When false (capture patterns) every byte-by-byte
    /// transition applies its `MoveOp` list. Making this const eliminates the
    /// per-byte `if !C::SKIP_MARKS` branch and stack spill.
    const SKIP_MARKS: bool;
}

/// Config marker parameterized directly on its flag bits, so adding a flag is a
/// new `const` param rather than a new named type per combination.
pub(crate) struct ExecConfig<const HAS_PERBYTE_GUARDS: bool, const HAS_MOVES: bool, const SKIP_MARKS: bool>;

impl<const G: bool, const M: bool, const SM: bool> TdfaExecConfig for ExecConfig<G, M, SM> {
    const HAS_PERBYTE_GUARDS: bool = G;
    const HAS_MOVES: bool = M;
    const SKIP_MARKS: bool = SM;
}

/// A recorded accept candidate: `(match end, finalization commands, match
/// start)`. The finals slice borrows the `Tdfa` (per-state or per-conditional),
/// so no clone is needed; the marks snapshot lives in a separate reused buffer
/// updated only when this candidate wins (see `consider_accept`). The best
/// candidate (leftmost; smallest `match start`) drives `finalize` at scan end.
type LastAccept<'a> = Option<(usize, &'a [FinalCommand], usize)>;

/// Anchored match against a (non-tagged) DFA: returns true if `input` matches
/// from the start. Used by DFA correctness tests; production paths go through
/// `execute` (TDFA) instead.
pub fn execute_dfa(dfa: &Dfa, input: &[u8]) -> bool {
    let mut state = dfa.start();
    let byte_to_class = dfa.byte_to_class();
    let transitions = dfa.transitions();
    let accepting = dfa.accepting();
    let num_classes = dfa.num_classes();
    for &byte in input {
        if state == DEAD_STATE {
            return false;
        }
        let class = byte_to_class[byte as usize] as usize;
        state = transitions[state as usize * num_classes + class];
    }
    accepting[state as usize]
}

/// Borrowed view of every table and flag the executor reads — the abstraction
/// that lets one loop drive both a heap-built [`Tdfa`] and the AoT table
/// tier's `static` tables. Implementors must uphold the same invariants
/// `Tdfa` does (table shapes and premultiplication, CSR range validity, flag
/// consistency); the executor's `debug_assert`s cross-check in debug builds.
pub(crate) trait TdfaTables {
    fn num_classes(&self) -> usize;
    fn num_marks(&self) -> usize;
    fn num_states(&self) -> usize;
    fn has_captures(&self) -> bool;
    fn has_moves(&self) -> bool;
    fn has_perbyte_guards(&self) -> bool;
    fn has_eoi_accepts(&self) -> bool;
    fn word_icase(&self) -> bool;
    fn start_fixed(&self) -> bool;
    fn start(&self, start: usize) -> u32;
    fn byte_to_class(&self) -> &[u8; 256];
    fn transitions(&self) -> &[u32];
    fn trans_flags(&self) -> &[u8];
    fn exec_transitions(&self) -> &[u32];
    fn accepting(&self) -> &[bool];
    fn accept_fallback(&self) -> &[bool];
    /// The compiled move table as raw `(cells, arena)` CSR slices.
    fn moves_raw(&self) -> (&[u32], &[MoveOp]);
    /// Scalar-fallback command lists; may be empty when `has_moves()`.
    fn transition_commands(&self, idx: usize) -> &[TagCommand];
    fn entry_moves(&self, start: usize) -> &[MoveOp];
    fn entry_commands(&self, start: usize) -> &[TagCommand];
    fn finals(&self, state: u32) -> &[FinalCommand];
    /// Zero-width guards; `None` for guard-free states. Table-tier
    /// implementations without guard support return `None` unconditionally
    /// (their `has_perbyte_guards`/`has_eoi_accepts` are false).
    fn guards(&self, state: u32) -> Option<&StateGuards>;
    fn psl_tables(&self) -> (&[u32], &[PosStampLoopFlat]);
    fn scan_skip_tables(&self) -> (&[u32], &[ScanSkipFlat]);
    fn stamp_arena(&self) -> &[u16];
    fn psl_ascii_bms(&self) -> &[u64];
}

impl TdfaTables for Tdfa {
    fn num_classes(&self) -> usize { Tdfa::num_classes(self) }
    fn num_marks(&self) -> usize { Tdfa::num_marks(self) }
    fn num_states(&self) -> usize { Tdfa::num_states(self) }
    fn has_captures(&self) -> bool { Tdfa::has_captures(self) }
    fn has_moves(&self) -> bool { Tdfa::has_moves(self) }
    fn has_perbyte_guards(&self) -> bool { Tdfa::has_perbyte_guards(self) }
    fn has_eoi_accepts(&self) -> bool { Tdfa::has_eoi_accepts(self) }
    fn word_icase(&self) -> bool { Tdfa::word_icase(self) }
    fn start_fixed(&self) -> bool { Tdfa::start_fixed(self) }
    fn start(&self, start: usize) -> u32 { Tdfa::start(self, start) }
    fn byte_to_class(&self) -> &[u8; 256] { Tdfa::byte_to_class(self) }
    fn transitions(&self) -> &[u32] { Tdfa::transitions(self) }
    fn trans_flags(&self) -> &[u8] { Tdfa::trans_flags(self) }
    fn exec_transitions(&self) -> &[u32] { Tdfa::exec_transitions(self) }
    fn accepting(&self) -> &[bool] { Tdfa::accepting(self) }
    fn accept_fallback(&self) -> &[bool] { Tdfa::accept_fallback(self) }
    fn moves_raw(&self) -> (&[u32], &[MoveOp]) { self.transition_moves().as_raw() }
    fn transition_commands(&self, idx: usize) -> &[TagCommand] { Tdfa::transition_commands(self, idx) }
    fn entry_moves(&self, start: usize) -> &[MoveOp] { Tdfa::entry_moves(self, start) }
    fn entry_commands(&self, start: usize) -> &[TagCommand] { Tdfa::entry_commands(self, start) }
    fn finals(&self, state: u32) -> &[FinalCommand] { Tdfa::finals(self, state) }
    fn guards(&self, state: u32) -> Option<&StateGuards> { Tdfa::guards(self, state) }
    fn psl_tables(&self) -> (&[u32], &[PosStampLoopFlat]) { Tdfa::psl_tables(self) }
    fn scan_skip_tables(&self) -> (&[u32], &[ScanSkipFlat]) { Tdfa::scan_skip_tables(self) }
    fn stamp_arena(&self) -> &[u16] { Tdfa::stamp_arena(self) }
    fn psl_ascii_bms(&self) -> &[u64] { Tdfa::psl_ascii_bms(self) }
}

/// `num_marks + 3`: the mark-file width (real marks, then `clear`,
/// `current_pos`, `scratch`). The size a [`Scratch`] must be built with.
pub(crate) fn mark_file_width<T: TdfaTables>(tdfa: &T) -> usize {
    tdfa.num_marks() + 3
}

/// Pick the config-flag monomorphization (`HAS_PERBYTE_GUARDS`, `HAS_MOVES`,
/// `SKIP_MARKS`) from the automaton's contents and run one anchored attempt.
/// The flag checks are once per attempt; the const generics drop the per-byte
/// branches inside `run_anchored`.
fn run_anchored_dyn<T: TdfaTables>(
    tdfa: &T,
    input: &[u8],
    start: usize,
    scratch: &mut Scratch,
    warm: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    let skip_marks = !tdfa.has_captures() && tdfa.start_fixed();
    match (tdfa.has_perbyte_guards(), tdfa.has_moves(), skip_marks) {
        (false, true,  false) => run_anchored::<ExecConfig<false, true, false>, T>(tdfa, input, start, scratch, warm),
        (false, true,  true)  => run_anchored::<ExecConfig<false, true, true>, T>(tdfa, input, start, scratch, warm),
        (false, false, false) => run_anchored::<ExecConfig<false, false, false>, T>(tdfa, input, start, scratch, warm),
        (false, false, true)  => run_anchored::<ExecConfig<false, false, true>, T>(tdfa, input, start, scratch, warm),
        (true,  true,  false) => run_anchored::<ExecConfig<true, true, false>, T>(tdfa, input, start, scratch, warm),
        (true,  true,  true)  => run_anchored::<ExecConfig<true, true, true>, T>(tdfa, input, start, scratch, warm),
        (true,  false, false) => run_anchored::<ExecConfig<true, false, false>, T>(tdfa, input, start, scratch, warm),
        (true,  false, true)  => run_anchored::<ExecConfig<true, false, true>, T>(tdfa, input, start, scratch, warm),
    }
}

/// The prefilter loop: skip to each candidate `pred` allows (at or after
/// `start`) and run the anchored automaton there, returning the first (leftmost)
/// match. The `scratch` is reused across every candidate; `skip`, when set,
/// warm-starts each attempt past the matched literal (see [`PrefixSkip`]).
fn run_prefiltered_dyn<T: TdfaTables>(
    tdfa: &T,
    input: &[u8],
    start: usize,
    pred: &StartPredicate,
    scratch: &mut Scratch,
    skip: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    let mut pos = start;
    loop {
        let cand = pred.find_from(input, pos)?;
        if let Some(m) = run_anchored_dyn(tdfa, input, cand, scratch, skip) {
            return Some(m);
        }
        pos = cand + 1;
    }
}

/// Execute the TDFA against `input`, allocating fresh buffers. Returns the first
/// match (range + captures) or `None`. Used by tests and one-shot callers; the
/// match-iteration hot path uses [`execute_reuse`] with a caller-owned scratch.
pub fn execute(tdfa: &Tdfa, input: &[u8], start: usize) -> Option<NfaMatch> {
    let mut scratch = Scratch::new(mark_file_width(tdfa), tdfa.num_capture_groups());
    let m = run_anchored_dyn(tdfa, input, start, &mut scratch, None)?;
    // Materialize captures from norm_buf for test/one-shot callers.
    let captures = scratch
        .norm_buf
        .chunks_exact(2)
        .map(|c| if c[0] == usize::MAX { None } else { Some(c[0]..c[1]) })
        .collect();
    Some(NfaMatch { range: m.range, captures })
}

/// Like [`execute`], but reuses the caller-owned `scratch` (sized to
/// [`mark_file_width`]) instead of allocating — so a `find_iter` over many
/// matches stays allocation-free per match.
pub(crate) fn execute_reuse<T: TdfaTables>(
    tdfa: &T,
    input: &[u8],
    start: usize,
    scratch: &mut Scratch,
) -> Option<NfaMatch> {
    run_anchored_dyn(tdfa, input, start, scratch, None)
}

/// Like [`execute_reuse`], but warm-starts past a prefilter-matched prefix when
/// `skip` is set (the literal/byte-class the prefilter already confirmed at
/// `start`). The match still begins at `start`; only the byte loop resumes at
/// `start + skip.len` from `skip.post_state`. `skip == None` is identical to
/// [`execute_reuse`].
pub(crate) fn execute_reuse_warm<T: TdfaTables>(
    tdfa: &T,
    input: &[u8],
    start: usize,
    scratch: &mut Scratch,
    skip: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    run_anchored_dyn(tdfa, input, start, scratch, skip)
}

/// Execute an **anchored** TDFA driven by a literal prefilter, reusing the
/// caller-owned `scratch`. `skip` warm-starts each verify past the matched
/// literal (see [`PrefixSkip`]).
pub(crate) fn execute_prefiltered_reuse<T: TdfaTables>(
    tdfa: &T,
    input: &[u8],
    start: usize,
    pred: &StartPredicate,
    scratch: &mut Scratch,
    skip: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    run_prefiltered_dyn(tdfa, input, start, pred, scratch, skip)
}

/// Apply a `TagCommandList` to a mark file in place (scalar, two-phase). Used
/// for the cold command sites — entry, anchor alts, and `$`-conditionals —
/// which run at most once per scan or rarely; the per-byte transition path uses
/// the precompiled move sequences instead. Touches only the real-mark lanes
/// (`0..num_marks`); the trailing `clear`/`current_pos`/`scratch` lanes are
/// irrelevant here because `CurrentPos` writes use `current_pos` directly.
fn apply_cmds_scalar(buf: &mut [usize], cmds: &[TagCommand], current_pos: usize) {
    if cmds.is_empty() {
        return;
    }
    // Phase 1: CurrentPos / Nil writes, visible to sibling Copies below.
    for cmd in cmds {
        if matches!(cmd.src, MarkValue::CurrentPos) {
            buf[cmd.dst.0 as usize] = current_pos;
        }
    }
    // Phase 2: pre-read all Copy sources before any write, so cyclic or
    // shared-source copies behave as a simultaneous assignment.
    let mut reads: SmallVec<[(usize, usize); 8]> = SmallVec::new();
    for cmd in cmds {
        if let MarkValue::Copy(src) = cmd.src {
            reads.push((cmd.dst.0 as usize, buf[src.0 as usize]));
        }
    }
    for (dst, val) in reads {
        buf[dst] = val;
    }
}

/// The reusable per-search buffers. Allocating these is the only heap cost of an
/// anchored run, so callers reuse one `Scratch` across many runs: the prefilter
/// loop reuses it across every candidate, and `TdfaExecutor` owns one and reuses
/// it across every match in a `find_iter` (see `execute_reuse`). That keeps the
/// hot path allocation-free per match.
#[derive(Debug)]
pub(crate) struct Scratch {
    /// The working mark file, mutated in place by each transition. Reset to
    /// `usize::MAX` (NO_MATCH) at the start of every `run_anchored`.
    src_buf: Box<[usize]>,
    /// Observable final-tag values for the winning fallback accept. Indexed by
    /// tag, and therefore independent of the usually much larger mark file.
    best_snap: Box<[usize]>,
    /// Scratch for applying a `$`-conditional's commands before snapshotting.
    cond_buf: Box<[usize]>,
    /// Normalized capture buffer. Sized to `2 * num_capture_groups`. `finalize`
    /// writes pairs (open, close) directly: `norm_buf[2*i]` = group i open,
    /// `norm_buf[2*i+1]` = group i close. `usize::MAX` = NO_MATCH sentinel.
    /// Pre-allocated once; no per-match allocation.
    pub(crate) norm_buf: Box<[usize]>,
}

#[cfg(feature = "tdfa-jit")]
impl Scratch {
    /// Raw pointer to the working mark file, handed to JIT-compiled capture
    /// code (which applies per-transition marks in place). Valid until the next
    /// mutation of `self`.
    pub(crate) fn src_buf_mut_ptr(&mut self) -> *mut usize {
        self.src_buf.as_mut_ptr()
    }

    /// Raw pointer to the final-value snapshot, handed to JIT-compiled capture
    /// code. Valid until the next mutation of `self`.
    pub(crate) fn best_snap_mut_ptr(&mut self) -> *mut usize {
        self.best_snap.as_mut_ptr()
    }
}

impl Scratch {
    /// `width` = `num_marks + 3` (real marks, then `clear`, `current_pos`,
    /// `scratch`); `num_capture_groups` sizes both the observable-tag snapshot
    /// (full match plus capture endpoints) and the normalized capture buffer.
    pub(crate) fn new(width: usize, num_capture_groups: usize) -> Self {
        let num_output_tags = 2 + 2 * num_capture_groups;
        Self {
            src_buf: vec![usize::MAX; width].into_boxed_slice(),
            best_snap: vec![usize::MAX; num_output_tags].into_boxed_slice(),
            cond_buf: vec![usize::MAX; width].into_boxed_slice(),
            norm_buf: vec![usize::MAX; 2 * num_capture_groups].into_boxed_slice(),
        }
    }
}

/// A precomputed "skip the prefix literal" descriptor. After `memmem` confirms
/// the `len`-byte prefilter literal at offset `P`, the anchored automaton would
/// just re-consume those bytes to reach `post_state` with the mark file
/// unchanged from entry. So a warm start jumps straight to `post_state` and
/// resumes the byte loop at `P + len`, never re-scanning the literal. For a
/// fully-literal regex `post_state` is already accepting and the next byte
/// dead-ends, so the match is produced with no transition-table work at all.
#[derive(Debug, Clone, Copy)]
pub struct PrefixSkip {
    /// The state to resume the byte loop in, after the prefix is skipped.
    pub post_state: u32,
    /// How many bytes the prefilter-matched prefix spans.
    pub len: usize,
}

/// Try to build a [`PrefixSkip`] for `literal` (the prefilter's exact byte
/// sequence) against the anchored `tdfa`. Returns `None` — meaning fall back to
/// a normal anchored run from `P` — whenever replaying the literal isn't
/// trivially a no-op on the mark file: any literal transition that writes marks
/// (e.g. a capture opening inside the leading literal), an automaton with anchor
/// alts / `$`-conditionals (which could fire inside the literal), `^` making the
/// offset-0 start differ, or a literal byte that dead-ends (shouldn't happen for
/// a genuine mandatory prefix).
pub(crate) fn compute_prefix_skip(tdfa: &Tdfa, literal: &[u8]) -> Option<PrefixSkip> {
    if literal.is_empty()
        || tdfa.has_perbyte_guards()
        || tdfa.has_eoi_accepts()
        || !tdfa.has_moves()
    {
        return None;
    }
    // `^` makes the offset-0 start state differ from the general start; one
    // `post_state` can't serve both, so bail and stay offset-independent.
    if tdfa.start(0) != tdfa.start(1) {
        return None;
    }

    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let trans_moves = tdfa.transition_moves();
    let num_classes = tdfa.num_classes();

    let mut state = tdfa.start(1);
    for &b in literal {
        if state == TDFA_DEAD_STATE {
            return None;
        }
        let class = *byte_to_class.iat(b as usize) as usize;
        let idx = state as usize * num_classes + class;
        // Replaying the literal must not touch the mark file, or the warm start
        // would have to reconstruct it. This rejects capture groups that
        // open/close inside the leading literal.
        if !trans_moves.iat(idx).is_empty() {
            return None;
        }
        let next = *transitions.iat(idx);
        if next == TDFA_DEAD_STATE {
            return None;
        }
        state = next;
    }
    Some(PrefixSkip {
        post_state: state,
        len: literal.len(),
    })
}

/// Like [`compute_prefix_skip`] but for a single-byte-class prefilter (e.g.
/// `[0-9]`, where the prefilter matches exactly one byte that may be any of
/// `first_bytes`). The warm start is sound only when *every* admissible first
/// byte takes the **same** non-dead, **mark-free** transition out of the start
/// state — then one `post_state` serves them all and the skipped byte writes no
/// marks to reconstruct. Returns `None` (→ a cold anchored run from `P`) on any
/// divergence, mark write, dead transition, or the same automaton conditions
/// `compute_prefix_skip` rejects.
pub(crate) fn compute_byteclass_skip(
    tdfa: &Tdfa,
    first_bytes: impl Iterator<Item = u8>,
) -> Option<PrefixSkip> {
    if tdfa.has_perbyte_guards() || tdfa.has_eoi_accepts() || !tdfa.has_moves() {
        return None;
    }
    if tdfa.start(0) != tdfa.start(1) {
        return None;
    }

    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let trans_moves = tdfa.transition_moves();
    let num_classes = tdfa.num_classes();
    let start = tdfa.start(1) as usize;

    let mut post: Option<u32> = None;
    for b in first_bytes {
        let class = *byte_to_class.iat(b as usize) as usize;
        let idx = start * num_classes + class;
        if !trans_moves.iat(idx).is_empty() {
            return None;
        }
        let next = *transitions.iat(idx);
        if next == TDFA_DEAD_STATE {
            return None;
        }
        match post {
            None => post = Some(next),
            Some(p) if p == next => {}
            Some(_) => return None,
        }
    }
    Some(PrefixSkip { post_state: post?, len: 1 })
}

/// One anchored attempt: run the automaton from byte offset `start`, reusing the
/// caller-owned `scratch`. Returns the match (range + captures) or `None`.
///
/// `warm`, when set, is a [`PrefixSkip`]: the start `start` is the literal's
/// offset and the run jumps to `warm.post_state`, resuming the byte loop at
/// `start + warm.len` instead of re-scanning the literal.
#[inline]
fn run_anchored<C: TdfaExecConfig, T: TdfaTables>(
    tdfa: &T,
    input: &[u8],
    start: usize,
    scratch: &mut Scratch,
    warm: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    let num_marks = tdfa.num_marks();
    let curpos_lane = num_marks + 1;
    let has_captures = tdfa.has_captures();

    let src_buf: &mut [usize] = &mut scratch.src_buf;
    let best_snap: &mut [usize] = &mut scratch.best_snap;
    let cond_buf: &mut [usize] = &mut scratch.cond_buf;
    let norm_buf = &mut scratch.norm_buf;

    src_buf.fill(usize::MAX);

    // Apply entry commands using the pre-compiled MoveOp fast path (same
    // compact loop used for per-transition moves). Falls back to the scalar
    // command interpreter only when moves weren't compiled — mark file too
    // large, effectively impossible in practice.
    if C::HAS_MOVES {
        let entry_moves = tdfa.entry_moves(start);
        if !entry_moves.is_empty() {
            *src_buf.mat(curpos_lane) = start;
            for op in entry_moves {
                let v = *src_buf.iat(op.src as usize);
                *src_buf.mat(op.dst as usize) = v;
            }
        }
    } else {
        apply_cmds_scalar(src_buf, tdfa.entry_commands(start), start);
    }

    let mut last_accept: LastAccept = None;
    let mut read_live = false;
    let accept_fallback = tdfa.accept_fallback();

    let (mut state, loop_start) = match warm {
        Some(s) => (s.post_state, start + s.len),
        None => (tdfa.start(start), start),
    };

    if state == TDFA_DEAD_STATE {
        return None;
    }
    let word_icase = tdfa.word_icase();
    if C::HAS_PERBYTE_GUARDS && tdfa.guards(state).is_some_and(|g| !g.switches.is_empty()) {
        let sig = boundary_signature(input, loop_start, word_icase);
        apply_switches(tdfa, &mut state, src_buf, sig, loop_start);
    }
    if *tdfa.accepting().iat(state as usize) {
        record_accept(
            &mut last_accept,
            best_snap,
            loop_start,
            src_buf,
            tdfa.finals(state),
            has_captures,
            C::HAS_PERBYTE_GUARDS || *accept_fallback.iat(state as usize),
            &mut read_live,
        );
    }
    if C::HAS_PERBYTE_GUARDS && tdfa.guards(state).is_some_and(|g| !g.accepts.is_empty()) {
        let sig = boundary_signature(input, loop_start, word_icase);
        let prune = record_accepts(
            tdfa,
            state,
            sig,
            loop_start,
            src_buf,
            cond_buf,
            &mut last_accept,
            best_snap,
            has_captures,
            &mut read_live,
        );
        if let Some((prune_state, prune_cmds)) = prune {
            apply_cmds_scalar(src_buf, prune_cmds, loop_start);
            state = prune_state;
        }
    }

    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let (mv_cells, mv_arena) = tdfa.moves_raw();
    let accepting = tdfa.accepting();
    let trans_flags = tdfa.trans_flags();
    let num_classes = tdfa.num_classes();

    let use_fast =
        C::SKIP_MARKS && !C::HAS_PERBYTE_GUARDS && !tdfa.exec_transitions().is_empty();

    let mut live_position = loop_start;

    let mut completed = true;

    if use_fast {
        let exec_trans = tdfa.exec_transitions();
        let mut estate = state * num_classes as u32;
        for (i, &byte) in input[loop_start..].iter().enumerate() {
            let class = *byte_to_class.iat(byte as usize) as u32;
            let raw = *exec_trans.iat((estate + class) as usize);
            if raw == TDFA_DEAD_STATE {
                completed = false;
                break;
            }
            estate = raw & EXEC_STATE_MASK;
            if raw & EXEC_ACCEPT_FLAG != 0 {
                last_accept = Some((loop_start + i + 1, &[], start));
                read_live = false;
            }
        }
        state = estate / num_classes as u32;
    } else {
    let (psl_index, psl_table): (&[u32], &[PosStampLoopFlat]) =
        if !C::HAS_PERBYTE_GUARDS && !C::SKIP_MARKS && C::HAS_MOVES {
            tdfa.psl_tables()
        } else {
            (&[], &[])
        };
    let (ss_index, ss_table): (&[u32], &[ScanSkipFlat]) = if !C::HAS_PERBYTE_GUARDS && C::HAS_MOVES {
        tdfa.scan_skip_tables()
    } else {
        (&[], &[])
    };
    let stamp_arena = tdfa.stamp_arena();
    let psl_ascii_bms: &[u64] = if !C::HAS_PERBYTE_GUARDS && !C::SKIP_MARKS && C::HAS_MOVES {
        tdfa.psl_ascii_bms()
    } else {
        &[]
    };
    let mut pos = loop_start;
    'byte_loop: while pos < input.len() {
        // Scan-skip / scan-stamp: if the current state is a non-accepting
        // self-loop, fast-scan ahead using the precomputed byte bitmap.
        // Pure skip (stamp_marks empty): self-loop moves are all empty, no
        // mark update needed.  Scan-stamp (stamp_marks non-empty): self-loop
        // moves are all `curpos → mark_j`; write each mark_j = pos once
        // after the scan (net effect of the per-byte curpos writes).
        if let Some(&ssi) = ss_index.get(state as usize) {
            if ssi != ACCEL_NONE {
                let ss = &ss_table[ssi as usize];
                let scan_start = pos;
                pos = scan_fast(&ss.fast, &ss.byte_bitmap, input, pos);
                // Stamp marks whenever the scan consumed bytes.  This MUST happen
                // before any early exit so that the EOI accept path (which runs
                // after the byte loop when `completed && has_eoi_accepts`) reads
                // the correct marks.
                if ss.stamp.1 != 0 && pos > scan_start {
                    for &mark_idx in stamp_slice(stamp_arena, ss.stamp) {
                        *src_buf.mat(mark_idx as usize) = pos;
                    }
                }
                if pos >= input.len() {
                    break; // byte loop exhausted — let EOI accept path run
                }
            }
        } // end scan-skip
        let byte = *input.iat(pos);
        let class = *byte_to_class.iat(byte as usize) as usize;
        let idx = state as usize * num_classes + class;
        let next = *transitions.iat(idx);
        if next == TDFA_DEAD_STATE {
            completed = false;
            break;
        }
        if !C::SKIP_MARKS {
            if C::HAS_MOVES {
                let moves = csr_iat(mv_cells, mv_arena, idx);
                if !moves.is_empty() {
                    let p = pos + 1;
                    *src_buf.mat(curpos_lane) = p;
                    for op in moves.iter() {
                        // Avoid a 4-cycle load-use chain when the source is
                        // curpos_lane — `p` is already in a register.
                        let v = if op.src as usize == curpos_lane { p } else { *src_buf.iat(op.src as usize) };
                        *src_buf.mat(op.dst as usize) = v;
                    }
                }
            } else {
                apply_cmds_scalar(src_buf, tdfa.transition_commands(idx), pos + 1);
            }
        }
        state = next;
        // tf is loaded in parallel with transitions[idx] (same index, both can
        // start as soon as idx is known) and encodes TF_ACCEPT + TF_FALLBACK
        // plus, for the guards path, TF_SWITCHES + TF_ACCEPTS — so the common
        // no-boundary byte checks a register bit instead of striding the guard
        // tables.
        let tf = *trans_flags.iat(idx);
        if C::HAS_PERBYTE_GUARDS {
            live_position = pos + 1;
            if tf & TF_SWITCHES != 0 {
                let sig = boundary_signature(input, pos + 1, word_icase);
                apply_switches(tdfa, &mut state, src_buf, sig, pos + 1);
            }
        }
        // A fired switch moves the live state off the transition target, so tf
        // no longer describes it; fall back to the per-state tables then.
        let switched = C::HAS_PERBYTE_GUARDS && state != next;
        let is_accepting = if switched {
            *accepting.iat(state as usize)
        } else {
            tf & TF_ACCEPT != 0
        };
        if is_accepting {
            let needs_snapshot = if C::HAS_PERBYTE_GUARDS {
                true
            } else {
                tf & TF_FALLBACK != 0
            };
            // PSL peek: for ASCII-only PSL states, check the next byte before
            // committing. If it's not in the self-loop set the scan returns
            // p=start immediately; skip both the scan and the wasted first
            // record_accept. If it is in-set, run the scan and record once at
            // the final position (eliminating the wasted record_accept at pos+1
            // that the old two-call path always emitted).
            let s2 = state as usize * 2;
            let (psl_bm0, psl_bm1) = if s2 + 1 < psl_ascii_bms.len() {
                (psl_ascii_bms[s2], psl_ascii_bms[s2 + 1])
            } else {
                (0, 0)
            };
            if (psl_bm0 | psl_bm1) != 0 {
                let start = pos + 1;
                let next_in_set = start < input.len() && {
                    let b = *input.iat(start) as usize;
                    b < 0x80 && (({
                        let w = if b < 0x40 { psl_bm0 } else { psl_bm1 };
                        (w >> (b & 63)) & 1
                    }) != 0)
                };
                if next_in_set {
                    // psl_ascii_bms nonzero ↔ psl_index[state] != ACCEL_NONE.
                    let psl = &psl_table[psl_index[state as usize] as usize];
                    let p = scan_pos_stamp(psl, input, start);
                    if p > start {
                        *src_buf.mat(curpos_lane) = p;
                        for &mark_idx in stamp_slice(stamp_arena, psl.stamp) {
                            *src_buf.mat(mark_idx as usize) = p;
                        }
                        record_accept(
                            &mut last_accept,
                            best_snap,
                            p,
                            src_buf,
                            tdfa.finals(state),
                            has_captures,
                            psl.needs_snapshot,
                            &mut read_live,
                        );
                        pos = p;
                        continue 'byte_loop;
                    }
                    // p == start despite peek: fall through to plain accept.
                }
                record_accept(
                    &mut last_accept,
                    best_snap,
                    pos + 1,
                    src_buf,
                    tdfa.finals(state),
                    has_captures,
                    needs_snapshot,
                    &mut read_live,
                );
            } else {
                // PSL-None or non-ASCII PSL: original two-call path.
                record_accept(
                    &mut last_accept,
                    best_snap,
                    pos + 1,
                    src_buf,
                    tdfa.finals(state),
                    has_captures,
                    needs_snapshot,
                    &mut read_live,
                );
                if let Some(&pi) = psl_index.get(state as usize).filter(|&&pi| pi != ACCEL_NONE) {
                    let psl = &psl_table[pi as usize];
                    let start = pos + 1;
                    let p = scan_pos_stamp(psl, input, start);
                    if p > start {
                        *src_buf.mat(curpos_lane) = p;
                        for &mark_idx in stamp_slice(stamp_arena, psl.stamp) {
                            *src_buf.mat(mark_idx as usize) = p;
                        }
                        record_accept(
                            &mut last_accept,
                            best_snap,
                            p,
                            src_buf,
                            tdfa.finals(state),
                            has_captures,
                            psl.needs_snapshot,
                            &mut read_live,
                        );
                        pos = p;
                        continue 'byte_loop;
                    }
                }
            }
        }
        let has_accept_guards = C::HAS_PERBYTE_GUARDS
            && if switched {
                tdfa.guards(state).is_some_and(|g| !g.accepts.is_empty())
            } else {
                tf & TF_ACCEPTS != 0
            };
        if has_accept_guards {
            let sig = boundary_signature(input, pos + 1, word_icase);
            let prune = record_accepts(
                tdfa,
                state,
                sig,
                pos + 1,
                src_buf,
                cond_buf,
                &mut last_accept,
                best_snap,
                has_captures,
                &mut read_live,
            );
            // Leftmost cut: the fired accept outranks every thread the pruned
            // state drops, so switch to it — for `\w+$`-style patterns the
            // next byte then hits the dead state instead of the scan running
            // to end of input.
            if let Some((prune_state, prune_cmds)) = prune {
                apply_cmds_scalar(src_buf, prune_cmds, pos + 1);
                state = prune_state;
            }
        }
        pos += 1;
    }
    }

    if C::HAS_PERBYTE_GUARDS {
        let sig = boundary_signature(input, live_position, word_icase);
        // Scan is over — the leftmost-cut return is irrelevant here.
        let _ = record_accepts(
            tdfa,
            state,
            sig,
            live_position,
            src_buf,
            cond_buf,
            &mut last_accept,
            best_snap,
            has_captures,
            &mut read_live,
        );
    } else if completed && tdfa.has_eoi_accepts() {
        let sig = boundary_signature(input, input.len(), word_icase);
        let _ = record_accepts(
            tdfa,
            state,
            sig,
            input.len(),
            src_buf,
            cond_buf,
            &mut last_accept,
            best_snap,
            has_captures,
            &mut read_live,
        );
    }

    match last_accept {
        Some((end, finals, start)) => Some(if has_captures {
            finalize(
                finals,
                src_buf,
                (!read_live).then_some(&*best_snap),
                end,
                norm_buf,
            )
        } else {
            let s = if read_live {
                snapshot_match_start(finals, src_buf)
            } else {
                start
            };
            finalize_nocap(s, end)
        }),
        None => None,
    }
}

/// Follow switch guards that hold at this position until no further state
/// change applies.
fn apply_switches<T: TdfaTables>(tdfa: &T, state: &mut u32, buf: &mut [usize], sig: u8, pos: usize) {
    for _ in 0..tdfa.num_states() {
        let Some(sw) = tdfa
            .guards(*state)
            .and_then(|g| g.switches.iter().find(|sw| sw.cond.holds_sig(sig)))
        else {
            return;
        };
        apply_cmds_scalar(buf, &sw.commands, pos);
        *state = sw.alt;
    }

    debug_assert!(
        !tdfa
            .guards(*state)
            .is_some_and(|g| g.switches.iter().any(|sw| sw.cond.holds_sig(sig))),
        "zero-width switch cycle"
    );
}

/// For each `$`-style accept on `state` whose predicate holds at the position's
/// boundary signature `sig`, snapshot the marks into `cond_buf`, apply the
/// accept's commands, and treat it as a new accept candidate.
///
/// Returns the leftmost cut of the first (highest-priority) accept that fired,
/// if it has one: the pruned successor state and the mark moves that switch
/// into it. Mid-scan callers apply it to the live state so the automaton can
/// die instead of dragging the outranked scanner threads to end of input; the
/// EOI callers ignore it (the scan is over). Safe to apply after recording —
/// `consider_accept` materializes every candidate (snapshot or recorded
/// start), so later live-mark writes can't corrupt them.
#[allow(clippy::too_many_arguments)]
fn record_accepts<'a, T: TdfaTables>(
    tdfa: &'a T,
    state: u32,
    sig: u8,
    pos: usize,
    marks: &[usize],
    cond_buf: &mut [usize],
    last_accept: &mut LastAccept<'a>,
    best_snap: &mut [usize],
    has_captures: bool,
    read_live: &mut bool,
) -> Option<(u32, &'a [TagCommand])> {
    let g = tdfa.guards(state)?;
    let mut prune: Option<(u32, &[TagCommand])> = None;
    let mut fired_any = false;
    for ac in &g.accepts {
        if !ac.cond.holds_sig(sig) {
            continue;
        }
        if !fired_any {
            fired_any = true;
            if ac.prune != NO_PRUNE {
                prune = Some((ac.prune, &ac.prune_commands));
            }
        }
        cond_buf.copy_from_slice(marks);
        apply_cmds_scalar(cond_buf, &ac.commands, pos);
        consider_accept(
            last_accept,
            best_snap,
            pos,
            cond_buf,
            &ac.finals,
            has_captures,
            read_live,
        );
    }
    prune
}

/// Read the `FULL_MATCH_START` value a finalization snapshot would produce, or
/// `usize::MAX` (NO_MATCH) if the row doesn't set it.
fn snapshot_match_start(finals: &[FinalCommand], marks: &[usize]) -> usize {
    for cmd in finals {
        if cmd.tag == FULL_MATCH_START {
            if let MarkValue::Copy(src) = cmd.src {
                return marks[src.0 as usize];
            }
        }
    }
    usize::MAX
}

/// Record an accept candidate, keeping the **leftmost** match.
#[inline]
#[allow(clippy::too_many_arguments)]
fn record_accept<'a>(
    last_accept: &mut LastAccept<'a>,
    best_snap: &mut [usize],
    end: usize,
    marks: &[usize],
    finals: &'a [FinalCommand],
    has_captures: bool,
    snapshot: bool,
    read_live: &mut bool,
) {
    if snapshot {
        consider_accept(last_accept, best_snap, end, marks, finals, has_captures, read_live);
    } else {
        *last_accept = Some((end, finals, usize::MAX));
        *read_live = true;
    }
}

/// The start is read from the live `marks` *before* any copy, so a non-replacing
/// candidate costs only the comparison — no snapshot copy.
#[inline]
fn consider_accept<'a>(
    last_accept: &mut LastAccept<'a>,
    best_snap: &mut [usize],
    end: usize,
    marks: &[usize],
    finals: &'a [FinalCommand],
    has_captures: bool,
    read_live: &mut bool,
) {
    let new_start = snapshot_match_start(finals, marks);
    if let Some((best_end, _, best_start)) = last_accept {
        if new_start > *best_start || (new_start == *best_start && end <= *best_end) {
            return;
        }
    }
    if has_captures {
        snapshot_final_values(finals, marks, best_snap);
    }
    *last_accept = Some((end, finals, new_start));
    *read_live = false;
}

/// Materialize the observable final values of an accept into tag-indexed
/// storage. Predicate-sentinel tags are intentionally omitted: they are not
/// part of the returned match and may lie beyond `values`.
fn snapshot_final_values(finals: &[FinalCommand], marks: &[usize], values: &mut [usize]) {
    for cmd in finals {
        let tag = cmd.tag as usize;
        if tag >= values.len() {
            continue;
        }
        let MarkValue::Copy(src) = cmd.src else {
            unreachable!("finals never use CurrentPos")
        };
        values[tag] = marks[src.0 as usize];
    }
}

/// Build a capture-free match directly from the recorded start and end.
fn finalize_nocap(start: usize, end: usize) -> NfaMatch {
    let start = if start == usize::MAX { 0 } else { start };
    NfaMatch {
        range: start..end,
        captures: Vec::new(),
    }
}

/// Normalize the mark file into `norm_buf` and return the match range.
///
/// Applies `FinalCommand`s: each command maps a mark register to a tag index.
/// Tags 0/1 are FULL_MATCH_START/END (used to build the range); tags 2+ map
/// to capture groups: `norm_buf[tag - 2] = mark_value`. Sentinel tags (above
/// the capture range, allocated by `make_sentinel()` for `ProgressSince`
/// nullable-loop predicates) exceed `norm_buf.len()` and are skipped.
/// `usize::MAX` is the NO_MATCH sentinel throughout.
fn finalize(
    finals: &[FinalCommand],
    marks: &[usize],
    final_values: Option<&[usize]>,
    end: usize,
    norm_buf: &mut [usize],
) -> NfaMatch {
    norm_buf.fill(usize::MAX);

    let mut full_start = usize::MAX;
    let mut full_end = usize::MAX;
    let num_output_tags = norm_buf.len() + 2;

    for cmd in finals {
        let tag = cmd.tag as usize;
        if tag >= num_output_tags {
            continue;
        }
        let MarkValue::Copy(src) = cmd.src else {
            unreachable!("finals never use CurrentPos")
        };
        let val = match final_values {
            Some(values) => values[tag],
            None => marks[src.0 as usize],
        };
        match tag {
            0 => full_start = val,
            1 => full_end = val,
            tag => norm_buf[tag - 2] = val,
        }
    }

    let start_pos = if full_start == usize::MAX { 0 } else { full_start };
    let end_pos = if full_end == usize::MAX { end } else { full_end };
    NfaMatch { range: start_pos..end_pos, captures: Vec::new() }
}

/// JIT capture-path setup: reset the working mark file to the unset sentinel and
/// apply the automaton's entry commands for `start`.
#[cfg(feature = "tdfa-jit")]
pub(crate) fn jit_prepare_marks(tdfa: &Tdfa, scratch: &mut Scratch, start: usize) {
    scratch.src_buf.fill(usize::MAX);
    apply_cmds_scalar(&mut scratch.src_buf, tdfa.entry_commands(start), start);
}

/// JIT capture-path finalize: build the winning `NfaMatch` for the accept at
/// `state`/`end`. `read_live` selects the buffer holding the winner's marks.
#[cfg(feature = "tdfa-jit")]
pub(crate) fn jit_finalize(
    tdfa: &Tdfa,
    state: u32,
    scratch: &mut Scratch,
    end: usize,
    read_live: bool,
) -> NfaMatch {
    let final_values = (!read_live).then_some(&*scratch.best_snap);
    finalize(
        tdfa.finals(state),
        &scratch.src_buf,
        final_values,
        end,
        &mut scratch.norm_buf,
    )
}

/// A TDFA match that borrows captures from the owning iterator's `Scratch.norm_buf`.
/// Zero allocation per match. Convert to an owned [`NfaMatch`] via `From`/`Into`.
///
/// `captures` is a flat slice of `usize` pairs: `captures[2*i]` = group `i` open
/// byte offset, `captures[2*i+1]` = group `i` close byte offset. `usize::MAX`
/// means the group did not participate.
pub struct TdfaMatch<'a> {
    /// The full match range.
    pub range: Range<usize>,
    captures: &'a [usize],
}

impl<'a> TdfaMatch<'a> {
    pub(crate) fn new(range: Range<usize>, norm_buf: &'a [usize]) -> Self {
        Self { range, captures: norm_buf }
    }

    /// Number of capture groups (not counting the full match).
    pub fn num_captures(&self) -> usize {
        self.captures.len() / 2
    }

    /// Capture group `i` (0-indexed). `None` if the group did not participate.
    pub fn capture(&self, i: usize) -> Option<Range<usize>> {
        let s = self.captures[2 * i];
        if s == usize::MAX {
            return None;
        }
        Some(s..self.captures[2 * i + 1])
    }
}

impl<'a> From<TdfaMatch<'a>> for NfaMatch {
    fn from(m: TdfaMatch<'a>) -> NfaMatch {
        let captures = m.captures
            .chunks_exact(2)
            .map(|c| if c[0] == usize::MAX { None } else { Some(c[0]..c[1]) })
            .collect();
        NfaMatch { range: m.range, captures }
    }
}

#[cfg(test)]
mod scratch_tests {
    use super::Scratch;

    #[test]
    fn fallback_snapshot_scales_with_output_tags() {
        let scratch = Scratch::new(10_003, 2);
        assert_eq!(scratch.src_buf.len(), 10_003);
        assert_eq!(scratch.cond_buf.len(), 10_003);
        assert_eq!(scratch.best_snap.len(), 6);
        assert_eq!(scratch.norm_buf.len(), 4);
    }
}
