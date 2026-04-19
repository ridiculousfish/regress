//! TDFA execution backend. Milestone 2: boolean anchored match only.

use crate::automata::tdfa::{TDFA_DEAD_STATE, Tdfa};

/// Anchored match: returns true if input matches from the start (full match
/// to end of input).
pub fn execute_anchored(tdfa: &Tdfa, input: &[u8]) -> bool {
    let mut state = tdfa.start();
    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let accepting = tdfa.accepting();
    let num_classes = tdfa.num_classes();
    for &byte in input {
        if state == TDFA_DEAD_STATE {
            return false;
        }
        let class = byte_to_class[byte as usize] as usize;
        state = transitions[state as usize * num_classes + class];
    }
    accepting[state as usize]
}

/// Scan `input` from position 0 and return the **longest** prefix length whose
/// TDFA state is accepting, or `None` if no prefix (including the empty one)
/// accepts. Used to observe leftmost-greedy match length without capture
/// tracking.
pub fn execute_anchored_longest_accept(tdfa: &Tdfa, input: &[u8]) -> Option<usize> {
    let mut state = tdfa.start();
    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let accepting = tdfa.accepting();
    let num_classes = tdfa.num_classes();

    let mut best: Option<usize> = None;
    if accepting[state as usize] {
        best = Some(0);
    }
    for (i, &byte) in input.iter().enumerate() {
        if state == TDFA_DEAD_STATE {
            break;
        }
        let class = byte_to_class[byte as usize] as usize;
        state = transitions[state as usize * num_classes + class];
        if state != TDFA_DEAD_STATE && accepting[state as usize] {
            best = Some(i + 1);
        }
    }
    best
}

/// Scan `input` from position 0 and return the **shortest** prefix length
/// whose TDFA state is accepting, or `None` if no prefix accepts. Used to
/// observe lazy match length.
pub fn execute_anchored_shortest_accept(tdfa: &Tdfa, input: &[u8]) -> Option<usize> {
    let mut state = tdfa.start();
    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let accepting = tdfa.accepting();
    let num_classes = tdfa.num_classes();

    if accepting[state as usize] {
        return Some(0);
    }
    for (i, &byte) in input.iter().enumerate() {
        if state == TDFA_DEAD_STATE {
            return None;
        }
        let class = byte_to_class[byte as usize] as usize;
        state = transitions[state as usize * num_classes + class];
        if state != TDFA_DEAD_STATE && accepting[state as usize] {
            return Some(i + 1);
        }
    }
    None
}
