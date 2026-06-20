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
use crate::automata::tdfa::{FinalCommand, MarkValue, TDFA_DEAD_STATE, TagCommand, Tdfa};
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
) -> Option<NfaMatch> {
    match (tdfa.has_anchor_alts(), tdfa.has_conditionals()) {
        (false, false) => run_anchored::<T, ExecConfig<false, false>>(tdfa, input, start, scratch),
        (true, false) => run_anchored::<T, ExecConfig<true, false>>(tdfa, input, start, scratch),
        (false, true) => run_anchored::<T, ExecConfig<false, true>>(tdfa, input, start, scratch),
        (true, true) => run_anchored::<T, ExecConfig<true, true>>(tdfa, input, start, scratch),
    }
}

/// The prefilter loop: skip to each candidate `pred` allows (at or after
/// `start`) and run the anchored automaton there, returning the first (leftmost)
/// match. The `scratch` is reused across every candidate.
fn run_prefiltered_dyn<T: MarkElem>(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
    pred: &StartPredicate,
    scratch: &mut Scratch<T>,
) -> Option<NfaMatch> {
    let mut pos = start;
    loop {
        let cand = pred.find_from(input, pos)?;
        if let Some(m) = run_anchored_dyn::<T>(tdfa, input, cand, scratch) {
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
        run_anchored_dyn::<usize>(tdfa, input, start, &mut Scratch::new(width))
    } else {
        run_anchored_dyn::<u32>(tdfa, input, start, &mut Scratch::new(width))
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
        run_anchored_dyn::<usize>(tdfa, input, start, &mut Scratch::new(mark_file_width(tdfa)))
    } else {
        run_anchored_dyn::<u32>(tdfa, input, start, scratch)
    }
}

/// Execute an **anchored** TDFA driven by a literal prefilter, allocating fresh
/// buffers (see [`execute_prefiltered_reuse`] for the reused-scratch variant).
pub fn execute_prefiltered(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
    pred: &StartPredicate,
) -> Option<NfaMatch> {
    let width = mark_file_width(tdfa);
    if needs_wide_marks(input) {
        run_prefiltered_dyn::<usize>(tdfa, input, start, pred, &mut Scratch::new(width))
    } else {
        run_prefiltered_dyn::<u32>(tdfa, input, start, pred, &mut Scratch::new(width))
    }
}

/// Like [`execute_prefiltered`], but reuses the caller-owned `u32` `scratch`.
pub(crate) fn execute_prefiltered_reuse(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
    pred: &StartPredicate,
    scratch: &mut Scratch<u32>,
) -> Option<NfaMatch> {
    if needs_wide_marks(input) {
        run_prefiltered_dyn::<usize>(
            tdfa,
            input,
            start,
            pred,
            &mut Scratch::new(mark_file_width(tdfa)),
        )
    } else {
        run_prefiltered_dyn::<u32>(tdfa, input, start, pred, scratch)
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

/// One anchored attempt: run the automaton from byte offset `start`, reusing the
/// caller-owned `scratch`. Returns the match (range + captures) or `None`. `T`
/// is the mark element, fixed for the whole pass.
#[inline]
fn run_anchored<T: MarkElem, C: TdfaExecConfig>(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
    scratch: &mut Scratch<T>,
) -> Option<NfaMatch> {
    let num_tags = tdfa.num_tags();
    let num_marks = tdfa.num_marks();
    let curpos_lane = num_marks + 1;

    // Reset the working mark file (it's reused across candidates). The `clear`
    // lane must stay `NO_MATCH`, which the fill restores. `best_snap`/`cond_buf`
    // are always written before read, so they need no reset.
    scratch.src_buf.fill(T::NO_MATCH);

    apply_cmds_scalar::<T>(
        &mut scratch.src_buf,
        tdfa.entry_commands(start),
        T::from_pos(start),
    );

    let mut state = tdfa.start(start);
    let mut last_accept: LastAccept<T> = None;

    if state == TDFA_DEAD_STATE {
        return None;
    }
    // Initial multiline-`^` check: if `start > 0` and the previous byte is a
    // line terminator, switch to the alt right at the start of execution.
    if C::HAS_ANCHOR_ALTS {
        maybe_switch_anchor_alt::<T>(tdfa, &mut state, &mut scratch.src_buf, input, start);
    }
    if *tdfa.accepting().iat(state as usize) {
        consider_accept(
            &mut last_accept,
            &mut scratch.best_snap,
            start,
            &scratch.src_buf,
            tdfa.finals().iat(state as usize),
        );
    }
    if C::HAS_CONDITIONALS {
        record_conditionals::<T>(
            tdfa,
            state,
            input,
            start,
            &scratch.src_buf,
            &mut scratch.cond_buf,
            &mut last_accept,
            &mut scratch.best_snap,
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

    // Position where `state` was last actually entered (see the long-standing
    // note below: EOI conditionals evaluate against this, not `input.len()`).
    let mut live_position = start;

    for (i, &byte) in input[start..].iter().enumerate() {
        let pos = start + i;
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
        if use_moves {
            let moves = trans_moves.iat(idx);
            if !moves.is_empty() {
                *scratch.src_buf.mat(curpos_lane) = T::from_pos(pos + 1);
                for op in moves.iter() {
                    let v = *scratch.src_buf.iat(op.src as usize);
                    *scratch.src_buf.mat(op.dst as usize) = v;
                }
            }
        } else {
            apply_cmds_scalar::<T>(&mut scratch.src_buf, trans_cmds.iat(idx), T::from_pos(pos + 1));
        }
        state = next;
        live_position = pos + 1;
        // Forward-branching anchor switch (mid-input multiline `^`).
        if C::HAS_ANCHOR_ALTS {
            maybe_switch_anchor_alt::<T>(tdfa, &mut state, &mut scratch.src_buf, input, pos + 1);
        }
        if *accepting.iat(state as usize) {
            consider_accept(
                &mut last_accept,
                &mut scratch.best_snap,
                pos + 1,
                &scratch.src_buf,
                tdfa.finals().iat(state as usize),
            );
        }
        if C::HAS_CONDITIONALS {
            record_conditionals::<T>(
                tdfa,
                state,
                input,
                pos + 1,
                &scratch.src_buf,
                &mut scratch.cond_buf,
                &mut last_accept,
                &mut scratch.best_snap,
            );
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
            &scratch.src_buf,
            &mut scratch.cond_buf,
            &mut last_accept,
            &mut scratch.best_snap,
        );
    }

    last_accept.map(|(end, finals, _)| {
        finalize::<T>(
            finals,
            &scratch.best_snap,
            end,
            num_tags,
            &mut scratch.tag_values,
        )
    })
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
        consider_accept(last_accept, best_snap, pos, cond_buf, &ac.finals);
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
/// The start is read from the live `marks` *before* any copy, so a non-replacing
/// candidate costs only the comparison — no snapshot copy. This guard matters
/// only for unanchored search, where the implicit prefix can keep a "still
/// searching" thread alive past a `$`-completed match; for anchored runs
/// `FULL_MATCH_START` is constant and this reduces to "latest accept wins".
fn consider_accept<'a, T: MarkElem>(
    last_accept: &mut LastAccept<'a, T>,
    best_snap: &mut [T],
    end: usize,
    marks: &[T],
    finals: &'a [FinalCommand],
) {
    let new_start = snapshot_match_start(finals, marks);
    if let Some((_, _, best_start)) = last_accept {
        if new_start > *best_start {
            return;
        }
    }
    best_snap.copy_from_slice(marks);
    *last_accept = Some((end, finals, new_start));
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
