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
//! The scan is monomorphized once in [`execute`] over [`MarkElem`] — the mark
//! element type: `u32` (the fast path; haystacks ≤ 4 GiB) or `usize` (the
//! fallback for larger inputs) — so the per-byte loop carries no dispatch.

use crate::automata::dfa::{DEAD_STATE, Dfa};
use crate::automata::nfa::{FULL_MATCH_END, FULL_MATCH_START, TEXT_POS_NO_MATCH, TextPos};
use crate::automata::nfa_backend::{NfaMatch, tags_to_captures};
use crate::automata::tdfa::{
    EXEC_ACCEPT_FLAG, EXEC_STATE_MASK, FinalCommand, MarkValue, TDFA_DEAD_STATE, TagCommand, Tdfa,
};
use crate::insn::StartPredicate;
use crate::util::DebugCheckIndex;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use smallvec::SmallVec;

/// A mark-file element. Marks hold byte offsets into the haystack (or the
/// `NO_MATCH` sentinel). `u32` is the SIMD-friendly fast path used when the
/// haystack fits in 4 GiB; `usize` is the fallback for larger inputs.
pub(crate) trait MarkElem: Copy + Ord {
    /// "Unset" sentinel — distinct from every valid offset.
    const NO_MATCH: Self;
    /// A valid input offset as a mark value. `p` is `< NO_MATCH` by construction
    /// (the dispatcher routes oversized inputs to a wider `MarkElem`).
    fn from_pos(p: usize) -> Self;
    /// This mark as a `usize` offset. Only meaningful for non-`NO_MATCH` values;
    /// finalization maps the sentinel separately.
    fn to_pos(self) -> usize;
}

impl MarkElem for u32 {
    const NO_MATCH: Self = u32::MAX;
    #[inline]
    fn from_pos(p: usize) -> Self {
        p as u32
    }
    #[inline]
    fn to_pos(self) -> usize {
        self as usize
    }
}

impl MarkElem for usize {
    const NO_MATCH: Self = usize::MAX;
    #[inline]
    fn from_pos(p: usize) -> Self {
        p
    }
    #[inline]
    fn to_pos(self) -> usize {
        self
    }
}

/// Compile-time switches that let [`execute_generic`] drop cold sites it can
/// statically prove the current automaton can't hit. Each realized combination
/// is one monomorphization; the dispatcher in [`execute`] picks one per scan
/// from the [`Tdfa`]'s contents, so the guarded branches const-fold away and
/// the hot loop carries no runtime check.
pub(crate) trait TdfaExecConfig {
    /// Some state carries a forward-branching anchor alt. When false, the
    /// per-byte + entry `maybe_switch_anchor_alt` calls are not emitted.
    const HAS_ANCHOR_ALTS: bool;
    /// Some state carries a `$`-style accept conditional. When false, the
    /// entry + per-byte + EOI `record_conditionals` calls are not emitted.
    const HAS_CONDITIONALS: bool;
}

/// Config marker parameterized directly on its flag bits, so adding a flag is a
/// new `const` param rather than a new named type per combination.
pub(crate) struct ExecConfig<const HAS_ANCHOR_ALTS: bool, const HAS_CONDITIONALS: bool>;

impl<const A: bool, const C: bool> TdfaExecConfig for ExecConfig<A, C> {
    const HAS_ANCHOR_ALTS: bool = A;
    const HAS_CONDITIONALS: bool = C;
}

/// A recorded accept candidate: `(match end, finalization commands, match
/// start)`. The finals slice borrows the `Tdfa` (per-state or per-conditional),
/// so no clone is needed; the marks snapshot lives in a separate reused buffer
/// updated only when this candidate wins (see `consider_accept`). The best
/// candidate (leftmost; smallest `match start`) drives `finalize` at scan end.
type LastAccept<'a, T> = Option<(usize, &'a [FinalCommand], T)>;

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

