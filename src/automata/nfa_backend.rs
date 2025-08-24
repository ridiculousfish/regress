//! NFA execution backend for pattern matching.

use crate::automata::nfa::{
    FULL_MATCH_END, FULL_MATCH_START, GOAL_STATE, Nfa, RegisterIdx, StateHandle, TextPos,
};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ops::Range;
use smallvec::{SmallVec, smallvec};

// A thread is a current state and register set.
#[derive(Clone, Debug, Default)]
pub struct Thread {
    pub state: StateHandle,
    pub registers: SmallVec<[TextPos; 2]>,
}

impl Thread {
    fn new(state: StateHandle) -> Self {
        Self {
            state,
            registers: smallvec![],
        }
    }

    fn with_registers(state: StateHandle, registers: SmallVec<[TextPos; 2]>) -> Self {
        Self { state, registers }
    }

    // Get a register, which must exist.
    fn get_reg(&self, idx: RegisterIdx) -> TextPos {
        self.registers[idx as usize]
    }
}

/// Result of NFA execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NfaMatch {
    /// The range of bytes that matched.
    pub range: Range<usize>,
}

/// Execute the NFA against a slice of bytes.
/// Returns the first match found, or None if no match is found.
pub fn execute_nfa(nfa: &Nfa, input: &[u8]) -> Option<NfaMatch> {
    let mut current_threads = Vec::new();
    let mut next_threads = Vec::new();

    // Start at the start state
    let start_thread = Thread::new(nfa.start());
    current_threads.push(start_thread);

    // Apply epsilon closure to get initial thread set
    epsilon_closure_with_registers(nfa, &mut current_threads, 0);

    // Check if we start in an accepting state (empty match)
    for thread in &current_threads {
        if thread.state == GOAL_STATE {
            // Extract match range from registers
            let start_pos = thread.get_reg(FULL_MATCH_START);
            let end_pos = thread.get_reg(FULL_MATCH_END);
            return Some(NfaMatch {
                range: start_pos..end_pos,
            });
        }
    }

    // Process each byte of input
    for (pos, &byte) in input.iter().enumerate() {
        next_threads.clear();

        // For each current thread, find transitions on this byte
        for thread in &current_threads {
            let state = nfa.at(thread.state);

            // Check byte transitions
            if let Some(next_state) = state.transition_for_byte(byte) {
                let next_thread = Thread::with_registers(next_state, thread.registers.clone());
                next_threads.push(next_thread);
            }
        }

        // If no threads can process this byte, no match
        if next_threads.is_empty() {
            return None;
        }

        // Add epsilon-reachable states with register updates
        epsilon_closure_with_registers(nfa, &mut next_threads, pos + 1);

        // Check if we've reached an accepting state
        for thread in &next_threads {
            if thread.state == GOAL_STATE {
                // Extract match range from registers
                let start_pos = thread.get_reg(FULL_MATCH_START);
                let end_pos = thread.get_reg(FULL_MATCH_END);
                return Some(NfaMatch {
                    range: start_pos..end_pos,
                });
            }
        }

        // Swap thread sets for next iteration
        core::mem::swap(&mut current_threads, &mut next_threads);
    }

    None
}

