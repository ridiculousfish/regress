//! TDFA execution backend.

use crate::automata::dfa::{DEAD_STATE, Dfa};
use crate::automata::nfa::{FULL_MATCH_END, FULL_MATCH_START, TEXT_POS_NO_MATCH, TextPos};
use crate::automata::nfa_backend::{NfaMatch, tags_to_captures};
use crate::automata::tdfa::{
    FinalCommand, InputMark, MarkValue, TDFA_DEAD_STATE, TagCommand, TagCommandList, Tdfa,
};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use smallvec::SmallVec;

/// A recorded accept candidate: `(match end, marks snapshot at the accept,
/// finalization commands)`. The best one (leftmost; see `consider_accept`)
/// drives `finalize` at scan end.
type LastAccept = Option<(usize, Vec<TextPos>, SmallVec<[FinalCommand; 4]>)>;

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

/// Execute the TDFA against `input`. Returns the first match (range +
/// captures) or `None`.
///
/// Tracks marks in a flat `marks` array. Each transition applies its
/// `transition_commands` to update marks. On every accepting state visit, the
/// mark snapshot is cloned so finalization can read the values that were live
/// *at* the accept (later transitions may overwrite them). On end-of-scan, the
/// last accept's snapshot is fed through that state's `finals` to populate a
/// per-tag value array, which then unpacks into captures.
pub fn execute(tdfa: &Tdfa, input: &[u8], start: usize) -> Option<NfaMatch> {
    let num_tags = tdfa.num_tags();
    let mut marks: Vec<TextPos> = vec![TEXT_POS_NO_MATCH; tdfa.num_marks()];
    apply_commands(&mut marks, tdfa.entry_commands(start), start);

    let mut state = tdfa.start(start);
    // The finals row to use at scan end. Cloned at each new accept so the
    // values that were live *at the accept* drive finalization (later
    // transitions / conditionals may overwrite them).
    let mut last_accept: LastAccept = None;

    if state == TDFA_DEAD_STATE {
        return None;
    }
    // Initial multiline-^ check: if `start > 0` and the previous byte is a
    // line terminator, switch to the alt right at the start of execution.
    maybe_switch_anchor_alt(tdfa, &mut state, &mut marks, input, start);
    if tdfa.accepting()[state as usize] {
        consider_accept(
            &mut last_accept,
            start,
            marks.clone(),
            tdfa.finals()[state as usize].clone(),
        );
    }
    record_conditionals(tdfa, state, input, start, &marks, &mut last_accept);

    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let trans_cmds: &[TagCommandList] = tdfa.transition_commands();
    let accepting = tdfa.accepting();
    let num_classes = tdfa.num_classes();

    // Position where `state` was last actually entered. Advances on each
    // successful byte transition. After breaking out of the loop on DEAD
    // this stays at the position we last advanced TO — *not* at the
    // failed-transition position. The EOI `record_conditionals` evaluates
    // its predicate against `live_position`, so a state abandoned mid-
    // input doesn't falsely accept just because `pos == input.len()`
    // happens to satisfy `$`.
    let mut live_position = start;

    for (i, &byte) in input[start..].iter().enumerate() {
        let pos = start + i;
        let class = byte_to_class[byte as usize] as usize;
        let idx = state as usize * num_classes + class;
        let next = transitions[idx];
        if next == TDFA_DEAD_STATE {
            break;
        }
        apply_commands(&mut marks, &trans_cmds[idx], pos + 1);
        state = next;
        live_position = pos + 1;
        // Forward-branching anchor switch: if `state` has a multiline-^
        // alt and the predicate holds at the new position, swap to it.
        maybe_switch_anchor_alt(tdfa, &mut state, &mut marks, input, pos + 1);
        if accepting[state as usize] {
            consider_accept(
                &mut last_accept,
                pos + 1,
                marks.clone(),
                tdfa.finals()[state as usize].clone(),
            );
        }
        record_conditionals(tdfa, state, input, pos + 1, &marks, &mut last_accept);
    }

    // EOI pass — `$` non-multiline naturally fires here; multiline `$` fires
    // here too if the previous-byte side of the predicate is satisfied.
    // We use `live_position` (see above) rather than `input.len()`.
    record_conditionals(tdfa, state, input, live_position, &marks, &mut last_accept);

    last_accept.map(|(end, snap, finals)| finalize(&finals, &snap, end, num_tags))
}

/// If `state` has an anchor alt and its predicate holds at `pos`, apply
/// the alt's switch commands to `marks` and swap `state` to the alt id.
/// Used both at the start of execution (catches multiline `^` firing
/// at a non-zero start offset whose preceding byte is a line terminator)
/// and after every byte transition (catches mid-input firings).
fn maybe_switch_anchor_alt(
    tdfa: &Tdfa,
    state: &mut u32,
    marks: &mut [TextPos],
    input: &[u8],
    pos: usize,
) {
    for alt in tdfa.anchor_alts(*state) {
        if alt.cond.holds(input, pos, &[]) {
            apply_commands(marks, &alt.commands, pos);
            *state = alt.alt;
            return;
        }
    }
}