/// Pick the config-flag monomorphization (`HAS_ANCHOR_ALTS` × `HAS_CONDITIONALS`)
/// from the automaton's contents and run one anchored attempt. The flag check is
/// once per attempt; the const generic still drops the per-byte branches inside
/// `run_anchored`.
fn run_anchored_dyn<T: MarkElem>(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
    scratch: &mut Scratch<T>,
    warm: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    match (tdfa.has_anchor_alts(), tdfa.has_conditionals()) {
        (false, false) => {
            run_anchored::<T, ExecConfig<false, false>>(tdfa, input, start, scratch, warm)
        }
        (true, false) => {
            run_anchored::<T, ExecConfig<true, false>>(tdfa, input, start, scratch, warm)
        }
        (false, true) => {
            run_anchored::<T, ExecConfig<false, true>>(tdfa, input, start, scratch, warm)
        }
        (true, true) => run_anchored::<T, ExecConfig<true, true>>(tdfa, input, start, scratch, warm),
    }
}

/// The prefilter loop: skip to each candidate `pred` allows (at or after
/// `start`) and run the anchored automaton there, returning the first (leftmost)
/// match. The `scratch` is reused across every candidate; `skip`, when set,
/// warm-starts each attempt past the matched literal (see [`PrefixSkip`]).
fn run_prefiltered_dyn<T: MarkElem>(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
    pred: &StartPredicate,
    scratch: &mut Scratch<T>,
    skip: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    let mut pos = start;
    loop {
        let cand = pred.find_from(input, pos)?;
        if let Some(m) = run_anchored_dyn::<T>(tdfa, input, cand, scratch, skip) {
            // Anchored automaton: the match starts exactly at `cand`, and since
            // candidates are visited left to right this is the leftmost match.
            return Some(m);
        }
        // No match anchored here; advance past this candidate. Candidate offsets
        // are UTF-8 lead bytes (codepoint boundaries), so `+1` is safe.
        pos = cand + 1;
    }
}

/// `u32` marks can only address offsets `< u32::MAX` (reserving that value as the
/// `NO_MATCH` sentinel); larger haystacks fall back to `usize`. The common path
/// is `u32`, so [`execute_reuse`] lets a caller hand in a reused `u32` scratch.
#[inline]
fn needs_wide_marks(input: &[u8]) -> bool {
    input.len() >= u32::MAX as usize
}

/// Execute the TDFA against `input`, allocating fresh buffers. Returns the first
/// match (range + captures) or `None`. Used by tests and one-shot callers; the
/// match-iteration hot path uses [`execute_reuse`] with a caller-owned scratch.
pub fn execute(tdfa: &Tdfa, input: &[u8], start: usize) -> Option<NfaMatch> {
    let width = mark_file_width(tdfa);
    if needs_wide_marks(input) {
        run_anchored_dyn::<usize>(tdfa, input, start, &mut Scratch::new(width), None)
    } else {
        run_anchored_dyn::<u32>(tdfa, input, start, &mut Scratch::new(width), None)
    }
}

/// Like [`execute`], but reuses the caller-owned `u32` `scratch` (sized to
/// [`mark_file_width`]) instead of allocating — so a `find_iter` over many
/// matches stays allocation-free per match. Oversized (`>= 4 GiB`) inputs are
/// rare and still allocate a local `usize` scratch.
pub(crate) fn execute_reuse(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
    scratch: &mut Scratch<u32>,
) -> Option<NfaMatch> {
    if needs_wide_marks(input) {
        run_anchored_dyn::<usize>(tdfa, input, start, &mut Scratch::new(mark_file_width(tdfa)), None)
    } else {
        run_anchored_dyn::<u32>(tdfa, input, start, scratch, None)
    }
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
    scratch: &mut Scratch<u32>,
    skip: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    if needs_wide_marks(input) {
        run_anchored_dyn::<usize>(tdfa, input, start, &mut Scratch::new(mark_file_width(tdfa)), skip)
    } else {
        run_anchored_dyn::<u32>(tdfa, input, start, scratch, skip)
    }
}

