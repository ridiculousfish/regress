//! NFA execution backend.

use crate::automata::nfa::{
    FULL_MATCH_END, FULL_MATCH_START, GOAL_STATE, Nfa, StateHandle, TEXT_POS_NO_MATCH, TagIdx,
    TextPos,
};
use crate::automata::util::BitSet;
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

    fn tags_to_captures(&self, num_capture_tags: usize) -> Vec<Option<Range<usize>>> {
        tags_to_captures(&self.tags[..num_capture_tags])
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

/// A priority-ordered set of `Thread`s, each at a distinct NFA state.
///
/// Push order encodes priority (earliest = highest). A push whose state is
/// already present is silently dropped — this implements leftmost-first
/// semantics. Membership checks use a bitset keyed by `StateHandle`.
struct ThreadSet {
    threads: Vec<Thread>,
    present: BitSet,
}

impl ThreadSet {
    fn new(num_states: usize) -> Self {
        Self {
            threads: Vec::new(),
            present: BitSet::new(num_states),
        }
    }

    /// Whether a thread for `state` is already in the set.
    #[inline]
    fn contains(&self, state: StateHandle) -> bool {
        self.present.test(state as usize)
    }

    /// Try to add a thread. Returns true if added, false if a thread for that
    /// state was already present.
    #[inline]
    fn push(&mut self, thread: Thread) -> bool {
        let idx = thread.state as usize;
        if self.present.test(idx) {
            return false;
        }
        self.present.set(idx);
        self.threads.push(thread);
        true
    }

    fn clear(&mut self) {
        self.threads.clear();
        self.present.clear_all();
    }

    fn iter(&self) -> core::slice::Iter<'_, Thread> {
        self.threads.iter()
    }

    fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    /// Index of the first thread at `GOAL_STATE`, if any. Because the set
    /// is priority-ordered, this is the highest-priority goal currently
    /// reachable.
    fn position_of_goal(&self) -> Option<usize> {
        self.threads.iter().position(|t| t.state == GOAL_STATE)
    }

    /// Drop all threads from `idx` onward (lower-priority continuations).
    /// Mirrors `tdfa::truncate_at_first_goal`'s pruning, applied
    /// dynamically: once a goal is seen at priority `idx`, nothing at
    /// `idx..` can win, so they're dead.
    fn truncate_to(&mut self, idx: usize) {
        for thread in &self.threads[idx..] {
            self.present.clear(thread.state as usize);
        }
        self.threads.truncate(idx);
    }
}

/// Build an `NfaMatch` snapshot from a thread that's reached `GOAL_STATE`.
fn match_from_thread(thread: &Thread, num_capture_tags: usize) -> NfaMatch {
    NfaMatch {
        range: thread.get_tag(FULL_MATCH_START)..thread.get_tag(FULL_MATCH_END),
        captures: thread.tags_to_captures(num_capture_tags),
    }
}

/// If any thread is at `GOAL_STATE`, record its match in `best` and prune
/// the set down to strictly higher-priority threads. Returns `true` if a
/// goal was seen (caller may then check `is_empty()` to decide whether to
/// stop). See `tdfa::truncate_at_first_goal` for the same idea applied
/// statically during DFA construction.
fn prune_and_record(
    threads: &mut ThreadSet,
    best: &mut Option<NfaMatch>,
    num_capture_tags: usize,
) -> bool {
    if let Some(idx) = threads.position_of_goal() {
        *best = Some(match_from_thread(&threads.threads[idx], num_capture_tags));
        threads.truncate_to(idx);
        true
    } else {
        false
    }
}