/// For each `$`-style conditional attached to `state`, evaluate its
/// predicate at `pos`; on hit, snapshot the marks, apply the conditional's
/// commands, and treat it as a new `last_accept` candidate. Priority order
/// within the conditional list mirrors the eps source order from
/// construction; later updates here intentionally overwrite earlier ones
/// at the same step, matching the existing "latest accept wins" semantics
/// of the regular accept path.
fn record_conditionals(
    tdfa: &Tdfa,
    state: u32,
    input: &[u8],
    pos: usize,
    marks: &[TextPos],
    last_accept: &mut LastAccept,
) {
    let conds = tdfa.anchor_conditionals(state);
    if conds.is_empty() {
        return;
    }
    for ac in conds {
        if !ac.cond.holds(input, pos, &[]) {
            continue;
        }
        let mut snap: Vec<TextPos> = marks.to_vec();
        apply_commands(&mut snap, &ac.commands, pos);
        consider_accept(last_accept, pos, snap, ac.finals.clone());
    }
}

/// Read the `FULL_MATCH_START` value a finalization snapshot would produce,
/// or `TEXT_POS_NO_MATCH` if the row doesn't set it. Used to order accept
/// candidates by match start so leftmost wins (see `consider_accept`).
fn snapshot_match_start(finals: &[FinalCommand], marks: &[TextPos]) -> TextPos {
    for cmd in finals {
        if cmd.tag == FULL_MATCH_START {
            if let MarkValue::Copy(src) = cmd.src {
                return marks[src.0 as usize];
            }
        }
    }
    TEXT_POS_NO_MATCH
}

/// Record an accept candidate, keeping the **leftmost** match. A candidate
/// replaces the current best unless it starts strictly later
/// (`FULL_MATCH_START` greater): an earlier start always wins, and an equal
/// start (greedy extension of the same match, or the latest-priority
/// conditional at one position) replaces so the longest/last-priority extent
/// is taken — matching the "latest accept wins" rule within a single start.
///
/// This guard matters only for unanchored search, where the implicit prefix
/// can keep a lower-priority "still searching" thread alive past a completed
/// match whose accept came via an `anchor_conditional` (e.g. `$`), so a
/// later-starting match could otherwise overwrite the correct one. For
/// anchored execution `FULL_MATCH_START` is constant across a run, so every
/// candidate compares equal and this reduces to "latest accept wins".
fn consider_accept(
    last_accept: &mut LastAccept,
    end: usize,
    snap: Vec<TextPos>,
    finals: SmallVec<[FinalCommand; 4]>,
) {
    let new_start = snapshot_match_start(&finals, &snap);
    if let Some((_, best_snap, best_finals)) = last_accept {
        if new_start > snapshot_match_start(best_finals, best_snap) {
            return;
        }
    }
    *last_accept = Some((end, snap, finals));
}

/// Apply a sequence of `TagCommand`s to the marks array. CurrentPos and Nil
/// are direct writes; Copy is two-phased so a list of copies behaves like a
/// simultaneous assignment (potential cycles or shared-source reads work).
fn apply_commands(marks: &mut [TextPos], cmds: &[TagCommand], current_pos: TextPos) {
    if cmds.is_empty() {
        return;
    }
    // Phase 1: CurrentPos / Nil writes. These populate the slots that any
    // sibling Copy commands may read from.
    for cmd in cmds {
        match cmd.src {
            MarkValue::CurrentPos => marks[cmd.dst.0 as usize] = current_pos,
            MarkValue::Copy(_) => {}
        }
    }
    // Phase 2: Copy writes. Pre-read all sources before any write so cyclic
    // or shared-source copies behave as a simultaneous assignment.
    let mut copy_reads: Vec<(InputMark, TextPos)> = Vec::new();
    for cmd in cmds {
        if let MarkValue::Copy(src) = cmd.src {
            copy_reads.push((cmd.dst, marks[src.0 as usize]));
        }
    }
    for (dst, val) in copy_reads {
        marks[dst.0 as usize] = val;
    }
}

/// Build an `NfaMatch` from a finalization snapshot. `finals` is the
/// row of finalize-time commands (per-state for regular accepts, per-
/// conditional for `$`-fired accepts); `marks` is the snapshot taken at
/// the accept; `end` is the byte offset where the match ended.
fn finalize(finals: &[FinalCommand], marks: &[TextPos], end: usize, num_tags: usize) -> NfaMatch {
    let mut tag_values = vec![TEXT_POS_NO_MATCH; num_tags];
    for cmd in finals {
        let val = match cmd.src {
            MarkValue::Copy(src) => marks[src.0 as usize],
            MarkValue::CurrentPos => unreachable!("finals never use CurrentPos"),
        };
        tag_values[cmd.tag as usize] = val;
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
