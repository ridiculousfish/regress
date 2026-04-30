//! NFA execution backend.

use crate::automata::nfa::{
    FULL_MATCH_END, FULL_MATCH_START, GOAL_STATE, Nfa, StateHandle, TEXT_POS_NO_MATCH, TagIdx,
    TextPos,
};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ops::Range;

// A thread is a current state and a per-tag value array. The NFA backend
// pre-dates register allocation, so each tag occupies its own slot.
#[derive(Clone, Debug, Default)]
pub struct Thread {
    pub state: StateHandle,
    pub tags: Box<[TextPos]>,
}

impl Thread {
    fn new(state: StateHandle, num_tags: usize) -> Self {
        let mut tags = Vec::new();
        tags.reserve_exact(num_tags);
        tags.resize(num_tags, TEXT_POS_NO_MATCH);
        Self {
            state,
            tags: tags.into_boxed_slice(),
        }
    }

    fn clone_to_state(&self, state: StateHandle) -> Self {
        Thread {
            state,
            tags: self.tags.clone(),
        }
    }

    fn get_tag(&self, idx: TagIdx) -> TextPos {
        self.tags[idx as usize]
    }

    fn get_tag_mut(&mut self, idx: TagIdx) -> &mut TextPos {
        &mut self.tags[idx as usize]
    }

    fn tags_to_captures(&self) -> Vec<Option<Range<usize>>> {
        tags_to_captures(&self.tags)
    }
}

/// Convert a flat per-tag value array into capture-group ranges. Tags 0/1 are
/// the full match and are skipped; pairs `(2,3), (4,5), ...` are group N's
/// open/close. `TEXT_POS_NO_MATCH` in either slot means the group didn't
/// participate.
pub(crate) fn tags_to_captures(tags: &[TextPos]) -> Vec<Option<Range<usize>>> {
    let mut captures = Vec::new();
    captures.reserve_exact((tags.len() - 2) / 2);
    let mut tag_idx = 2;
    while tag_idx + 1 < tags.len() {
        let start = tags[tag_idx];
        let end = tags[tag_idx + 1];
        debug_assert!((start == TEXT_POS_NO_MATCH) == (end == TEXT_POS_NO_MATCH));
        if start == TEXT_POS_NO_MATCH {
            captures.push(None);
        } else {
            captures.push(Some(start..end));
        }
        tag_idx += 2;
    }
    captures
}

/// Result of NFA execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NfaMatch {
    /// The range of bytes that matched.
    pub range: Range<usize>,
    /// The capture groups. Each capture group is represented as an Option<Range<usize>>.
    /// None means the group didn't match (e.g., in an alternation).
    pub captures: Vec<Option<Range<usize>>>,
}

/// Execute the NFA against a slice of bytes.
/// Returns the first match found, or None if no match is found.
pub fn execute_nfa(nfa: &Nfa, input: &[u8]) -> Option<NfaMatch> {
    let mut current_threads = Vec::new();
    let mut next_threads = Vec::new();
    let num_tags = nfa.num_tags();

    // Start at the start state
    let start_thread = Thread::new(nfa.start(), num_tags);
    current_threads.push(start_thread);

    // Apply epsilon closure to get initial thread set
    epsilon_closure_with_tags(nfa, &mut current_threads, 0);

    // Process each byte of input
    for (pos, &byte) in input.iter().enumerate() {
        next_threads.clear();

        // For each current thread, find transitions on this byte
        for thread in &current_threads {
            let state = nfa.at(thread.state);

            // Check byte transitions
            if let Some(next_state) = state.transition_for_byte(byte) {
                let next_thread = thread.clone_to_state(next_state);
                next_threads.push(next_thread);
            }
        }

        // If no threads can process this byte, check for matches in current set
        if next_threads.is_empty() {
            for thread in &current_threads {
                if thread.state == GOAL_STATE {
                    let start_pos = thread.get_tag(FULL_MATCH_START);
                    let end_pos = thread.get_tag(FULL_MATCH_END);
                    let captures = thread.tags_to_captures();
                    return Some(NfaMatch {
                        range: start_pos..end_pos,
                        captures,
                    });
                }
            }
            return None;
        }

        // Add epsilon-reachable states with tag updates
        epsilon_closure_with_tags(nfa, &mut next_threads, pos + 1);

        // Swap thread sets for next iteration
        core::mem::swap(&mut current_threads, &mut next_threads);
    }

    // After processing all input, check for accepting states
    for thread in &current_threads {
        if thread.state == GOAL_STATE {
            let start_pos = thread.get_tag(FULL_MATCH_START);
            let end_pos = thread.get_tag(FULL_MATCH_END);
            let captures = thread.tags_to_captures();
            return Some(NfaMatch {
                range: start_pos..end_pos,
                captures,
            });
        }
    }

    None
}

/// Add all epsilon-reachable states to the given thread set, updating tags.
fn epsilon_closure_with_tags(nfa: &Nfa, threads: &mut Vec<Thread>, current_pos: TextPos) {
    let mut i = 0;
    while i < threads.len() {
        let thread = threads[i].clone();
        let state = nfa.at(thread.state);

        // Process all epsilon transitions
        for eps_edge in &state.eps {
            // Check if we already have a thread in this target state
            let target_exists = threads.iter().any(|t| t.state == eps_edge.target);

            if !target_exists {
                // Create new thread and update its tags
                let mut new_thread = thread.clone_to_state(eps_edge.target);

                // Apply tag operations from the epsilon edge
                for &tag_idx in &eps_edge.ops {
                    *new_thread.get_tag_mut(tag_idx) = current_pos;
                }

                threads.push(new_thread);
            }
        }

        i += 1;
    }
}