/// Execute an **anchored** TDFA driven by a literal prefilter, reusing the
/// caller-owned `u32` `scratch`. `skip` warm-starts each verify past the matched
/// literal (see [`PrefixSkip`]).
pub(crate) fn execute_prefiltered_reuse(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
    pred: &StartPredicate,
    scratch: &mut Scratch<u32>,
    skip: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    if needs_wide_marks(input) {
        run_prefiltered_dyn::<usize>(
            tdfa,
            input,
            start,
            pred,
            &mut Scratch::new(mark_file_width(tdfa)),
            skip,
        )
    } else {
        run_prefiltered_dyn::<u32>(tdfa, input, start, pred, scratch, skip)
    }
}

/// Apply a `TagCommandList` to a mark file in place (scalar, two-phase). Used
/// for the cold command sites — entry, anchor alts, and `$`-conditionals —
/// which run at most once per scan or rarely; the per-byte transition path uses
/// the precompiled move sequences instead. Touches only the real-mark lanes
/// (`0..num_marks`); the trailing `clear`/`current_pos`/`scratch` lanes are
/// irrelevant here because `CurrentPos` writes use `current_pos` directly.
fn apply_cmds_scalar<T: MarkElem>(buf: &mut [T], cmds: &[TagCommand], current_pos: T) {
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
    let mut reads: SmallVec<[(usize, T); 8]> = SmallVec::new();
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
/// hot path allocation-free per match — only the returned `NfaMatch`'s own
/// captures Vec (empty for capture-free patterns) is per-match.
#[derive(Debug)]
pub(crate) struct Scratch<T: MarkElem> {
    /// The working mark file, mutated in place by each transition. Reset to
    /// `NO_MATCH` at the start of every `run_anchored`.
    src_buf: Box<[T]>,
    /// Snapshot of the winning accept's marks (copied in on replace only).
    best_snap: Box<[T]>,
    /// Scratch for applying a `$`-conditional's commands before snapshotting.
    cond_buf: Box<[T]>,
    /// Reusable per-tag value buffer for `finalize` (refilled each call). Holds
    /// `usize` offsets regardless of `T`, so it's not width-typed.
    tag_values: Vec<TextPos>,
}

#[cfg(feature = "tdfa-jit")]
impl Scratch<u32> {
    /// Raw pointer to the working mark file, handed to JIT-compiled capture
    /// code (which applies per-transition marks in place). Valid until the next
    /// mutation of `self`.
    pub(crate) fn src_buf_mut_ptr(&mut self) -> *mut u32 {
        self.src_buf.as_mut_ptr()
    }

    /// Raw pointer to the accept-snapshot buffer, handed to JIT-compiled capture
    /// code (which copies the live marks here on a fallback accept). Valid until
    /// the next mutation of `self`.
    pub(crate) fn best_snap_mut_ptr(&mut self) -> *mut u32 {
        self.best_snap.as_mut_ptr()
    }
}

impl<T: MarkElem> Scratch<T> {
    /// `width` = `num_marks + 3` (real marks, then `clear`, `current_pos`,
    /// `scratch`).
    pub(crate) fn new(width: usize) -> Self {
        Self {
            src_buf: vec![T::NO_MATCH; width].into_boxed_slice(),
            best_snap: vec![T::NO_MATCH; width].into_boxed_slice(),
            cond_buf: vec![T::NO_MATCH; width].into_boxed_slice(),
            tag_values: Vec::new(),
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
    if literal.is_empty() || tdfa.has_anchor_alts() || tdfa.has_conditionals() || !tdfa.has_moves()
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
    // No accept can fire strictly inside a mandatory prefix (that would mean a
    // match shorter than the literal exists), so intermediate accept checks are
    // unnecessary; the boundary accept at `P + len` is still performed by the
    // warm start. `state` is where the byte loop resumes.
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
    if tdfa.has_anchor_alts() || tdfa.has_conditionals() || !tdfa.has_moves() {
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
    let start = tdfa.start(1) as usize;

    let mut post: Option<u32> = None;
    for b in first_bytes {
        let class = *byte_to_class.iat(b as usize) as usize;
        let idx = start * num_classes + class;
        // Skipping the byte must not drop a mark write (no capture opening on
        // the leading byte), mirroring `compute_prefix_skip`.
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
            // Two admissible bytes lead to different post-states; one warm start
            // can't serve both, so fall back to a cold run.
            Some(_) => return None,
        }
    }
    // `post` is `None` only for an empty class (no admissible byte) — nothing to
    // warm-start from, so the `?` correctly declines.
    Some(PrefixSkip { post_state: post?, len: 1 })
}

/// One anchored attempt: run the automaton from byte offset `start`, reusing the
/// caller-owned `scratch`. Returns the match (range + captures) or `None`. `T`
/// is the mark element, fixed for the whole pass.
///
/// `warm`, when set, is a [`PrefixSkip`]: the start `start` is the literal's
/// offset and the run jumps to `warm.post_state`, resuming the byte loop at
/// `start + warm.len` instead of re-scanning the literal. It's only ever set
/// when the automaton has no anchor alts / conditionals (see
/// `compute_prefix_skip`), so those `C` branches are statically dead on the warm
/// path.
#[inline]
fn run_anchored<T: MarkElem, C: TdfaExecConfig>(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
    scratch: &mut Scratch<T>,
    warm: Option<PrefixSkip>,
) -> Option<NfaMatch> {
    let num_tags = tdfa.num_tags();
    let num_marks = tdfa.num_marks();
    let curpos_lane = num_marks + 1;
    // Loop-invariant: capture-free patterns skip the per-byte accept snapshot.
    let has_captures = tdfa.has_captures();

    // Pull the three mark buffers out of `scratch` (and the deref through the
    // `&mut Scratch`) into direct `&mut [T]` locals held on the stack for the
    // whole run. These are disjoint fields, so all three borrows coexist; the
    // hot per-byte loop then indexes a plain slice (one indirection) instead of
    // chasing `&mut Scratch` -> `Box<[T]>` -> data on every access. `tag_values`
    // is only touched once at the end.
    let src_buf: &mut [T] = &mut scratch.src_buf;
    let best_snap: &mut [T] = &mut scratch.best_snap;
    let cond_buf: &mut [T] = &mut scratch.cond_buf;
    let tag_values: &mut Vec<TextPos> = &mut scratch.tag_values;

    // Reset the working mark file (it's reused across candidates). The `clear`
    // lane must stay `NO_MATCH`, which the fill restores. `best_snap`/`cond_buf`
    // are always written before read, so they need no reset.
    src_buf.fill(T::NO_MATCH);

    apply_cmds_scalar::<T>(src_buf, tdfa.entry_commands(start), T::from_pos(start));

    let mut last_accept: LastAccept<T> = None;
    // Whether the winning accept's tags live in `src_buf` (read at scan end) or
    // were snapshotted into `best_snap`. Updated by every recorded accept.
    let mut read_live = false;
    // Per-state fallback flags: which accepting states need the eager snapshot.
    let accept_fallback = tdfa.accept_fallback();

    // Cold start: begin in the start state and scan from `start`. Warm start
    // (prefix-skip): begin in the post-literal state and scan from `start + len`
    // — the mark file is unchanged from entry because the literal writes none
    // (guaranteed by `compute_prefix_skip`).
    let (mut state, loop_start) = match warm {
        Some(s) => (s.post_state, start + s.len),
        None => (tdfa.start(start), start),
    };

    if state == TDFA_DEAD_STATE {
        return None;
    }
    // Initial position checks at `loop_start`. Cold path (`loop_start == start`):
    // multiline-`^` switch and the start-state empty-match accept. Warm path:
    // the boundary accept right after the literal (the interior accept checks it
    // skipped can't fire — see `compute_prefix_skip`); anchor-alt / conditional
    // branches are statically dead since warm is only set when neither exists.
    if C::HAS_ANCHOR_ALTS {
        maybe_switch_anchor_alt::<T>(tdfa, &mut state, src_buf, input, loop_start);
    }
    if *tdfa.accepting().iat(state as usize) {
        record_accept::<T>(
            &mut last_accept,
            best_snap,
            loop_start,
            src_buf,
            tdfa.finals().iat(state as usize),
            has_captures,
            C::HAS_CONDITIONALS || *accept_fallback.iat(state as usize),
            &mut read_live,
        );
    }
    if C::HAS_CONDITIONALS {
        record_conditionals::<T>(
            tdfa,
            state,
            input,
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

    // Whether to drive transitions with the precompiled move sequences (the
    // common path) or interpret the command lists directly. The latter covers
    // only the degenerate too-many-marks-for-`u16` case (see
    // `Tdfa::compile_moves_all`). Loop-invariant, so the branch predicts
    // perfectly.
    let use_moves = tdfa.has_moves();

    // When the pattern has no captures and `FULL_MATCH_START` is fixed at entry
    // (anchored/prefilter builds), no per-byte mark is ever read back: the match
    // is `[start, end]` with `start` from the untouched entry mark and `end` from
    // the accept position. So the whole transition-mark application is dead and we
    // drop it from the hot loop. Loop-invariant — predicts perfectly. (Capture or
    // `.*?`-scan runs keep applying marks.)
    let skip_marks = !has_captures && tdfa.start_fixed();

    // The capture-free fast loop: premultiplied transitions (no per-byte index
    // multiply) + accept-flagged targets (no `accepting[]` load) + no mark work.
    // Applicable exactly when `exec_transitions` was built (capture-free,
    // `start_fixed`, and — guaranteed by the dispatcher's const flags — no
    // conditionals or anchor alts). The match always starts at `start`, so accepts
    // record that directly.
    let use_fast = skip_marks
        && !C::HAS_CONDITIONALS
        && !C::HAS_ANCHOR_ALTS
        && !tdfa.exec_transitions().is_empty();

    // Position where `state` was last actually entered (see the long-standing
    // note below: EOI conditionals evaluate against this, not `input.len()`).
    let mut live_position = loop_start;

    if use_fast {
        // `state` is premultiplied here; `estate + class` indexes the table with a
        // bare add. No marks, no `accepting[]` load, no `finals` — the accept is a
        // bit test and the start is the run's `start`.
        let exec_trans = tdfa.exec_transitions();
        let mut estate = state * num_classes as u32;
        for (i, &byte) in input[loop_start..].iter().enumerate() {
            let class = *byte_to_class.iat(byte as usize) as u32;
            let raw = *exec_trans.iat((estate + class) as usize);
            if raw == TDFA_DEAD_STATE {
                break;
            }
            estate = raw & EXEC_STATE_MASK;
            if raw & EXEC_ACCEPT_FLAG != 0 {
                // Latest (longest) accept wins; start is fixed at `start`.
                last_accept = Some((loop_start + i + 1, &[], T::from_pos(start)));
                read_live = false;
            }
        }
    } else {
    for (i, &byte) in input[loop_start..].iter().enumerate() {
        let pos = loop_start + i;
        let class = *byte_to_class.iat(byte as usize) as usize;
        let idx = state as usize * num_classes + class;
        let next = *transitions.iat(idx);
        if next == TDFA_DEAD_STATE {
            break;
        }
        // Apply the transition's mark update in place. An empty move sequence is
        // the common tag-free case (marks untouched, skipped). Each move is
        // `buf[dst] = buf[src]`; the sequence is ordered so reads see pre-update
        // values, with `src` possibly naming the `current_pos` or `scratch` lane.
        if !skip_marks {
            if use_moves {
                let moves = trans_moves.iat(idx);
                if !moves.is_empty() {
                    *src_buf.mat(curpos_lane) = T::from_pos(pos + 1);
                    for op in moves.iter() {
                        let v = *src_buf.iat(op.src as usize);
                        *src_buf.mat(op.dst as usize) = v;
                    }
                }
            } else {
                apply_cmds_scalar::<T>(src_buf, trans_cmds.iat(idx), T::from_pos(pos + 1));
            }
        }
        state = next;
        // `live_position` only feeds the EOI conditional pass below, so the
        // per-byte store is dead weight unless this run has conditionals.
        if C::HAS_CONDITIONALS {
            live_position = pos + 1;
        }
        // Forward-branching anchor switch (mid-input multiline `^`). Guard on the
        // per-state list so the common (no-alt) state skips the call — most states
        // on a literal-verify path carry neither an alt nor a conditional.
        if C::HAS_ANCHOR_ALTS && !tdfa.anchor_alts(state).is_empty() {
            maybe_switch_anchor_alt::<T>(tdfa, &mut state, src_buf, input, pos + 1);
        }
        if *accepting.iat(state as usize) {
            record_accept::<T>(
                &mut last_accept,
                best_snap,
                pos + 1,
                src_buf,
                tdfa.finals().iat(state as usize),
                has_captures,
                C::HAS_CONDITIONALS || *accept_fallback.iat(state as usize),
                &mut read_live,
            );
        }
        if C::HAS_CONDITIONALS && !tdfa.anchor_conditionals(state).is_empty() {
            record_conditionals::<T>(
                tdfa,
                state,
                input,
                pos + 1,
                src_buf,
                cond_buf,
                &mut last_accept,
                best_snap,
                has_captures,
                &mut read_live,
            );
        }
    }
    }

    // EOI pass — `$` non-multiline naturally fires here; multiline `$` fires
    // here too if the previous-byte side of the predicate is satisfied. We use
    // `live_position` (not `input.len()`) so a state abandoned mid-input doesn't
    // falsely accept just because `pos == input.len()` satisfies `$`.
    if C::HAS_CONDITIONALS {
        record_conditionals::<T>(
            tdfa,
            state,
            input,
            live_position,
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
            // The winner's tags are in the live registers (`src_buf`, read here at
            // scan end) for cheap-recorded accepts, or in the eager snapshot
            // (`best_snap`) for fallback/conditional accepts.
            let marks: &[T] = if read_live { src_buf } else { best_snap };
            Some(if has_captures {
                finalize::<T>(finals, marks, end, num_tags, tag_values)
            } else {
                // Snapshot path stored the start; cheap path derives it now.
                let s = if read_live {
                    snapshot_match_start(finals, marks)
                } else {
                    start
                };
                finalize_nocap::<T>(s, end)
            })
        }
        None => None,
    }
}

/// If `state` has an anchor alt and its predicate holds at `pos`, apply the
/// alt's switch commands to `buf` (scalar, in place) and swap `state` to the
/// alt id. Cold: anchor alts are rare.
fn maybe_switch_anchor_alt<T: MarkElem>(
    tdfa: &Tdfa,
    state: &mut u32,
    buf: &mut [T],
    input: &[u8],
    pos: usize,
) {
    for alt in tdfa.anchor_alts(*state) {
        if alt.cond.holds(input, pos, &[]) {
            apply_cmds_scalar::<T>(buf, &alt.commands, T::from_pos(pos));
            *state = alt.alt;
            return;
        }
    }
}

/// For each `$`-style conditional attached to `state`, evaluate its predicate at
/// `pos`; on a hit, snapshot the marks into `cond_buf`, apply the conditional's
/// commands, and treat it as a new accept candidate. Cold: most states carry no
/// conditionals (the early-out covers the hot path).
#[allow(clippy::too_many_arguments)]
fn record_conditionals<'a, T: MarkElem>(
    tdfa: &'a Tdfa,
    state: u32,
    input: &[u8],
    pos: usize,
    marks: &[T],
    cond_buf: &mut [T],
    last_accept: &mut LastAccept<'a, T>,
    best_snap: &mut [T],
    has_captures: bool,
    read_live: &mut bool,
) {
    let conds = tdfa.anchor_conditionals(state);
    if conds.is_empty() {
        return;
    }
    for ac in conds {
        if !ac.cond.holds(input, pos, &[]) {
            continue;
        }
        cond_buf.copy_from_slice(marks);
        apply_cmds_scalar::<T>(cond_buf, &ac.commands, T::from_pos(pos));
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
/// `NO_MATCH` if the row doesn't set it. Used to order accept candidates by
/// match start so leftmost wins (see `consider_accept`).
fn snapshot_match_start<T: MarkElem>(finals: &[FinalCommand], marks: &[T]) -> T {
    for cmd in finals {
        if cmd.tag == FULL_MATCH_START {
            if let MarkValue::Copy(src) = cmd.src {
                return marks[src.0 as usize];
            }
        }
    }
    T::NO_MATCH
}

/// Record an accept candidate, keeping the **leftmost** match. A candidate
/// replaces the current best unless it starts strictly later (`FULL_MATCH_START`
/// greater): an earlier start always wins; an equal start (greedy extension of
/// the same match, or the latest-priority conditional at one position) replaces
/// so the longest/last-priority extent is taken.
///
/// Record an accept at a regular accepting state.
///
/// The Laurikari/TDFA insight: tag values live in registers, and `best_snap`
/// only ever yields the *final* winner — every intermediate snapshot is
/// overwritten. So for the common case we record only `(end, finals)` and read
/// the registers at scan end (`run_anchored`'s finalize). That's correct because
/// `truncate_at_first_goal` pruning fixes the leftmost start once a match is
/// found, so the **last** accept is the winner and the diverging higher-priority
/// continuations that run past it write *different* registers (the determinizer's
/// allocation), leaving the winner's registers intact.
///
/// The exception is the "fallback" case the snapshot was invented for: with a
/// `$`-style conditional, the unanchored prefix can stay alive past a completed
/// match and produce a later-start accept, so there we still snapshot eagerly
/// and keep the leftmost ([`consider_accept`]).
#[inline]
#[allow(clippy::too_many_arguments)]
fn record_accept<'a, T: MarkElem>(
    last_accept: &mut LastAccept<'a, T>,
    best_snap: &mut [T],
    end: usize,
    marks: &[T],
    finals: &'a [FinalCommand],
    has_captures: bool,
    snapshot: bool,
    read_live: &mut bool,
) {
    if snapshot {
        consider_accept(last_accept, best_snap, end, marks, finals, has_captures, read_live);
    } else {
        // No per-byte copy or start read: last accept wins, registers read at
        // scan end from the live `src_buf`.
        *last_accept = Some((end, finals, T::NO_MATCH));
        *read_live = true;
    }
}

/// The start is read from the live `marks` *before* any copy, so a non-replacing
/// candidate costs only the comparison — no snapshot copy. This guard matters
/// only for unanchored search, where the implicit prefix can keep a "still
/// searching" thread alive past a `$`-completed match; for anchored runs
/// `FULL_MATCH_START` is constant and this reduces to "latest accept wins".
///
/// `has_captures` is loop-invariant (const per scan): when false we skip the
/// per-byte `best_snap` copy entirely — the match is `start..end` with no
/// captures (`finalize_nocap`), so the snapshot is dead weight. This is the big
/// win for accept-heavy capture-free patterns like `.*`, which would otherwise
/// memcpy the mark file on every byte.
#[inline]
fn consider_accept<'a, T: MarkElem>(
    last_accept: &mut LastAccept<'a, T>,
    best_snap: &mut [T],
    end: usize,
    marks: &[T],
    finals: &'a [FinalCommand],
    has_captures: bool,
    read_live: &mut bool,
) {
    let new_start = snapshot_match_start(finals, marks);
    if let Some((_, _, best_start)) = last_accept {
        if new_start > *best_start {
            return;
        }
    }
    if has_captures {
        best_snap.copy_from_slice(marks);
    }
    *last_accept = Some((end, finals, new_start));
    // This accept's tags are in the snapshot (or, for nocap, in the stored
    // start), not the live registers.
    *read_live = false;
}

/// Build a capture-free match directly from the recorded start (the
/// `FULL_MATCH_START` snapshot value) and end — no mark file, no captures.
fn finalize_nocap<T: MarkElem>(start: T, end: usize) -> NfaMatch {
    let start = if start == T::NO_MATCH { 0 } else { start.to_pos() };
    NfaMatch {
        range: start..end,
        captures: Vec::new(),
    }
}

/// Build an `NfaMatch` from a finalization snapshot. `finals` is the row of
/// finalize-time commands (per-state for regular accepts, per-conditional for
/// `$`-fired accepts); `marks` is the snapshot taken at the accept; `end` is the
/// byte offset where the match ended. Mark values widen to `usize` here, mapping
/// the `T::NO_MATCH` sentinel onto `TEXT_POS_NO_MATCH`.
fn finalize<T: MarkElem>(
    finals: &[FinalCommand],
    marks: &[T],
    end: usize,
    num_tags: usize,
    tag_values: &mut Vec<TextPos>,
) -> NfaMatch {
    // Reused scratch: refill rather than allocate.
    tag_values.clear();
    tag_values.resize(num_tags, TEXT_POS_NO_MATCH);
    for cmd in finals {
        let val = match cmd.src {
            MarkValue::Copy(src) => marks[src.0 as usize],
            MarkValue::CurrentPos => unreachable!("finals never use CurrentPos"),
        };
        tag_values[cmd.tag as usize] = if val == T::NO_MATCH {
            TEXT_POS_NO_MATCH
        } else {
            val.to_pos()
        };
    }

    // Anchored match — the engine semantics are "match starts at 0" — but
    // FULL_MATCH_START / FULL_MATCH_END writes from the eps closure should
    // already encode that. Use them when present; otherwise fall back.
    let start_pos = tag_values[FULL_MATCH_START as usize];
    let end_pos = if tag_values[FULL_MATCH_END as usize] == TEXT_POS_NO_MATCH {
        // Committed-accept sentinel finals are all-Nil; synthesize from `end`.
        end
    } else {
        tag_values[FULL_MATCH_END as usize]
    };
    let start_pos = if start_pos == TEXT_POS_NO_MATCH {
        0
    } else {
        start_pos
    };

    let captures = tags_to_captures(tag_values);
    NfaMatch {
        range: start_pos..end_pos,
        captures,
    }
}

/// JIT capture-path setup: reset the working mark file to the unset sentinel and
/// apply the automaton's entry commands for `start`. The JIT-compiled code then
/// applies per-transition marks in place; [`jit_finalize`] reads them back. This
/// mirrors `run_anchored`'s pre-loop `src_buf.fill` + entry-command application.
#[cfg(feature = "tdfa-jit")]
pub(crate) fn jit_prepare_marks(tdfa: &Tdfa, scratch: &mut Scratch<u32>, start: usize) {
    scratch.src_buf.fill(<u32 as MarkElem>::NO_MATCH);
    apply_cmds_scalar::<u32>(
        &mut scratch.src_buf,
        tdfa.entry_commands(start),
        <u32 as MarkElem>::from_pos(start),
    );
}

/// JIT capture-path finalize: build the winning `NfaMatch` for the accept at
/// `state`/`end`. `read_live` selects the buffer holding the winner's marks —
/// the live `src_buf` (the common case, a non-fallback accept whose registers
/// survive to scan end) or the eager `best_snap` taken at a fallback accept
/// (Laurikari). Reuses the interpreter's [`finalize`].
#[cfg(feature = "tdfa-jit")]
pub(crate) fn jit_finalize(
    tdfa: &Tdfa,
    state: u32,
    scratch: &mut Scratch<u32>,
    end: usize,
    read_live: bool,
) -> NfaMatch {
    let marks: &[u32] = if read_live {
        &scratch.src_buf
    } else {
        &scratch.best_snap
    };
    finalize::<u32>(
        &tdfa.finals()[state as usize],
        marks,
        end,
        tdfa.num_tags(),
        &mut scratch.tag_values,
    )
}
