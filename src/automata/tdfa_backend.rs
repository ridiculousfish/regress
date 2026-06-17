//! TDFA execution backend.
//!
//! The hot path recasts each transition's mark update as a single data-parallel
//! **gather**: the determinizer precompiles every transition's `TagCommandList`
//! into a [`ShuffleVec`](crate::automata::tdfa::ShuffleVec) over a mark file
//! laid out as `[marks[0..num_marks], clear, current_pos]`, and the executor
//! applies it with one permute (`out[i] = src[shuffle[i]]`). This removes the
//! per-transition allocation and two-phase command interpretation that used to
//! dominate; the simultaneous-assignment semantics fall out of the gather.
//!
//! The whole scan is generic over two axes, chosen once in [`execute`] and then
//! monomorphized so the per-byte loop carries no dispatch:
//! - [`MarkElem`] — the mark element type: `u32` (the fast path; haystacks
//!   ≤ 4 GiB) or `usize` (the fallback for larger inputs).
//! - [`Permute`] — the gather strategy: [`ScalarGather`] (portable reference)
//!   or a SIMD impl chosen by target/feature.

use crate::automata::dfa::{DEAD_STATE, Dfa};
use crate::automata::nfa::{FULL_MATCH_END, FULL_MATCH_START, TEXT_POS_NO_MATCH};
use crate::automata::nfa_backend::{NfaMatch, tags_to_captures};
use crate::automata::tdfa::{FinalCommand, MarkValue, TDFA_DEAD_STATE, TagCommand, Tdfa};
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

/// A gather strategy: `out[i] = src[idx[i]]` for the whole mark file at once.
/// `idx` is a transition's [`ShuffleVec`](crate::automata::tdfa::ShuffleVec);
/// `src` and `out` are mark files of equal length (`num_marks + 2`).
pub(crate) trait Permute<T: MarkElem> {
    fn apply(src: &[T], idx: &[u16], out: &mut [T]);
}

/// Portable reference gather. Correct for every `T` and arch; also the oracle
/// the SIMD impls are differentially tested against.
pub(crate) struct ScalarGather;

