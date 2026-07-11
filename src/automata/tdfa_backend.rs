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
    EXEC_ACCEPT_FLAG, EXEC_STATE_MASK, FinalCommand, MarkValue, TDFA_DEAD_STATE, TagCommand, Tdfa,
    PosStampLoop, ScanFast, ScanSkip, SCAN_MAX_RANGES,
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
    pos
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
}

/// Config marker parameterized directly on its flag bits, so adding a flag is a
/// new `const` param rather than a new named type per combination.
pub(crate) struct ExecConfig<const HAS_PERBYTE_GUARDS: bool>;

impl<const G: bool> TdfaExecConfig for ExecConfig<G> {
    const HAS_PERBYTE_GUARDS: bool = G;
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

/// `num_marks + 3`: the mark-file width (real marks, then `clear`,
/// `current_pos`, `scratch`). The size a [`Scratch`] must be built with.
pub(crate) fn mark_file_width(tdfa: &Tdfa) -> usize {
    tdfa.num_marks() + 3
}

/// Pick the config-flag monomorphization (`HAS_PERBYTE_GUARDS`) from the
/// automaton's contents and run one anchored attempt. The flag check is once per
/// attempt; the const generic still drops the per-byte branches inside
/// `run_anchored`.
fn run_anchored_dyn(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
    scratch: &mut Scratch,
    warm: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    if tdfa.has_perbyte_guards() {
        run_anchored::<ExecConfig<true>>(tdfa, input, start, scratch, warm)
    } else {
        run_anchored::<ExecConfig<false>>(tdfa, input, start, scratch, warm)
    }
}

/// The prefilter loop: skip to each candidate `pred` allows (at or after
/// `start`) and run the anchored automaton there, returning the first (leftmost)
/// match. The `scratch` is reused across every candidate; `skip`, when set,
/// warm-starts each attempt past the matched literal (see [`PrefixSkip`]).
fn run_prefiltered_dyn(
    tdfa: &Tdfa,
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
pub(crate) fn execute_reuse(
    tdfa: &Tdfa,
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
pub(crate) fn execute_reuse_warm(
    tdfa: &Tdfa,
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
pub(crate) fn execute_prefiltered_reuse(
    tdfa: &Tdfa,
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
    /// Snapshot of the winning accept's marks (copied in on replace only).
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

    /// Raw pointer to the accept-snapshot buffer, handed to JIT-compiled capture
    /// code (which copies the live marks here on a fallback accept). Valid until
    /// the next mutation of `self`.
    pub(crate) fn best_snap_mut_ptr(&mut self) -> *mut usize {
        self.best_snap.as_mut_ptr()
    }
}

impl Scratch {
    /// `width` = `num_marks + 3` (real marks, then `clear`, `current_pos`,
    /// `scratch`); `num_capture_groups` sizes the normalized capture buffer.
    pub(crate) fn new(width: usize, num_capture_groups: usize) -> Self {
        Self {
            src_buf:   vec![usize::MAX; width].into_boxed_slice(),
            best_snap: vec![usize::MAX; width].into_boxed_slice(),
            cond_buf:  vec![usize::MAX; width].into_boxed_slice(),
            norm_buf:  vec![usize::MAX; 2 * num_capture_groups].into_boxed_slice(),
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
pub(crate) struct PrefixSkip {
    /// The state to resume the byte loop in, after the prefix is skipped.
    pub(crate) post_state: u32,
    /// How many bytes the prefilter-matched prefix spans.
    pub(crate) len: usize,
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
fn run_anchored<C: TdfaExecConfig>(
    tdfa: &Tdfa,
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
    if tdfa.has_moves() {
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
    if C::HAS_PERBYTE_GUARDS && !tdfa.guards(state).switches.is_empty() {
        let sig = boundary_signature(input, loop_start, word_icase);
        apply_switches(tdfa, &mut state, src_buf, sig, loop_start);
    }
    if *tdfa.accepting().iat(state as usize) {
        record_accept(
            &mut last_accept,
            best_snap,
            loop_start,
            src_buf,
            tdfa.finals().iat(state as usize),
            has_captures,
            C::HAS_PERBYTE_GUARDS || *accept_fallback.iat(state as usize),
            &mut read_live,
        );
    }
    if C::HAS_PERBYTE_GUARDS && !tdfa.guards(state).accepts.is_empty() {
        let sig = boundary_signature(input, loop_start, word_icase);
        record_accepts(
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
    }

    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let trans_moves = tdfa.transition_moves();
    let trans_cmds = tdfa.transition_commands();
    let accepting = tdfa.accepting();
    let num_classes = tdfa.num_classes();

    let use_moves = tdfa.has_moves();

    let skip_marks = !has_captures && tdfa.start_fixed();

    let use_fast =
        skip_marks && !C::HAS_PERBYTE_GUARDS && !tdfa.exec_transitions().is_empty();

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
    let pos_stamp_loops: &[Option<PosStampLoop>] = if !C::HAS_PERBYTE_GUARDS && !skip_marks && use_moves {
        tdfa.pos_stamp_loops()
    } else {
        &[]
    };
    let scan_skips: &[Option<ScanSkip>] = if !C::HAS_PERBYTE_GUARDS && use_moves {
        tdfa.scan_skips()
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
        if let Some(ss) = scan_skips.get(state as usize).and_then(Option::as_ref) {
            let scan_start = pos;
            pos = match &ss.fast {
                // Few excluded bytes, all ASCII: use memchr to jump directly
                // to the next stopping byte.
                ScanFast::Memchr { count, bytes } => match count {
                    0 => input.len(), // all bytes self-loop; scan to EOI
                    1 => memchr::memchr(bytes[0], &input[pos..])
                        .map(|i| pos + i).unwrap_or(input.len()),
                    2 => memchr::memchr2(bytes[0], bytes[1], &input[pos..])
                        .map(|i| pos + i).unwrap_or(input.len()),
                    _ => memchr::memchr3(bytes[0], bytes[1], bytes[2], &input[pos..])
                        .map(|i| pos + i).unwrap_or(input.len()),
                },
                // Few ASCII excluded bytes + non-ASCII bytes also excluded
                // (typical for Unicode patterns like `[^"]`): scan until the
                // first non-ASCII byte OR one of the excluded ASCII bytes.
                //
                // The `b >= 0x80` check eliminates the indexed bitmap load
                // (which has an ~8-cycle load-use chain), replacing it with
                // two simple comparisons.  LLVM can vectorise short closures.
                ScanFast::AsciiBarrier { count, bytes } => {
                    let b0 = bytes[0];
                    let b1 = bytes[1];
                    let end = match count {
                        0 => input[pos..].iter().position(|&b| b >= 0x80),
                        1 => input[pos..].iter().position(|&b| b >= 0x80 || b == b0),
                        _ => input[pos..].iter().position(|&b| b >= 0x80 || b == b0 || b == b1),
                    };
                    end.map(|i| pos + i).unwrap_or(input.len())
                },
                // All non-ASCII excluded; set fits in a small number of byte
                // ranges.  SSE2 saturating-subtract range masks process 16
                // bytes per iteration; scalar tail uses the two ASCII bitmap
                // words (bm0/bm1) — single shift+and — instead of the range
                // cascade, trading 8 compare/branch pairs for one load+shift.
                ScanFast::AsciiRanges { count, pairs, bm0, bm1 } => {
                    #[cfg(all(target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
                    { pos = scan_ascii_ranges_sse2(input, pos, *count, pairs); }
                    // Scalar tail (also the full path on non-x86-64).
                    while pos < input.len() {
                        let b = *input.iat(pos) as usize;
                        if b >= 0x80 { break; }
                        let word = if b < 0x40 { *bm0 } else { *bm1 };
                        if (word >> (b & 63)) & 1 == 0 { break; }
                        pos += 1;
                    }
                    pos
                }
                // All non-ASCII bytes excluded; pre-store the two ASCII bitmap
                // words.  Selecting between two registers with a conditional
                // move eliminates the 4-cycle data-dependent indexed load from
                // bitmap[b>>6] (L1 load-use chain), cutting the critical path
                // from ~9 cycles/byte to ~5 cycles/byte.
                ScanFast::BitmapAscii { bm0, bm1 } => {
                    while pos < input.len() {
                        let b = *input.iat(pos) as usize;
                        if b >= 0x80 {
                            break;
                        }
                        let word = if b < 0x40 { *bm0 } else { *bm1 };
                        if (word >> (b & 63)) & 1 == 0 {
                            break;
                        }
                        pos += 1;
                    }
                    pos
                }
                // All non-ASCII self-loop; ASCII exit bytes fit in ≤SCAN_MAX_RANGES ranges.
                // SSE2 path scans 16 bytes per iteration until a stop byte is found.
                // Scalar tail uses the two ASCII bitmap words (bm0/bm1) — same cost
                // as BitmapAscii — rather than the 4-range loop.
                ScanFast::AsciiRangesStop { count, pairs, bm0, bm1 } => {
                    #[cfg(all(target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
                    { pos = scan_ascii_ranges_stop_sse2(input, pos, *count, pairs); }
                    while pos < input.len() {
                        let b = *input.iat(pos) as usize;
                        if b < 0x80 {
                            let word = if b < 0x40 { *bm0 } else { *bm1 };
                            if (word >> (b & 63)) & 1 == 0 { break; }
                        }
                        pos += 1;
                    }
                    pos
                }
                // Generic bitmap scan.
                ScanFast::Bitmap => {
                    let bitmap = &ss.byte_bitmap;
                    while pos < input.len() {
                        let b = *input.iat(pos) as usize;
                        if (bitmap[b >> 6] >> (b & 63)) & 1 == 0 {
                            break;
                        }
                        pos += 1;
                    }
                    pos
                }
            };
            // Stamp marks whenever the scan consumed bytes.  This MUST happen
            // before any early exit so that the EOI accept path (which runs
            // after the byte loop when `completed && has_eoi_accepts`) reads
            // the correct marks.
            if !ss.stamp_marks.is_empty() && pos > scan_start {
                for &mark_idx in &ss.stamp_marks {
                    *src_buf.mat(mark_idx as usize) = pos;
                }
            }
            if pos >= input.len() {
                break; // byte loop exhausted — let EOI accept path run
            }
        }
        let byte = *input.iat(pos);
        let class = *byte_to_class.iat(byte as usize) as usize;
        let idx = state as usize * num_classes + class;
        let next = *transitions.iat(idx);
        if next == TDFA_DEAD_STATE {
            completed = false;
            break;
        }
        if !skip_marks {
            if use_moves {
                let moves = trans_moves.iat(idx);
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
                apply_cmds_scalar(src_buf, trans_cmds.iat(idx), pos + 1);
            }
        }
        state = next;
        if C::HAS_PERBYTE_GUARDS {
            live_position = pos + 1;
            if !tdfa.guards(state).switches.is_empty() {
                let sig = boundary_signature(input, pos + 1, word_icase);
                apply_switches(tdfa, &mut state, src_buf, sig, pos + 1);
            }
        }
        if *accepting.iat(state as usize) {
            record_accept(
                &mut last_accept,
                best_snap,
                pos + 1,
                src_buf,
                tdfa.finals().iat(state as usize),
                has_captures,
                C::HAS_PERBYTE_GUARDS || *accept_fallback.iat(state as usize),
                &mut read_live,
            );
            // Position-stamp self-loop peel: if this accepting state's
            // self-loop only stamps curpos → marks, scan ahead to the run
            // end and stamp once instead of once per byte.
            if let Some(psl) = pos_stamp_loops.get(state as usize).and_then(Option::as_ref) {
                let start = pos + 1;
                let p = match &psl.fast {
                    ScanFast::Memchr { count, bytes } => match count {
                        0 => input.len(),
                        1 => memchr::memchr(bytes[0], &input[start..]).map(|i| start + i).unwrap_or(input.len()),
                        2 => memchr::memchr2(bytes[0], bytes[1], &input[start..]).map(|i| start + i).unwrap_or(input.len()),
                        _ => memchr::memchr3(bytes[0], bytes[1], bytes[2], &input[start..]).map(|i| start + i).unwrap_or(input.len()),
                    },
                    ScanFast::AsciiBarrier { count, bytes } => {
                        let b0 = bytes[0];
                        let b1 = bytes[1];
                        let end = match count {
                            0 => input[start..].iter().position(|&b| b >= 0x80),
                            1 => input[start..].iter().position(|&b| b >= 0x80 || b == b0),
                            _ => input[start..].iter().position(|&b| b >= 0x80 || b == b0 || b == b1),
                        };
                        end.map(|i| start + i).unwrap_or(input.len())
                    },
                    ScanFast::AsciiRanges { count, pairs, bm0, bm1 } => {
                        #[cfg(all(target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
                        let mut p = scan_ascii_ranges_sse2(input, start, *count, pairs);
                        #[cfg(not(all(target_arch = "x86_64", not(feature = "prohibit-unsafe"))))]
                        let mut p = start;
                        while p < input.len() {
                            let b = *input.iat(p) as usize;
                            if b >= 0x80 { break; }
                            let word = if b < 0x40 { *bm0 } else { *bm1 };
                            if (word >> (b & 63)) & 1 == 0 { break; }
                            p += 1;
                        }
                        p
                    },
                    ScanFast::BitmapAscii { bm0, bm1 } => {
                        let mut p = start;
                        while p < input.len() {
                            let b = *input.iat(p) as usize;
                            if b >= 0x80 {
                                break;
                            }
                            let word = if b < 0x40 { *bm0 } else { *bm1 };
                            if (word >> (b & 63)) & 1 == 0 {
                                break;
                            }
                            p += 1;
                        }
                        p
                    },
                    ScanFast::AsciiRangesStop { count, pairs, bm0, bm1 } => {
                        #[cfg(all(target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
                        let mut p = scan_ascii_ranges_stop_sse2(input, start, *count, pairs);
                        #[cfg(not(all(target_arch = "x86_64", not(feature = "prohibit-unsafe"))))]
                        let mut p = start;
                        while p < input.len() {
                            let b = *input.iat(p) as usize;
                            if b < 0x80 {
                                let word = if b < 0x40 { *bm0 } else { *bm1 };
                                if (word >> (b & 63)) & 1 == 0 { break; }
                            }
                            p += 1;
                        }
                        p
                    },
                    ScanFast::Bitmap => {
                        let bitmap = &psl.byte_bitmap;
                        let mut p = start;
                        while p < input.len() {
                            let b = *input.iat(p) as usize;
                            if (bitmap[b >> 6] >> (b & 63)) & 1 == 0 {
                                break;
                            }
                            p += 1;
                        }
                        p
                    },
                };
                if p > start {
                    *src_buf.mat(curpos_lane) = p;
                    for &mark_idx in &psl.stamp_marks {
                        *src_buf.mat(mark_idx as usize) = p;
                    }
                    record_accept(
                        &mut last_accept,
                        best_snap,
                        p,
                        src_buf,
                        tdfa.finals().iat(state as usize),
                        has_captures,
                        psl.needs_snapshot,
                        &mut read_live,
                    );
                    pos = p;
                    continue 'byte_loop;
                }
            }
        }
        if C::HAS_PERBYTE_GUARDS && !tdfa.guards(state).accepts.is_empty() {
            let sig = boundary_signature(input, pos + 1, word_icase);
            record_accepts(
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
        }
        pos += 1;
    }
    }

    if C::HAS_PERBYTE_GUARDS {
        let sig = boundary_signature(input, live_position, word_icase);
        record_accepts(
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
        record_accepts(
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
        Some((end, finals, start)) => {
            let marks: &[usize] = if read_live { src_buf } else { best_snap };
            Some(if has_captures {
                finalize(finals, marks, end, norm_buf)
            } else {
                let s = if read_live {
                    snapshot_match_start(finals, marks)
                } else {
                    start
                };
                finalize_nocap(s, end)
            })
        }
        None => None,
    }
}

/// Follow switch guards that hold at this position until no further state
/// change applies.
fn apply_switches(tdfa: &Tdfa, state: &mut u32, buf: &mut [usize], sig: u8, pos: usize) {
    for _ in 0..tdfa.num_states() {
        let Some(sw) = tdfa
            .guards(*state)
            .switches
            .iter()
            .find(|sw| sw.cond.holds_sig(sig))
        else {
            return;
        };
        apply_cmds_scalar(buf, &sw.commands, pos);
        *state = sw.alt;
    }

    debug_assert!(
        !tdfa
            .guards(*state)
            .switches
            .iter()
            .any(|sw| sw.cond.holds_sig(sig)),
        "zero-width switch cycle"
    );
}

/// For each `$`-style accept on `state` whose predicate holds at the position's
/// boundary signature `sig`, snapshot the marks into `cond_buf`, apply the
/// accept's commands, and treat it as a new accept candidate.
#[allow(clippy::too_many_arguments)]
fn record_accepts<'a>(
    tdfa: &'a Tdfa,
    state: u32,
    sig: u8,
    pos: usize,
    marks: &[usize],
    cond_buf: &mut [usize],
    last_accept: &mut LastAccept<'a>,
    best_snap: &mut [usize],
    has_captures: bool,
    read_live: &mut bool,
) {
    for ac in &tdfa.guards(state).accepts {
        if !ac.cond.holds_sig(sig) {
            continue;
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
        best_snap.copy_from_slice(marks);
    }
    *last_accept = Some((end, finals, new_start));
    *read_live = false;
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
    end: usize,
    norm_buf: &mut [usize],
) -> NfaMatch {
    norm_buf.fill(usize::MAX);

    let mut full_start = usize::MAX;
    let mut full_end = usize::MAX;

    for cmd in finals {
        let MarkValue::Copy(src) = cmd.src else {
            unreachable!("finals never use CurrentPos")
        };
        let val = marks[src.0 as usize];
        match cmd.tag as usize {
            0 => full_start = val,
            1 => full_end = val,
            tag => {
                let norm_idx = tag - 2;
                // Sentinel tags (ProgressSince for nullable loops) have indices
                // beyond the capture range; norm_buf is sized to captures only.
                if norm_idx < norm_buf.len() {
                    norm_buf[norm_idx] = val;
                }
            }
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
    let marks: &[usize] = if read_live {
        &scratch.src_buf
    } else {
        &scratch.best_snap
    };
    finalize(&tdfa.finals()[state as usize], marks, end, &mut scratch.norm_buf)
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
