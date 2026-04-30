//! TDFA execution backend. M3: anchored match with full capture tracking.

use crate::automata::nfa::{FULL_MATCH_END, FULL_MATCH_START, TEXT_POS_NO_MATCH, TextPos};
use crate::automata::nfa_backend::{NfaMatch, tags_to_captures};
use crate::automata::tdfa::{
    InputMark, MarkValue, TDFA_COMMITTED_ACCEPT_STATE, TDFA_LAST_SENTINEL, TagCommand, Tdfa,
};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ops::Range;

/// Anchored leftmost match. Returns the byte range of the match if any. Thin
/// wrapper around `execute_anchored_match` for callers that don't need
/// captures.
pub fn execute_anchored(tdfa: &Tdfa, input: &[u8]) -> Option<Range<usize>> {
    execute_anchored_match(tdfa, input).map(|m| m.range)
}

/// Anchored leftmost match returning a full `NfaMatch` (range + captures).
///
/// Tracks marks in a flat `marks` array. Each transition applies its
/// `transition_commands` to update marks. On every accepting state visit, the
/// mark snapshot is cloned so finalization can read the values that were live
/// *at* the accept (later transitions may overwrite them). On end-of-scan, the
/// last accept's snapshot is fed through that state's `finals` to populate a
/// per-tag value array, which then unpacks into captures.
///
/// The mark-snapshot clone per accept is the obviously-correct Phase A
/// baseline; Phase B's register allocation will eliminate it by guaranteeing
/// no two live marks share a slot.
pub fn execute_anchored_match(tdfa: &Tdfa, input: &[u8]) -> Option<NfaMatch> {
    let num_tags = tdfa.num_tags();
    let mut marks: Vec<TextPos> = vec![TEXT_POS_NO_MATCH; tdfa.num_marks()];
    apply_commands(&mut marks, tdfa.entry_commands(), 0);

    let mut state = tdfa.start();
    let mut last_accept: Option<(usize, u32, Vec<TextPos>)> = None;

    if state <= TDFA_LAST_SENTINEL {
        return if state == TDFA_COMMITTED_ACCEPT_STATE {
            Some(finalize(tdfa, state, &marks, input.len(), num_tags))
        } else {
            None
        };
    }
    if tdfa.accepting()[state as usize] {
        last_accept = Some((0, state, marks.clone()));
    }

    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let trans_cmds = tdfa.transition_commands();
    let accepting = tdfa.accepting();
    let num_classes = tdfa.num_classes();

    for (i, &byte) in input.iter().enumerate() {
        let class = byte_to_class[byte as usize] as usize;
        let idx = state as usize * num_classes + class;
        let next = transitions[idx];
        if next <= TDFA_LAST_SENTINEL {
            if next == TDFA_COMMITTED_ACCEPT_STATE {
                apply_commands(&mut marks, &trans_cmds[idx], i + 1);
                return Some(finalize(tdfa, next, &marks, input.len(), num_tags));
            }
            break;
        }
        apply_commands(&mut marks, &trans_cmds[idx], i + 1);
        state = next;
        if accepting[state as usize] {
            last_accept = Some((i + 1, state, marks.clone()));
        }
    }

    last_accept.map(|(end, accept_state, snap)| finalize(tdfa, accept_state, &snap, end, num_tags))
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
            MarkValue::Nil => marks[cmd.dst.0 as usize] = TEXT_POS_NO_MATCH,
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

/// Build an `NfaMatch` from a finalization snapshot. `state` is the accepting
/// state whose `finals` map marks → tags; `marks` is the snapshot taken at
/// the accept; `end` is the byte offset where the match ended.
fn finalize(
    tdfa: &Tdfa,
    state: u32,
    marks: &[TextPos],
    end: usize,
    num_tags: usize,
) -> NfaMatch {
    let mut tag_values = vec![TEXT_POS_NO_MATCH; num_tags];
    let row = &tdfa.finals()[state as usize];
    for cmd in row {
        let val = match cmd.src {
            MarkValue::Copy(src) => marks[src.0 as usize],
            MarkValue::Nil => TEXT_POS_NO_MATCH,
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
    let start_pos = if start_pos == TEXT_POS_NO_MATCH { 0 } else { start_pos };

    let captures = tags_to_captures(&tag_values);
    NfaMatch {
        range: start_pos..end_pos,
        captures,
    }
}