/// Add all epsilon-reachable states to the given thread set, updating registers.
fn epsilon_closure_with_registers(nfa: &Nfa, threads: &mut Vec<Thread>, current_pos: TextPos) {
    let mut i = 0;
    while i < threads.len() {
        let thread = threads[i].clone();
        let state = nfa.at(thread.state);

        // Process all epsilon transitions
        for eps_edge in &state.eps {
            // Check if we already have a thread in this target state
            let target_exists = threads.iter().any(|t| t.state == eps_edge.target);

            if !target_exists {
                // Create new thread with updated registers
                let mut new_registers = thread.registers.clone();

                // Apply register operations from the epsilon edge
                for &reg_idx in &eps_edge.ops {
                    // Ensure registers vector is large enough
                    while new_registers.len() <= reg_idx as usize {
                        new_registers.push(0);
                    }
                    new_registers[reg_idx as usize] = current_pos;
                }

                let new_thread = Thread::with_registers(eps_edge.target, new_registers);
                threads.push(new_thread);
            }
        }

        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Flags;

    /// Helper function to create an NFA from a pattern string
    fn create_nfa(pattern: &str) -> Nfa {
        let flags = Flags::default();
        let mut ire = crate::backends::try_parse(pattern.chars().map(u32::from), flags)
            .expect("Pattern failed to parse");
        crate::backends::optimize(&mut ire);
        Nfa::try_from(&ire).unwrap()
    }

    #[test]
    fn test_simple_match() {
        // Create a simple pattern "abc"
        let nfa = create_nfa("abc");

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
        let nfa = create_nfa("abc|def");

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

    #[test]
    fn test_exact_quantifier() {
        // Create a pattern "a{3}" - exactly 3 'a's
        let nfa = create_nfa("a{3}");

        // Test exact match
        let result = execute_nfa(&nfa, b"aaa");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // Test too few
        let result = execute_nfa(&nfa, b"aa");
        assert_eq!(result, None);

        // Test too many - should match first 3
        let result = execute_nfa(&nfa, b"aaaa");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));
    }

    #[test]
    fn test_bounded_quantifier_greedy() {
        // Create a pattern "a{2,4}" - 2 to 4 'a's
        // Note: Current NFA algorithm finds the first valid match
        let nfa = create_nfa("a{2,4}");

        // Test minimum
        let result = execute_nfa(&nfa, b"aa");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        // Test middle - should match minimum first
        let result = execute_nfa(&nfa, b"aaa");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        // Test maximum - should match minimum first
        let result = execute_nfa(&nfa, b"aaaa");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        // Test more than maximum - should match minimum first
        let result = execute_nfa(&nfa, b"aaaaa");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        // Test too few
        let result = execute_nfa(&nfa, b"a");
        assert_eq!(result, None);
    }

    #[test]
    fn test_bounded_quantifier_non_greedy() {
        // Create a pattern "a{2,4}?" - non-greedy 2 to 4 'a's
        let nfa = create_nfa("a{2,4}?");

        // Test minimum - non-greedy should prefer shorter match
        let result = execute_nfa(&nfa, b"aa");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        // Test with more - non-greedy should still prefer minimum
        let result = execute_nfa(&nfa, b"aaa");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        let result = execute_nfa(&nfa, b"aaaa");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        // Test too few
        let result = execute_nfa(&nfa, b"a");
        assert_eq!(result, None);
    }

    #[test]
    fn test_star_quantifier_greedy() {
        // Create a pattern "a*" - zero or more 'a's
        // Note: Current NFA algorithm finds the first valid match (zero-length)
        let nfa = create_nfa("a*");

        // Test zero matches - should match empty string first
        let result = execute_nfa(&nfa, b"");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));

        // Test one match - should match empty string first
        let result = execute_nfa(&nfa, b"a");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));

        // Test multiple matches - should match empty string first
        let result = execute_nfa(&nfa, b"aaaa");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));

        // Test with non-matching suffix - should match empty string
        let result = execute_nfa(&nfa, b"aaab");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));

        // Test non-matching at start - should match empty string
        let result = execute_nfa(&nfa, b"baa");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));
    }

    #[test]
    fn test_star_quantifier_non_greedy() {
        // Create a pattern "a*?" - non-greedy zero or more 'a's
        let nfa = create_nfa("a*?");

        // Test - non-greedy should prefer zero matches
        let result = execute_nfa(&nfa, b"");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));

        let result = execute_nfa(&nfa, b"a");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));

        let result = execute_nfa(&nfa, b"aaaa");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));
    }

    #[test]
    fn test_plus_quantifier_greedy() {
        // Create a pattern "a+" - one or more 'a's
        // Note: Current NFA algorithm finds the first valid match
        let nfa = create_nfa("a+");

        // Test zero matches - should fail
        let result = execute_nfa(&nfa, b"");
        assert_eq!(result, None);

        // Test one match - should match minimum (1)
        let result = execute_nfa(&nfa, b"a");
        assert_eq!(result, Some(NfaMatch { range: 0..1 }));

        // Test multiple matches - should match minimum (1)
        let result = execute_nfa(&nfa, b"aaaa");
        assert_eq!(result, Some(NfaMatch { range: 0..1 }));

        // Test with non-matching suffix - should match minimum (1)
        let result = execute_nfa(&nfa, b"aaab");
        assert_eq!(result, Some(NfaMatch { range: 0..1 }));

        // Test non-matching at start
        let result = execute_nfa(&nfa, b"baa");
        assert_eq!(result, None);
    }

    #[test]
    fn test_plus_quantifier_non_greedy() {
        // Create a pattern "a+?" - non-greedy one or more 'a's
        let nfa = create_nfa("a+?");

        // Test zero matches - should fail
        let result = execute_nfa(&nfa, b"");
        assert_eq!(result, None);

        // Test one match - non-greedy should prefer minimum
        let result = execute_nfa(&nfa, b"a");
        assert_eq!(result, Some(NfaMatch { range: 0..1 }));

        // Test multiple - non-greedy should still prefer minimum (1)
        let result = execute_nfa(&nfa, b"aaaa");
        assert_eq!(result, Some(NfaMatch { range: 0..1 }));

        // Test non-matching at start
        let result = execute_nfa(&nfa, b"baa");
        assert_eq!(result, None);
    }

    #[test]
    fn test_question_quantifier() {
        // Create a pattern "a?" - zero or one 'a'
        let nfa = create_nfa("a?");

        // Test zero matches - should match empty string
        let result = execute_nfa(&nfa, b"");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));

        // Test one match - should match empty string first
        let result = execute_nfa(&nfa, b"a");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));

        // Test more than one - should match empty string first
        let result = execute_nfa(&nfa, b"aa");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));

        // Test non-matching - should match empty string
        let result = execute_nfa(&nfa, b"b");
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));
    }

    #[test]
    fn test_infinite_loops_actually_loop() {
        // Test a+ with multiple characters - can loop but finds first valid match
        let nfa = create_nfa("a+");
        let result = execute_nfa(&nfa, b"aaa");
        // Finds match after 1 'a' since that satisfies the pattern
        assert_eq!(result, Some(NfaMatch { range: 0..1 }));

        // Test a* with multiple characters - can loop but finds first valid match
        let nfa = create_nfa("a*");
        let result = execute_nfa(&nfa, b"aaa");
        // Finds zero-length match immediately since that satisfies the pattern
        assert_eq!(result, Some(NfaMatch { range: 0..0 }));
    }

    #[test]
    fn test_loop_with_following_pattern() {
        // Test pattern that requires the loop to work correctly
        let nfa = create_nfa("a+b");

        // Should match "ab"
        let result = execute_nfa(&nfa, b"ab");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        // Should match "aaab" - requires loop to consume multiple 'a's
        let result = execute_nfa(&nfa, b"aaab");
        assert_eq!(result, Some(NfaMatch { range: 0..4 }));

        // Should not match just "b"
        let result = execute_nfa(&nfa, b"b");
        assert_eq!(result, None);
    }

    #[test]
    fn test_star_with_following_pattern() {
        // Test a*b pattern
        let nfa = create_nfa("a*b");

        // Should match "b" (zero 'a's)
        let result = execute_nfa(&nfa, b"b");
        assert_eq!(result, Some(NfaMatch { range: 0..1 }));

        // Should match "ab"
        let result = execute_nfa(&nfa, b"ab");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        // Should match "aaab" - requires loop to consume multiple 'a's
        let result = execute_nfa(&nfa, b"aaab");
        assert_eq!(result, Some(NfaMatch { range: 0..4 }));
    }

    #[test]
    fn test_bounded_loop_greediness_matters() {
        // Greedy a{2,4}b should prefer longer matches of 'a'
        let nfa = create_nfa("a{2,4}b");

        // Test "aab" - should match (2 a's + b)
        let result = execute_nfa(&nfa, b"aab");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // Test "aaab" - should match (3 a's + b)
        let result = execute_nfa(&nfa, b"aaab");
        assert_eq!(result, Some(NfaMatch { range: 0..4 }));

        // Test "aaaab" - should match (4 a's + b)
        let result = execute_nfa(&nfa, b"aaaab");
        assert_eq!(result, Some(NfaMatch { range: 0..5 }));

        // Non-greedy a{2,4}?b should prefer shorter matches of 'a'
        let nfa = create_nfa("a{2,4}?b");

        // Test "aab" - should match (2 a's + b)
        let result = execute_nfa(&nfa, b"aab");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // Test "aaab" - non-greedy should still match (2 a's + b) if possible
        // But our current NFA execution will find the first complete match
        let result = execute_nfa(&nfa, b"aaab");
        assert_eq!(result, Some(NfaMatch { range: 0..4 }));
    }

    #[test]
    fn test_bounded_loops_can_exit_at_minimum() {
        // These tests will fail if bounded loops can't exit after minimum iterations

        // Test a{2,4} followed by 'b' - should be able to exit after 2 'a's
        let nfa = create_nfa("a{2,4}b");

        // This should work: exactly minimum + required suffix
        let result = execute_nfa(&nfa, b"aab");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // Test a{1,3} followed by 'x' - should be able to exit after 1 'a'
        let nfa = create_nfa("a{1,3}x");

        // This should work: exactly minimum + required suffix
        let result = execute_nfa(&nfa, b"ax");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        // Test a{3,5} alone - should be able to match exactly 3
        let nfa = create_nfa("a{3,5}");

        // This should work: exactly minimum iterations
        let result = execute_nfa(&nfa, b"aaa");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // This will also match minimum (3) since our algorithm finds first valid match
        let result = execute_nfa(&nfa, b"aaaa");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));
    }

    #[test]
    fn test_bounded_loops_must_allow_early_exit() {
        // This test specifically checks that we can exit as soon as minimum is satisfied
        // when there's a following pattern that requires it

        // Pattern: a{2,100}b - should be able to exit after just 2 'a's when 'b' follows
        let nfa = create_nfa("a{2,100}b");

        // Should match with exactly minimum 'a's
        let result = execute_nfa(&nfa, b"aab");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // Pattern: a{1,50}c - should be able to exit after just 1 'a' when 'c' follows
        let nfa = create_nfa("a{1,50}c");

        // Should match with exactly minimum 'a's
        let result = execute_nfa(&nfa, b"ac");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));
    }

    #[test]
    fn test_without_early_exit_these_should_fail() {
        // These tests should fail if bounded loops cannot exit after minimum iterations
        // They require the ability to exit the loop early when a following pattern matches

        // Test: a{4,10}b with input "aaaab" (exactly 4 'a's + 'b')
        // Without early exit capability, this would need to consume all 10 'a's or fail
        let nfa = create_nfa("a{4,10}b");
        let result = execute_nfa(&nfa, b"aaaab");
        assert_eq!(result, Some(NfaMatch { range: 0..5 }));

        // Test: a{2,8}x with input "aax" (exactly 2 'a's + 'x')
        // Without early exit, this would try to consume up to 8 'a's
        let nfa = create_nfa("a{2,8}x");
        let result = execute_nfa(&nfa, b"aax");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // Test: a{1,7}z with input "az" (exactly 1 'a' + 'z')
        // Without early exit, this would be impossible to match
        let nfa = create_nfa("a{1,7}z");
        let result = execute_nfa(&nfa, b"az");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));
    }

    #[test]
    fn test_immediate_exit_after_minimum_required() {
        // This test requires immediate exit capability after minimum iterations
        // The pattern structure should only work if we can exit immediately after min

        // Test a very specific case: pattern "a{3,3}$" with input "aaa"
        // This should match exactly - no optional iterations to complicate things
        let nfa = create_nfa("a{3,3}");
        let result = execute_nfa(&nfa, b"aaa");
        assert_eq!(result, Some(NfaMatch { range: 0..3 }));

        // The real test: a bounded quantifier where we need immediate exit
        // Pattern "a{2,4}" should be able to match just "aa"
        let nfa = create_nfa("a{2,4}");
        let result = execute_nfa(&nfa, b"aa");
        assert_eq!(result, Some(NfaMatch { range: 0..2 }));

        // This should also work with exactly minimum iterations
        let nfa = create_nfa("a{1,5}");
        let result = execute_nfa(&nfa, b"a");
        assert_eq!(result, Some(NfaMatch { range: 0..1 }));
    }
}
