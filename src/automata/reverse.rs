//! Reverse automaton for required-suffix-literal search.
//!
//! Some patterns have no usable *prefix* literal (so the Phase-1 prefilter
//! can't help) but do end in a required literal — e.g. `\w+\s+Holmes`. For
//! those we find the literal with `memmem`, then **drive the automaton
//! backwards** from the literal's end to locate the match start, and finally run
//! the forward (tagged) TDFA from that start for the real extent and captures.
//!
//! The backward driver is a plain DFA built from the **reversed** NFA graph
//! (tag-free): edges flipped, the old goal becomes the start and the old start
//! becomes the (single) accept. Running it right-to-left from offset `e` and
//! recording the smallest position at which it accepts yields the leftmost start
//! `s` such that `input[s..e]` matches the whole pattern.
//!
//! Zero-width assertions: the subset construction in [`Dfa::try_from`] ignores
//! epsilon *conditions* (it treats every eps edge as unconditional), so a
//! reversed automaton with `\b`/`^`/`$`/`ProgressSince` edges would be wrong.
//! [`reverse_nfa`] therefore returns `None` when any eps edge is conditional;
//! the caller falls back to a plain scan.

use crate::automata::dfa::{DEAD_STATE, Dfa};
use crate::automata::nfa::{EpsCondition, GOAL_STATE, Nfa, State, StateHandle};
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

/// Build the reverse of a tag-free anchored NFA, suitable for [`Dfa::try_from`].
///
/// The reversed automaton recognizes the reversed language: every byte edge
/// `u --range--> v` becomes `v --range--> u`, and every (unconditional) eps edge
/// `u --> v` becomes `v --> u` with its tag ops dropped. Roles swap: the old
/// `GOAL_STATE` becomes the start, and the old start becomes the lone accepting
/// state. Since `Dfa::try_from` hard-codes the accept as `GOAL_STATE` (id 0) and
/// the start as `nfa.start`, we relabel by swapping ids `0` and `old_start` so
/// the old start lands on id 0.
///
/// Returns `None` (no reverse automaton; caller falls back) if any eps edge is
/// conditional — see the module note.
pub(crate) fn reverse_nfa(nfa: &Nfa) -> Option<Nfa> {
    let old_start = nfa.start;
    debug_assert_ne!(old_start, GOAL_STATE, "start is never the goal state");

    // Relabel so the old start becomes id 0 (the new accept / `GOAL_STATE`) and
    // the old goal becomes the new start.
    let swap = |x: StateHandle| -> StateHandle {
        if x == GOAL_STATE {
            old_start
        } else if x == old_start {
            GOAL_STATE
        } else {
            x
        }
    };

    let n = nfa.states.len();
    let mut new_states: Vec<State> = (0..n).map(|_| State::default()).collect();

    for (u, st) in nfa.states.iter().enumerate() {
        let u = u as StateHandle;
        for &(range, v) in &st.transitions {
            // Reverse of `u --range--> v`: from `swap(v)`, consume `range` to
            // reach `swap(u)`. The forward NFA is byte-deterministic per state,
            // but the reversed graph is not — several forward states may
            // transition into the same `v` over overlapping ranges, which
            // `State`'s single-destination-per-byte table can't represent.
            // Route each reversed byte edge through its own fresh intermediate
            // state reached by an (unconditional) eps edge, so no state ever
            // accumulates overlapping byte ranges; the nondeterminism then
            // lives in the eps closure, where `Dfa::try_from` handles it.
            let mid = new_states.len() as StateHandle;
            let mut mid_state = State::default();
            mid_state.add_transition(range, swap(u));
            new_states.push(mid_state);
            new_states[swap(v) as usize].add_eps(mid);
        }
        for e in &st.eps {
            if !matches!(e.cond, EpsCondition::Always) {
                return None;
            }
            new_states[swap(e.target) as usize].add_eps(swap(u));
        }
    }

    Some(Nfa::from_reversed_parts(
        /* start */ swap(GOAL_STATE),
        new_states.into_boxed_slice(),
    ))
}

/// Walk the reverse DFA right-to-left from byte offset `end`, returning the
/// **smallest** start `s` in `min_start..=end` at which it accepts — i.e. the
/// leftmost match start (not before `min_start`) such that `input[s..end]`
/// matches the whole pattern — or `None` if it never accepts in range.
///
/// `min_start` is the current search offset: the reverse walk must not reach
/// back past it, or it could report a match overlapping one already returned
/// (the backtracker, searching forward from `min_start`, would never start a
/// match before it). Each step consumes `input[pos - 1]` and decrements `pos`;
/// accepts are recorded as we reach further back, so the final answer is the
/// longest in-range backward match.
pub(crate) fn reverse_find_start(
    dfa: &Dfa,
    input: &[u8],
    end: usize,
    min_start: usize,
) -> Option<usize> {
    let byte_to_class = dfa.byte_to_class();
    let transitions = dfa.transitions();
    let accepting = dfa.accepting();
    let num_classes = dfa.num_classes();

    let mut state = dfa.start();
    // An accept right at `end` would mean the pattern matches the empty string;
    // for a required-suffix pattern that can't happen, but handle it uniformly.
    let mut best = if accepting[state as usize] {
        Some(end)
    } else {
        None
    };

    let mut pos = end;
    // Stop at `min_start`: never consume `input[min_start - 1]`, so no recorded
    // accept is below `min_start`.
    while pos > min_start && state != DEAD_STATE {
        let class = byte_to_class[input[pos - 1] as usize] as usize;
        state = transitions[state as usize * num_classes + class];
        pos -= 1;
        if state == DEAD_STATE {
            break;
        }
        if accepting[state as usize] {
            best = Some(pos);
        }
    }
    best
}
