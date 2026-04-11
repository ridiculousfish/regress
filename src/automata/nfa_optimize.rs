//! Optimize states in an NFA builder

use crate::automata::nfa::{State, StateHandle};
use std::collections::HashSet;

// Optimize epsilons within states.
fn optimize_eps(states: &mut [State]) {
    // Drop zero-op epsilons to self.
    for (idx, state) in states.iter_mut().enumerate() {
        state
            .eps
            .retain(|edge| !(edge.target as usize == idx && edge.ops.is_empty()));
    }

    // Stable-dedup identical epsilon edges.
    let mut seen = HashSet::new();
    for state in states {
        // Common case.
        if state.eps.len() < 2 {
            continue;
        }
        seen.clear();
        state.eps.retain(|edge| seen.insert(edge.clone()));
    }
}

// Forward state handles to their dense representation.
fn forward_to_dense(states: &mut [State], forwarder: impl Fn(StateHandle) -> StateHandle) {
    for state in states {
        for edge in &mut state.eps {
            edge.target = forwarder(edge.target);
        }
        for t in &mut state.transitions {
            t.1 = forwarder(t.1);
        }
    }
}

// A state is collapsible if it has no byte transitions and a single outgoing
// epsilon transition  with no operations. In that case, "collapse" the state into its target.
fn get_collapse_target(s: &State) -> Option<StateHandle> {
    if s.eps.len() == 1 && s.eps[0].ops.is_empty() && s.transitions.is_empty() {
        Some(s.eps[0].target)
    } else {
        None
    }
}

// Collapse states that don't need to be distinct, and update handle references.
fn collapse_and_forward(states: &mut Vec<State>) {
    // Clear nodes that have only a single outgoing epsilon transition
    // with no operations. Implement this by "forwarding" each node to its target.
    // This maps StateHandles to the StateHandle that replaces them.
    let forwarding: Box<[StateHandle]> = states
        .iter()
        .enumerate()
        .map(|(i, s)| get_collapse_target(s).unwrap_or(i as StateHandle))
        .collect();

    // Construct our ultimate dense state array.
    // This keeps only states that aren't forwarded.
    // Maps StateHandles to their ultimate index.
    let mut next_dense_index: StateHandle = 0;
    let mut handle_to_dense_index = Vec::new();
    for (idx, &target) in forwarding.iter().enumerate() {
        if target == idx as StateHandle {
            // This state is not forwarded, keep it.
            handle_to_dense_index.push(Some(next_dense_index));
            next_dense_index += 1;
        } else {
            // This state is forwarded, it maps nowhere.
            handle_to_dense_index.push(None);
        }
    }

    // Construct a lambda that, given a state handle, returns the forwarded dense index.
    let sparse_to_dense = |h: StateHandle| {
        // Walk the chain until we hit a state that forwards to itself.
        let mut cursor = h;
        while cursor != forwarding[cursor as usize] {
            cursor = forwarding[cursor as usize];
        }
        handle_to_dense_index[cursor as usize].expect("State should not be forwarded")
    };
    // Update each edge to use the dense index.
    forward_to_dense(states, sparse_to_dense);

    // Retain only non-forwarded states.
    let mut state_idx = 0;
    states.retain(|_state| {
        let keep = handle_to_dense_index[state_idx].is_some();
        state_idx += 1;
        keep
    });
    debug_assert_eq!(states.len(), next_dense_index as usize);
}

// Optimize NFA states in-place.
pub(super) fn optimize_states(states: &mut Vec<State>) {
    optimize_eps(states);
    collapse_and_forward(states);
}
