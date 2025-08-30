//! NFA execution backend.

use crate::automata::nfa::{
    FULL_MATCH_END, FULL_MATCH_START, GOAL_STATE, Nfa, RegisterIdx, StateHandle, TEXT_POS_NO_MATCH,
    TextPos,
};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ops::Range;

// A thread is a current state and register set.
#[derive(Clone, Debug, Default)]
pub struct Thread {
    pub state: StateHandle,
    pub registers: Box<[TextPos]>,
}

impl Thread {
    // Construct a Thread with a specific state and number of registers.
    fn new(state: StateHandle, num_registers: usize) -> Self {
        let mut registers = Vec::new();
        registers.reserve_exact(num_registers);
        registers.resize(num_registers, TEXT_POS_NO_MATCH);
        Self {
            state,
            registers: registers.into_boxed_slice(),
        }
    }

    // Clone this Thread, producing a new Thread at a new state.
    fn clone_to_state(&self, state: StateHandle) -> Self {
        Thread {
            state,
            registers: self.registers.clone(),
        }
    }

    // Get a register, which must exist.
    fn get_reg(&self, idx: RegisterIdx) -> TextPos {
        self.registers[idx as usize]
    }

    // Get a mutable reference to a register.
    fn get_reg_mut(&mut self, idx: RegisterIdx) -> &mut TextPos {
        &mut self.registers[idx as usize]
    }

    // Extract capture groups from registers.
    // Register 0 and 1 are for the full match.
    // Capture groups start at register 2 (open) and 3 (close) for group 0,
    // then 4 (open) and 5 (close) for group 1, etc.
    fn registers_to_captures(&self) -> Vec<Option<Range<usize>>> {
        let mut captures = Vec::new();
        captures.reserve_exact((self.registers.len() - 2) / 2);

        // Skip the first two registers (full match start/end)
        let mut reg_idx = 2;

        while reg_idx + 1 < self.registers.len() {
            let open_reg = reg_idx;
            let close_reg = reg_idx + 1;

            let start = self.registers[open_reg];
            let end = self.registers[close_reg];
            // Either both should be no match, or neither.
            debug_assert!((start == TEXT_POS_NO_MATCH) == (end == TEXT_POS_NO_MATCH));

            if start == TEXT_POS_NO_MATCH {
                captures.push(None);
            } else {
                captures.push(Some(start..end));
            }

            reg_idx += 2;
        }

        captures
    }
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
    let num_registers = nfa.num_registers();

    // Start at the start state
    let start_thread = Thread::new(nfa.start(), num_registers);
    current_threads.push(start_thread);

    // Apply epsilon closure to get initial thread set
    epsilon_closure_with_registers(nfa, &mut current_threads, 0);

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
                    let start_pos = thread.get_reg(FULL_MATCH_START);
                    let end_pos = thread.get_reg(FULL_MATCH_END);
                    let captures = thread.registers_to_captures();
                    return Some(NfaMatch {
                        range: start_pos..end_pos,
                        captures,
                    });
                }
            }
            return None;
        }

        // Add epsilon-reachable states with register updates
        epsilon_closure_with_registers(nfa, &mut next_threads, pos + 1);

        // Swap thread sets for next iteration
        core::mem::swap(&mut current_threads, &mut next_threads);
    }

    // After processing all input, check for accepting states
    for thread in &current_threads {
        if thread.state == GOAL_STATE {
            let start_pos = thread.get_reg(FULL_MATCH_START);
            let end_pos = thread.get_reg(FULL_MATCH_END);
            let captures = thread.registers_to_captures();
            return Some(NfaMatch {
                range: start_pos..end_pos,
                captures,
            });
        }
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
                // Create new thread and update its registers
                let mut new_thread = thread.clone_to_state(eps_edge.target);

                // Apply register operations from the epsilon edge
                for &reg_idx in &eps_edge.ops {
                    *new_thread.get_reg_mut(reg_idx) = current_pos;
                }

                threads.push(new_thread);
            }
        }

        i += 1;
    }
}
