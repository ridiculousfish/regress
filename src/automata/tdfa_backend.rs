//! TDFA execution backend.

use crate::automata::dfa::{DEAD_STATE, Dfa};
use crate::automata::nfa::{FULL_MATCH_END, FULL_MATCH_START, TEXT_POS_NO_MATCH, TextPos};
use crate::automata::nfa_backend::{NfaMatch, tags_to_captures};
use crate::automata::tdfa::{
    InputMark, MarkValue, TDFA_DEAD_STATE, TagCommand, TagCommandList, Tdfa,
};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

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
    apply_commands(&mut marks, tdfa.entry_commands(), start);

    let mut state = tdfa.start();
    let mut last_accept: Option<(usize, u32, Vec<TextPos>)> = None;

    if state == TDFA_DEAD_STATE {
        return None;
    }
    if tdfa.accepting()[state as usize] {
        last_accept = Some((start, state, marks.clone()));
    }

    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let trans_cmds: &[TagCommandList] = tdfa.transition_commands();
    let accepting = tdfa.accepting();
    let num_classes = tdfa.num_classes();

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
        if accepting[state as usize] {
            last_accept = Some((pos + 1, state, marks.clone()));
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
fn finalize(tdfa: &Tdfa, state: u32, marks: &[TextPos], end: usize, num_tags: usize) -> NfaMatch {
    let mut tag_values = vec![TEXT_POS_NO_MATCH; num_tags];
    let row = &tdfa.finals()[state as usize];
    for cmd in row {
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
