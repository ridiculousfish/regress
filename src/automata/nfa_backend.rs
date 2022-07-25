//! NFA execution backend for pattern matching.

use crate::automata::nfa::{Nfa, StateHandle, GOAL_STATE};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ops::Range;

/// Result of NFA execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NfaMatch {
    /// The range of bytes that matched.
    pub range: Range<usize>,
}

/// Execute the NFA against a slice of bytes.
/// Returns the first match found, or None if no match is found.
pub fn execute_nfa(nfa: &Nfa, input: &[u8]) -> Option<NfaMatch> {
    // Current set of active states
    let mut current_states = Vec::new();
    let mut next_states = Vec::new();

    // Start with the start state and all epsilon-reachable states
    current_states.push(nfa.start());
    epsilon_closure(nfa, &mut current_states);

    // Check if we start in an accepting state (empty match)
    if current_states.contains(&GOAL_STATE) {
        return Some(NfaMatch { range: 0..0 });
    }

    // Process each byte of input
    for (pos, &byte) in input.iter().enumerate() {
        next_states.clear();

        // For each current state, find transitions on this byte
        for &state_handle in &current_states {
            let state = nfa.at(state_handle);

            // Check byte transitions
            if let Some(next_state) = state.transition_for_byte(byte) {
                next_states.push(next_state);
            }
        }

        // If no states can process this byte, no match
        if next_states.is_empty() {
            return None;
        }

        // Add epsilon-reachable states
        epsilon_closure(nfa, &mut next_states);

        // Check if we've reached an accepting state
        if next_states.contains(&GOAL_STATE) {
            return Some(NfaMatch { range: 0..pos + 1 });
        }

        // Swap state sets for next iteration
        core::mem::swap(&mut current_states, &mut next_states);
    }

    None
}

/// Add all epsilon-reachable states to the given state set.
fn epsilon_closure(nfa: &Nfa, states: &mut Vec<StateHandle>) {
    let mut i = 0;
    while i < states.len() {
        let state_handle = states[i];
        let state = nfa.at(state_handle);

        // Add all epsilon transitions
        for &eps_target in &state.eps {
            if !states.contains(&eps_target) {
                states.push(eps_target);
            }
        }

        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Flags;

    #[test]
    fn test_simple_match() {
        // Create a simple pattern "abc"
        let pattern = "abc";
        let flags = Flags::default();
        let mut ire = crate::backends::try_parse(pattern.chars().map(u32::from), flags).unwrap();
        crate::backends::optimize(&mut ire);
        let nfa = Nfa::try_from(&ire).unwrap();

        // Test matching
        let result = execute_nfa(&nfa, b"abc");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // Test non-matching
        let result = execute_nfa(&nfa, b"def");
        assert_eq!(result, None);

        // Test partial match
        let result = execute_nfa(&nfa, b"ab");
        assert_eq!(result, None);
    }

    #[test]
    fn test_alternation() {
        // Create a pattern "abc|def"
        let pattern = "abc|def";
        let flags = Flags::default();
        let mut ire = crate::backends::try_parse(pattern.chars().map(u32::from), flags).unwrap();
        crate::backends::optimize(&mut ire);
        let nfa = Nfa::try_from(&ire).unwrap();

        // Test first alternative
        let result = execute_nfa(&nfa, b"abc");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // Test second alternative
        let result = execute_nfa(&nfa, b"def");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // Test non-matching
        let result = execute_nfa(&nfa, b"ghi");
        assert_eq!(result, None);
    }
}