impl<T: MarkElem> Permute<T> for ScalarGather {
    #[inline]
    fn apply(src: &[T], idx: &[u16], out: &mut [T]) {
        for (o, &i) in out.iter_mut().zip(idx) {
            *o = src[i as usize];
        }
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
}

/// Config marker parameterized directly on its flag bits, so adding a flag is a
/// new `const` param rather than a new named type per combination.
pub(crate) struct ExecConfig<const HAS_ANCHOR_ALTS: bool>;

impl<const A: bool> TdfaExecConfig for ExecConfig<A> {
    const HAS_ANCHOR_ALTS: bool = A;
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

/// Execute the TDFA against `input`. Returns the first match (range + captures)
/// or `None`.
///
/// This is the dispatch wrapper: it picks the mark width and gather strategy
/// **once** (based on haystack size, the mark-file width, and target features)
/// and calls the monomorphized [`execute_generic`]. The chosen body has no
/// per-byte dispatch.
pub fn execute(tdfa: &Tdfa, input: &[u8], start: usize) -> Option<NfaMatch> {
    // Cross the mark width with the config-flag combinations, picking one
    // monomorphization from the automaton's contents. Each `$flag` is a `bool`
    // read off `tdfa`; the macro fans it out to a `true`/`false` match so the
    // const generic is known at the call site. New flags are added by listing
    // another `$flag = $value` pair (the arm count doubles per flag).
    macro_rules! dispatch {
        ($t:ty; $($flag:ident = $value:expr),* $(,)?) => {
            dispatch!(@width $t; [$($flag = $value),*] [])
        };
        // Peel one flag: expand to a `match` over its two `bool` values, then
        // recurse with the chosen literal appended to the resolved list.
        (@width $t:ty; [$flag:ident = $value:expr $(, $rflag:ident = $rvalue:expr)*] [$($done:tt)*]) => {
            match $value {
                true => dispatch!(@width $t; [$($rflag = $rvalue),*] [$($done)* true,]),
                false => dispatch!(@width $t; [$($rflag = $rvalue),*] [$($done)* false,]),
            }
        };
        // All flags resolved to literals: emit the monomorphized call.
        (@width $t:ty; [] [$($lit:tt)*]) => {
            execute_generic::<ScalarGather, $t, ExecConfig<$($lit)*>>(tdfa, input, start)
        };
    }

    // `u32` marks can only address offsets `< u32::MAX` (reserving that value as
    // the `NO_MATCH` sentinel). For larger haystacks fall back to `usize`.
    let has_anchor_alts = tdfa.has_anchor_alts();
    if input.len() >= u32::MAX as usize {
        return dispatch!(usize; HAS_ANCHOR_ALTS = has_anchor_alts);
    }
    dispatch!(u32; HAS_ANCHOR_ALTS = has_anchor_alts)
}

/// Apply a `TagCommandList` to a mark file in place (scalar, two-phase). Used
/// for the cold command sites — entry, anchor alts, and `$`-conditionals —
/// which run at most once per scan or rarely; the per-byte transition path uses
/// the precompiled gather instead. Touches only the real-mark lanes
/// (`0..num_marks`); the trailing `clear`/`current_pos` lanes are irrelevant
/// here because `CurrentPos` writes use `current_pos` directly.
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

/// The monomorphized scan. `P` is the gather strategy and `T` the mark element;
/// both are fixed for the whole pass.
fn execute_generic<P: Permute<T>, T: MarkElem, C: TdfaExecConfig>(
    tdfa: &Tdfa,
    input: &[u8],
    start: usize,
) -> Option<NfaMatch> {
    let num_tags = tdfa.num_tags();
    let num_marks = tdfa.num_marks();
    // Mark file: real marks `0..num_marks`, then `clear` and `current_pos`.
    let width = num_marks + 2;
    let curpos_lane = num_marks + 1;

    // Double buffer: a gather reads `src_buf` and writes `dst_buf`, then they
    // swap. The `clear` lane stays `NO_MATCH` (identity preserves it). Both
    // allocated once per scan — not per transition.
    let mut src_buf = vec![T::NO_MATCH; width].into_boxed_slice();
    let mut dst_buf = vec![T::NO_MATCH; width].into_boxed_slice();
    // Reused snapshot of the winning accept's marks (copied in on replace only).
    let mut best_snap = vec![T::NO_MATCH; width].into_boxed_slice();
    // Scratch for applying a `$`-conditional's commands before snapshotting.
    let mut cond_buf = vec![T::NO_MATCH; width].into_boxed_slice();

    apply_cmds_scalar::<T>(&mut src_buf, tdfa.entry_commands(start), T::from_pos(start));

    let mut state = tdfa.start(start);
    let mut last_accept: LastAccept<T> = None;

    if state == TDFA_DEAD_STATE {
        return None;
    }
    // Initial multiline-`^` check: if `start > 0` and the previous byte is a
    // line terminator, switch to the alt right at the start of execution.
    if C::HAS_ANCHOR_ALTS {
        maybe_switch_anchor_alt::<T>(tdfa, &mut state, &mut src_buf, input, start);
    }
    if tdfa.accepting()[state as usize] {
        consider_accept(
            &mut last_accept,
            &mut best_snap,
            start,
            &src_buf,
            &tdfa.finals()[state as usize],
        );
    }
    record_conditionals::<T>(
        tdfa,
        state,
        input,
        start,
        &src_buf,
        &mut cond_buf,
        &mut last_accept,
        &mut best_snap,
    );

    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let trans_shuffles = tdfa.transition_shuffles();
    let trans_cmds = tdfa.transition_commands();
    let accepting = tdfa.accepting();
    let num_classes = tdfa.num_classes();

    // Whether to drive transitions with the precompiled gather (the fast path)
    // or interpret the command lists directly. The latter covers automata whose
    // mark file is too large for compiled shuffles (see `Tdfa::compile_shuffles`).
    // Loop-invariant, so the branch predicts perfectly.
    let use_gather = tdfa.has_shuffles();

    // Position where `state` was last actually entered (see the long-standing
    // note below: EOI conditionals evaluate against this, not `input.len()`).
    let mut live_position = start;

    for (i, &byte) in input[start..].iter().enumerate() {
        let pos = start + i;
        let class = byte_to_class[byte as usize] as usize;
        let idx = state as usize * num_classes + class;
        let next = transitions[idx];
        if next == TDFA_DEAD_STATE {
            break;
        }
        // Apply the transition's mark update. With shuffles, one gather (an
        // empty shuffle is the identity — marks untouched, the common tag-free
        // case — so we skip the gather + swap). Without, interpret the commands
        // in place.
        if use_gather {
            let shuf = &trans_shuffles[idx];
            if !shuf.is_empty() {
                src_buf[curpos_lane] = T::from_pos(pos + 1);
                P::apply(&src_buf, shuf, &mut dst_buf);
                core::mem::swap(&mut src_buf, &mut dst_buf);
            }
        } else {
            apply_cmds_scalar::<T>(&mut src_buf, &trans_cmds[idx], T::from_pos(pos + 1));
        }
        state = next;
        live_position = pos + 1;
        // Forward-branching anchor switch (mid-input multiline `^`).
        if C::HAS_ANCHOR_ALTS {
            maybe_switch_anchor_alt::<T>(tdfa, &mut state, &mut src_buf, input, pos + 1);
        }
        if accepting[state as usize] {
            consider_accept(
                &mut last_accept,
                &mut best_snap,
                pos + 1,
                &src_buf,
                &tdfa.finals()[state as usize],
            );
        }
        record_conditionals::<T>(
            tdfa,
            state,
            input,
            pos + 1,
            &src_buf,
            &mut cond_buf,
            &mut last_accept,
            &mut best_snap,
        );
    }

    // EOI pass — `$` non-multiline naturally fires here; multiline `$` fires
    // here too if the previous-byte side of the predicate is satisfied. We use
    // `live_position` (not `input.len()`) so a state abandoned mid-input doesn't
    // falsely accept just because `pos == input.len()` satisfies `$`.
    record_conditionals::<T>(
        tdfa,
        state,
        input,
        live_position,
        &src_buf,
        &mut cond_buf,
        &mut last_accept,
        &mut best_snap,
    );

    last_accept.map(|(end, finals, _)| finalize::<T>(finals, &best_snap, end, num_tags))
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
) -> NfaMatch {
    let mut tag_values = vec![TEXT_POS_NO_MATCH; num_tags];
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

    let captures = tags_to_captures(&tag_values);
    NfaMatch {
        range: start_pos..end_pos,
        captures,
    }
}