/// Execute the NFA against a slice of bytes.
///
/// Leftmost-first semantics: at every step, after the eps closure, the
/// first thread (in priority order) at `GOAL_STATE` defines the current
/// best match; threads at lower priority are pruned (they can never win).
/// Only strictly higher-priority threads continue stepping — if one of
/// them later reaches `GOAL_STATE`, it overwrites the best (it's a
/// higher-priority match). When all surviving threads die, the saved
/// `best` is the answer.
/// Attempt to match the NFA against `input` anchored at byte offset
/// `start`. Predicate evaluation (`^`, `$`) operates against the FULL
/// `input` at the actual byte position (`start + i`), so `^` non-multiline
/// fires only when `start == 0` — not when the harness happens to be
/// trying a non-zero start position via slice indexing.
pub fn execute(nfa: &Nfa, input: &[u8], start: usize) -> Option<NfaMatch> {
    let mut current_threads = ThreadSet::new(nfa.states.len());
    let mut next_threads = ThreadSet::new(nfa.states.len());
    let num_tags = nfa.num_tags();
    let mut best: Option<NfaMatch> = None;

    // Start at the start state.
    current_threads.push(Thread::new(nfa.start(), num_tags));
    epsilon_closure_with_tags(nfa, &mut current_threads, input, start);

    // Initial closure may already reach GOAL (e.g. patterns that accept
    // the empty input).
    if prune_and_record(&mut current_threads, &mut best, nfa.num_capture_tags())
        && current_threads.is_empty()
    {
        return best;
    }

    for (i, &byte) in input[start..].iter().enumerate() {
        let pos = start + i;
        for thread in current_threads.iter() {
            let state = nfa.at(thread.state);
            if let Some(next_state) = state.transition_for_byte(byte) {
                next_threads.push(thread.clone_to_state(next_state));
            }
        }
        if next_threads.is_empty() {
            return best;
        }

        epsilon_closure_with_tags(nfa, &mut next_threads, input, pos + 1);

        // Goals seen at this step are higher priority than `best` only if
        // they come from threads that were strictly higher in priority
        // than the previously-recorded goal — which is exactly what the
        // pruning maintains as an invariant on `current_threads`.
        if prune_and_record(&mut next_threads, &mut best, nfa.num_capture_tags())
            && next_threads.is_empty()
        {
            return best;
        }

        core::mem::swap(&mut current_threads, &mut next_threads);
        next_threads.clear();
    }

    best
}

/// Add all epsilon-reachable states to the given thread set, updating tags.
///
/// Traversal is depth-first in eps-edge priority order: for each thread
/// already in the set (in priority order), we recursively expand its eps
/// subtree before moving to the next sibling. The push order into the set
/// IS the priority order — leftmost-first execution depends on this. A
/// breadth-first worklist would interleave high- and low-priority paths
/// and produce wrong matches for non-greedy quantifiers (the higher-
/// priority "exit non-greedy loop" path could land behind the lower-
/// priority "keep looping" path in the set, defeating pruning).
fn epsilon_closure_with_tags(
    nfa: &Nfa,
    threads: &mut ThreadSet,
    input: &[u8],
    current_pos: TextPos,
) {
    let initial_n = threads.threads.len();
    for idx in 0..initial_n {
        dfs_expand_eps(nfa, threads, idx, input, current_pos);
    }
}

/// Depth-first expansion of `threads[idx]`'s eps subtree. Pushes new
/// threads at the end of the set in DFS order, then recurses into each
/// newly-pushed thread before processing the next sibling eps edge.
/// Predicated eps edges (`^`, `$`) are skipped when their predicate
/// doesn't hold at `current_pos`.
fn dfs_expand_eps(
    nfa: &Nfa,
    threads: &mut ThreadSet,
    idx: usize,
    input: &[u8],
    current_pos: TextPos,
) {
    let parent_state = threads.threads[idx].state;
    let state = nfa.at(parent_state);
    for edge_idx in 0..state.eps.len() {
        // Re-read each iteration to dodge the &state borrow held across the
        // recursive call (which also borrows nfa).
        let (target, ops_len, cond_holds) = {
            let e = &nfa.at(parent_state).eps[edge_idx];
            (
                e.target,
                e.ops.len(),
                e.cond.holds(input, current_pos, &threads.threads[idx].tags),
            )
        };
        if !cond_holds {
            continue;
        }
        if threads.contains(target) {
            continue;
        }
        let mut new_thread = threads.threads[idx].clone_to_state(target);
        for op_idx in 0..ops_len {
            let op = nfa.at(parent_state).eps[edge_idx].ops[op_idx];
            *new_thread.get_tag_mut(op.tag) = match op.kind {
                crate::automata::nfa::OpKind::CurrentPos => current_pos,
                crate::automata::nfa::OpKind::Nil => TEXT_POS_NO_MATCH,
            };
        }
        threads.push(new_thread);
        let new_idx = threads.threads.len() - 1;
        dfs_expand_eps(nfa, threads, new_idx, input, current_pos);
    }
}
