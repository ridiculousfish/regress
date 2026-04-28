//! TDFA execution backend. Milestone 2: anchored match, no tag tracking.

use crate::automata::tdfa::{TDFA_COMMITTED_ACCEPT_STATE, TDFA_LAST_SENTINEL, Tdfa};
use core::ops::Range;

/// Anchored leftmost match. Scans from position 0, tracking the last accepting
/// position seen, and stops at dead-state, committed-accept, or end of input.
///
/// Greedy vs lazy semantics fall out of TDFA construction: lazy quantifiers
/// dead-state after their shortest accept, greedy quantifiers stay alive and
/// keep updating the last-accept position. Leftmost-first alternation is
/// likewise baked in by truncate-at-first-GOAL during determinization.
///
/// Sentinel IDs `<= TDFA_LAST_SENTINEL` short-circuit the scan: `DEAD` breaks
/// with the current last-accept; `COMMITTED_ACCEPT` returns `0..input.len()`
/// immediately (every reachable state is accepting, so further scanning can
/// only extend the match to end of input).
pub fn execute_anchored(tdfa: &Tdfa, input: &[u8]) -> Option<Range<usize>> {
    let mut state = tdfa.start();
    let byte_to_class = tdfa.byte_to_class();
    let transitions = tdfa.transitions();
    let accepting = tdfa.accepting();
    let num_classes = tdfa.num_classes();

    if state <= TDFA_LAST_SENTINEL {
        return if state == TDFA_COMMITTED_ACCEPT_STATE {
            Some(0..input.len())
        } else {
            None
        };
    }

    let mut last_accept: Option<usize> = None;
    if accepting[state as usize] {
        last_accept = Some(0);
    }
    for (i, &byte) in input.iter().enumerate() {
        let class = byte_to_class[byte as usize] as usize;
        state = transitions[state as usize * num_classes + class];
        if state <= TDFA_LAST_SENTINEL {
            if state == TDFA_COMMITTED_ACCEPT_STATE {
                return Some(0..input.len());
            }
            break;
        }
        if accepting[state as usize] {
            last_accept = Some(i + 1);
        }
    }
    last_accept.map(|end| 0..end)
}
