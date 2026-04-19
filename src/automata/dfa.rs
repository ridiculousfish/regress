//! DFA construction via subset construction from an NFA.

use crate::automata::nfa::{GOAL_STATE, Nfa, StateHandle};
use std::collections::HashMap;

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

pub type DfaStateId = u32;

/// The dead state: all transitions loop to self, not accepting.
pub const DEAD_STATE: DfaStateId = 0;

/// Maximum number of DFA states before we bail out.
const DFA_STATE_BUDGET: usize = 4096;

#[derive(Debug)]
pub enum Error {
    BudgetExceeded,
}

#[derive(Debug)]
pub struct Dfa {
    start: DfaStateId,
    num_classes: usize,
    byte_to_class: [u8; 256],
    transitions: Box<[DfaStateId]>,
    accepting: Box<[bool]>,
}

/// Compute byte equivalence classes from NFA transitions.
///
/// Two bytes are in the same class iff they trigger exactly the same set of
/// NFA transitions. We find class boundaries by collecting every `range.start`
/// and `range.end + 1` from every NFA transition.
pub(super) fn compute_byte_classes(nfa: &Nfa) -> ([u8; 256], usize) {
    let mut cuts: Vec<u16> = Vec::new();
    cuts.push(0);
    for state in nfa.states.iter() {
        for &(ref range, _) in &state.transitions {
            cuts.push(range.start as u16);
            if range.end < 255 {
                cuts.push(range.end as u16 + 1);
            }
        }
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut byte_to_class = [0u8; 256];
    let mut class_id: u8 = 0;
    for window in cuts.windows(2) {
        for b in window[0]..window[1] {
            byte_to_class[b as usize] = class_id;
        }
        class_id += 1;
    }
    // Last class: from last cut through 255.
    let last_cut = *cuts.last().unwrap();
    for b in last_cut..=255u16 {
        byte_to_class[b as usize] = class_id;
    }
    let num_classes = class_id as usize + 1;
    (byte_to_class, num_classes)
}

/// Compute the epsilon closure of a set of NFA states.
///
/// Returns a sorted `Vec<StateHandle>` — the canonical representation used as
/// a HashMap key for DFA state identity. Register operations on epsilon edges
/// are discarded (regular DFA); a future TDFA would track them here.
fn epsilon_closure(nfa: &Nfa, seeds: &[StateHandle], scratch: &mut Vec<bool>) -> Vec<StateHandle> {
    let num_states = nfa.states.len();
    scratch.clear();
    scratch.resize(num_states, false);

    let mut stack: Vec<StateHandle> = Vec::new();
    for &s in seeds {
        if !scratch[s as usize] {
            scratch[s as usize] = true;
            stack.push(s);
        }
    }

    while let Some(s) = stack.pop() {
        for edge in &nfa.states[s as usize].eps {
            let t = edge.target;
            if !scratch[t as usize] {
                scratch[t as usize] = true;
                stack.push(t);
            }
        }
    }

    let mut result = Vec::new();
    for (i, &b) in scratch.iter().enumerate() {
        if b {
            result.push(i as StateHandle);
        }
    }
    result
}

/// Build a lookup table: for each byte class, one representative byte value.
pub(super) fn representative_bytes(byte_to_class: &[u8; 256], num_classes: usize) -> Vec<u8> {
    let mut reps = vec![0u8; num_classes];
    for (byte, &class) in byte_to_class.iter().enumerate() {
        // First byte found for each class wins.
        if byte == 0 || reps[class as usize] == 0 && class != 0 {
            reps[class as usize] = byte as u8;
        }
    }
    reps
}

impl Dfa {
    pub fn try_from(nfa: &Nfa) -> Result<Self, Error> {
        let (byte_to_class, num_classes) = compute_byte_classes(nfa);
        let rep_bytes = representative_bytes(&byte_to_class, num_classes);

        let mut state_map: HashMap<Vec<StateHandle>, DfaStateId> = HashMap::new();
        let mut transitions: Vec<DfaStateId> = Vec::new();
        let mut accepting: Vec<bool> = Vec::new();
        let mut worklist: Vec<Vec<StateHandle>> = Vec::new();
        let mut scratch = Vec::new();

        // State 0 = dead state (self-loops, not accepting).
        state_map.insert(Vec::new(), DEAD_STATE);
        transitions.resize(num_classes, DEAD_STATE);
        accepting.push(false);

        // Start state = epsilon closure of {nfa.start}.
        let start_set = epsilon_closure(nfa, &[nfa.start], &mut scratch);
        let start_accepting = start_set.contains(&GOAL_STATE);
        let start_id: DfaStateId = 1;
        state_map.insert(start_set.clone(), start_id);
        transitions.resize(transitions.len() + num_classes, DEAD_STATE);
        accepting.push(start_accepting);
        worklist.push(start_set);

        let mut targets: Vec<StateHandle> = Vec::new();

        while let Some(nfa_set) = worklist.pop() {
            let dfa_state = state_map[&nfa_set];
            let row_offset = dfa_state as usize * num_classes;

            for class in 0..num_classes {
                let rep = rep_bytes[class];

                // Compute move: collect NFA states reachable by consuming rep.
                targets.clear();
                for &nfa_state in &nfa_set {
                    if let Some(target) = nfa.states[nfa_state as usize].transition_for_byte(rep) {
                        targets.push(target);
                    }
                }

                if targets.is_empty() {
                    continue; // Already DEAD_STATE.
                }

                // Deduplicate targets before closure.
                targets.sort_unstable();
                targets.dedup();

                let target_set = epsilon_closure(nfa, &targets, &mut scratch);

                let target_id = match state_map.get(&target_set) {
                    Some(&id) => id,
                    None => {
                        let id = accepting.len() as DfaStateId;
                        if id as usize >= DFA_STATE_BUDGET {
                            return Err(Error::BudgetExceeded);
                        }
                        let is_accepting = target_set.contains(&GOAL_STATE);
                        accepting.push(is_accepting);
                        transitions.resize(transitions.len() + num_classes, DEAD_STATE);
                        state_map.insert(target_set.clone(), id);
                        worklist.push(target_set);
                        id
                    }
                };
                transitions[row_offset + class] = target_id;
            }
        }

        Ok(Dfa {
            start: start_id,
            num_classes,
            byte_to_class,
            transitions: transitions.into_boxed_slice(),
            accepting: accepting.into_boxed_slice(),
        })
    }

    pub fn num_states(&self) -> usize {
        self.accepting.len()
    }

    pub fn num_classes(&self) -> usize {
        self.num_classes
    }

    pub fn start(&self) -> DfaStateId {
        self.start
    }

    pub fn byte_to_class(&self) -> &[u8; 256] {
        &self.byte_to_class
    }

    pub fn transitions(&self) -> &[DfaStateId] {
        &self.transitions
    }

    pub fn accepting(&self) -> &[bool] {
        &self.accepting
    }
}

