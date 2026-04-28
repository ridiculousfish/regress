//! Tagged DFA — priority-ordered subset construction over a TNFA.
//!
//! Milestone 2: determinization with order-sensitive state identity (so greedy
//! and lazy quantifiers produce different automata), but no tag tracking yet —
//! `tag_map`s stay empty throughout and `TagCommand` sequences are unused.
//! Phase A (mark minting and command emission) lands in a later milestone.

use crate::automata::dfa::{compute_byte_classes, representative_bytes};
use crate::automata::nfa::{GOAL_STATE, Nfa, StateHandle};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use smallvec::SmallVec;
use std::collections::{HashMap, VecDeque};

pub type TdfaStateId = u32;

/// The dead state: all transitions loop to self, not accepting.
pub const TDFA_DEAD_STATE: TdfaStateId = 0;

/// Committed-accept sentinel: every reachable state stays accepting, so the
/// match answer is already known to be `0..input.len()`. All transitions
/// self-loop. Accepting.
pub const TDFA_COMMITTED_ACCEPT_STATE: TdfaStateId = 1;

/// IDs `<= TDFA_LAST_SENTINEL` are sentinels. The executor's hot-path check
/// is a single comparison against this bound.
pub const TDFA_LAST_SENTINEL: TdfaStateId = TDFA_COMMITTED_ACCEPT_STATE;

/// Maximum number of TDFA states before we bail out. Matches
/// `dfa::DFA_STATE_BUDGET`.
const TDFA_STATE_BUDGET: usize = 4096;

#[derive(Debug)]
pub enum Error {
    BudgetExceeded,
}

/// An abstract tag-version identifier. During Phase A construction every
/// register write on an epsilon edge mints a fresh `InputMark`. A later pass
/// (Phase B) maps many `InputMark`s onto a smaller set of physical registers.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct InputMark(pub u32);

/// Source operand of a tag command: what value to write into the destination
/// `InputMark`. A tag command is a single assignment executed when the TDFA
/// takes a transition (or on accept); the `src` names where the value comes
/// from.
///
/// - `CurrentPos` — stamp the mark with the current input offset. This is how
///   capture-group boundaries and full-match endpoints get recorded.
/// - `Copy` — reuse another mark's value verbatim. Emitted by canonicalization
///   and, later, by register-allocation reconciliation to move data between
///   marks without re-reading input.
/// - `Nil` — clear the mark to the "unset" sentinel (e.g. an optional capture
///   group that didn't participate on this path).
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum MarkValue {
    CurrentPos,
    Copy(InputMark),
    Nil,
}

/// A single tag-mark assignment performed on a transition or on accept.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct TagCommand {
    pub dst: InputMark,
    pub src: MarkValue,
}

/// One member of a TDFA configuration: an NFA state plus the per-tag version
/// map recording which `InputMark` currently holds each tag's value in this
/// entry.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct TaggedNfaState {
    pub state: StateHandle,
    /// Indexed by the TNFA's global tag/register index. Length equals the
    /// automaton's total tag count and is uniform across every entry in a
    /// configuration (so index-by-index comparison in equality and
    /// canonicalization is well-defined). `None` means the tag has not been
    /// written on the path reaching this state.
    pub tag_map: SmallVec<[Option<InputMark>; 4]>,
}

/// One TDFA state: an ordered list of `TaggedNfaState` threads. Order encodes
/// priority (earliest = highest), so `[A, B]` and `[B, A]` are distinct TDFA
/// states — this is what lets greedy and lazy quantifiers produce different
/// automata. `Eq`/`Hash` are order-sensitive.
///
/// Called a *configuration* in the TDFA literature (Laurikari 2000;
/// Trofimovich 2017). We use `TdfaState` here because it pairs cleanly with
/// `TdfaStateId` as "contents vs. handle."
#[derive(Clone, Eq, PartialEq, Hash, Debug, Default)]
pub struct TdfaState(pub SmallVec<[TaggedNfaState; 4]>);

/// Renumber the `InputMark`s in `cfg` into a canonical form. Canonical ids are
/// assigned in order of first appearance when walking the configuration in
/// priority order (entries in order, within each entry `tag_map` in index
/// order).
///
/// Returns the canonical configuration and the command sequence that moves
/// each raw `InputMark`'s value into its canonical destination. The command
/// list is empty when the input is already canonical.
pub fn canonicalize(cfg: TdfaState) -> (TdfaState, SmallVec<[TagCommand; 4]>) {
    // `mapping[raw] = canon` records which canonical id each raw mark was
    // assigned. A single raw mark may appear in multiple entries / tag slots;
    // all occurrences must rewrite to the same canonical id, so we memoize.
    let mut mapping: HashMap<InputMark, InputMark> = HashMap::new();
    let mut entries: SmallVec<[TaggedNfaState; 4]> = SmallVec::new();

    // Canonical ids are handed out 0, 1, 2, ... in the order raw marks are
    // first encountered during the priority-order walk below. Two states
    // that differ only in raw numbering end up byte-identical after this,
    // which is what makes the determinization loop's dedup map work.
    let mut next_canonical_mark = InputMark(0);
    let mut next_canonical = move || -> InputMark {
        let res = next_canonical_mark;
        next_canonical_mark.0 += 1;
        res
    };

    // Walk threads in priority order (outer loop) and, within each thread,
    // tag slots in index order (inner loop). This fixed traversal is what
    // makes "first appearance" a well-defined notion.
    for entry in cfg.0 {
        let mut tag_map = SmallVec::with_capacity(entry.tag_map.len());
        for &slot in &entry.tag_map {
            // `None` (unset tag) passes through unchanged — only real marks
            // get renumbered. First sight of a raw mark mints a fresh
            // canonical id; subsequent sights reuse the memoized one.
            let canon = slot.map(|raw| *mapping.entry(raw).or_insert_with(&mut next_canonical));
            tag_map.push(canon);
        }
        tag_map.shrink_to_fit();
        entries.push(TaggedNfaState {
            state: entry.state,
            tag_map,
        });
    }

    // The caller attaches these commands to the incoming DFA edge: they
    // copy the values currently held in raw marks into the canonical slots
    // of the (possibly pre-existing) destination state. Without them, the
    // renumbering would silently discard captured positions.
    //
    // We invert the mapping (keyed by canonical id) and sort so the
    // emitted sequence is deterministic regardless of HashMap iteration
    // order — important for testability and for downstream Eq/Hash of
    // transition tables.
    let mut pairs: Vec<(InputMark, InputMark)> = mapping
        .into_iter()
        .map(|(raw, canon)| (canon, raw))
        .collect();
    pairs.sort();

    // Skip no-op `canon := canon` copies — when `raw == canon` the value is
    // already in the right place. This is why an already-canonical input
    // produces an empty command list.
    let commands = pairs
        .into_iter()
        .filter(|&(canon, raw)| canon != raw)
        .map(|(canon, raw)| TagCommand {
            dst: canon,
            src: MarkValue::Copy(raw),
        })
        .collect();

    (TdfaState(entries), commands)
}

/// Priority-ordered epsilon closure. Pre-order DFS: first visit to an NFA
/// state wins, later duplicates are dropped. Within each NFA state, epsilon
/// edges are walked in source order (`State.eps` is already priority-ordered
/// by the builder). Seeds contribute their priorities in the order given.
///
/// In Milestone 2 `tag_map`s stay empty and epsilon ops are ignored; the
/// only thing priority ordering buys us here is that greedy and lazy
/// quantifiers produce different closures.
fn close_priority(nfa: &Nfa, seeds: &[TaggedNfaState]) -> TdfaState {
    let mut threads: SmallVec<[TaggedNfaState; 4]> = SmallVec::new();
    let mut seen = vec![false; nfa.states.len()];

    // DFS with an explicit stack: push in reverse so the first seed / first
    // eps edge comes off the stack first, preserving source order in output.
    let mut stack: Vec<TaggedNfaState> = seeds.iter().rev().cloned().collect();

    while let Some(thread) = stack.pop() {
        if seen[thread.state as usize] {
            continue;
        }
        seen[thread.state as usize] = true;
        let state = thread.state;
        threads.push(thread);
        let eps = &nfa.states[state as usize].eps;
        for edge in eps.iter().rev() {
            if !seen[edge.target as usize] {
                // Tag ops on eps edges are ignored in M2; tag_map inherits
                // unchanged from the parent thread (empty throughout M2).
                stack.push(TaggedNfaState {
                    state: edge.target,
                    tag_map: SmallVec::new(),
                });
            }
        }
    }

    TdfaState(threads)
}

/// Leftmost-greedy truncation: if any thread is at GOAL, keep `[0..=goal_idx]`
/// and drop lower-priority threads after it (they can only produce worse
/// matches). Higher-priority threads before the goal stay alive as live
/// continuations — on a longer input they might still reach GOAL and win.
fn truncate_at_first_goal(mut s: TdfaState) -> TdfaState {
    if let Some(idx) = s.0.iter().position(|t| t.state == GOAL_STATE) {
        s.0.truncate(idx + 1);
    }
    s
}

#[derive(Debug)]
pub struct Tdfa {
    start: TdfaStateId,
    num_classes: usize,
    byte_to_class: [u8; 256],
    transitions: Box<[TdfaStateId]>,
    accepting: Box<[bool]>,
}

/// Post-construction pass: detect *committed-dead* states (can never reach an
/// accepting state) and *committed-accept* states (accepting, and every
/// transitively reachable state is also accepting), merge them into the two
/// sentinel IDs `TDFA_DEAD_STATE` (0) and `TDFA_COMMITTED_ACCEPT_STATE` (1),
/// and renumber the survivors to IDs `>= 2`. The executor's hot-path check
/// becomes `state <= TDFA_LAST_SENTINEL`.
fn rewrite_with_sentinels(
    old_start: TdfaStateId,
    num_classes: usize,
    old_trans: &[TdfaStateId],
    old_accept: &[bool],
) -> (TdfaStateId, Box<[TdfaStateId]>, Box<[bool]>) {
    let n = old_accept.len();

    // Reverse adjacency — one entry per (src, class), duplicates fine.
    let mut rev: Vec<Vec<u32>> = vec![Vec::new(); n];
    for s in 0..n {
        for c in 0..num_classes {
            let t = old_trans[s * num_classes + c] as usize;
            rev[t].push(s as u32);
        }
    }

    // (1) reaches_accept[s]: some accepting state is reachable from s. BFS
    // backward from accepting states through the reverse graph.
    let mut reaches_accept = vec![false; n];
    let mut q: VecDeque<usize> = VecDeque::new();
    for s in 0..n {
        if old_accept[s] {
            reaches_accept[s] = true;
            q.push_back(s);
        }
    }
    while let Some(t) = q.pop_front() {
        for &s in &rev[t] {
            let s = s as usize;
            if !reaches_accept[s] {
                reaches_accept[s] = true;
                q.push_back(s);
            }
        }
    }

    // (2) committed_accept[s]: accepting AND every transition goes to another
    // committed_accept. Start from {accepting}, remove any s with a transition
    // to a non-committed state, propagate removals through predecessors.
    let mut committed_accept = old_accept.to_vec();
    let mut q: VecDeque<usize> = VecDeque::new();
    for s in 0..n {
        if committed_accept[s] {
            for c in 0..num_classes {
                let t = old_trans[s * num_classes + c] as usize;
                if !old_accept[t] {
                    committed_accept[s] = false;
                    q.push_back(s);
                    break;
                }
            }
        }
    }
    while let Some(removed) = q.pop_front() {
        for &p in &rev[removed] {
            let p = p as usize;
            if committed_accept[p] {
                committed_accept[p] = false;
                q.push_back(p);
            }
        }
    }

    // (3) Remap. Sentinels occupy IDs 0 and 1; everyone else gets IDs >= 2 in
    // original order.
    let mut remap = vec![0u32; n];
    let mut next_id: u32 = TDFA_LAST_SENTINEL + 1;
    for s in 0..n {
        if !reaches_accept[s] {
            remap[s] = TDFA_DEAD_STATE;
        } else if committed_accept[s] {
            remap[s] = TDFA_COMMITTED_ACCEPT_STATE;
        } else {
            remap[s] = next_id;
            next_id += 1;
        }
    }

    let new_n = next_id as usize;
    let mut new_trans = vec![TDFA_DEAD_STATE; new_n * num_classes];
    let mut new_accept = vec![false; new_n];

    // Sentinels: self-loop; dead non-accepting, committed-accept accepting.
    for c in 0..num_classes {
        new_trans[TDFA_DEAD_STATE as usize * num_classes + c] = TDFA_DEAD_STATE;
        new_trans[TDFA_COMMITTED_ACCEPT_STATE as usize * num_classes + c] =
            TDFA_COMMITTED_ACCEPT_STATE;
    }
    new_accept[TDFA_DEAD_STATE as usize] = false;
    new_accept[TDFA_COMMITTED_ACCEPT_STATE as usize] = true;

    // Real states: translate transitions through remap.
    for s in 0..n {
        let new_s = remap[s] as usize;
        if (new_s as TdfaStateId) <= TDFA_LAST_SENTINEL {
            continue;
        }
        new_accept[new_s] = old_accept[s];
        for c in 0..num_classes {
            let t = old_trans[s * num_classes + c] as usize;
            new_trans[new_s * num_classes + c] = remap[t];
        }
    }

    (
        remap[old_start as usize],
        new_trans.into_boxed_slice(),
        new_accept.into_boxed_slice(),
    )
}

impl Tdfa {
    pub fn try_from(nfa: &Nfa) -> Result<Self, Error> {
        let (byte_to_class, num_classes) = compute_byte_classes(nfa);
        let rep_bytes = representative_bytes(&byte_to_class, num_classes);

        let mut state_map: HashMap<TdfaState, TdfaStateId> = HashMap::new();
        let mut transitions: Vec<TdfaStateId> = Vec::new();
        let mut accepting: Vec<bool> = Vec::new();
        let mut worklist: Vec<TdfaState> = Vec::new();

        // State 0 = dead state (self-loops, not accepting). Represented as
        // the empty TdfaState so that an exhausted step() lands here.
        state_map.insert(TdfaState::default(), TDFA_DEAD_STATE);
        transitions.resize(num_classes, TDFA_DEAD_STATE);
        accepting.push(false);

        // Start state = priority-ordered closure of {nfa.start}, canonicalized.
        let start_seed = TaggedNfaState {
            state: nfa.start(),
            tag_map: SmallVec::new(),
        };
        let start_closure = close_priority(nfa, &[start_seed]);
        let start_closure = truncate_at_first_goal(start_closure);
        let (canon_start, _cmds) = canonicalize(start_closure);
        let start_id: TdfaStateId = 1;
        let start_accepting = canon_start.0.iter().any(|t| t.state == GOAL_STATE);
        state_map.insert(canon_start.clone(), start_id);
        transitions.resize(transitions.len() + num_classes, TDFA_DEAD_STATE);
        accepting.push(start_accepting);
        worklist.push(canon_start);

        while let Some(state) = worklist.pop() {
            let dfa_state = state_map[&state];
            let row_offset = dfa_state as usize * num_classes;

            for class in 0..num_classes {
                let rep = rep_bytes[class];

                // Priority-ordered step: walk threads in order, take each
                // byte transition, seed the next closure. Threads with no
                // outgoing byte edge simply drop out.
                let mut seeds: SmallVec<[TaggedNfaState; 4]> = SmallVec::new();
                for thread in &state.0 {
                    if let Some(tgt) = nfa.states[thread.state as usize].transition_for_byte(rep) {
                        seeds.push(TaggedNfaState {
                            state: tgt,
                            tag_map: SmallVec::new(),
                        });
                    }
                }
                if seeds.is_empty() {
                    continue; // Already TDFA_DEAD_STATE.
                }

                let next = close_priority(nfa, &seeds);
                let next = truncate_at_first_goal(next);
                let (canon_next, _cmds) = canonicalize(next);

                let target_id = match state_map.get(&canon_next) {
                    Some(&id) => id,
                    None => {
                        let id = accepting.len() as TdfaStateId;
                        if id as usize >= TDFA_STATE_BUDGET {
                            return Err(Error::BudgetExceeded);
                        }
                        let is_accepting = canon_next.0.iter().any(|t| t.state == GOAL_STATE);
                        accepting.push(is_accepting);
                        transitions.resize(transitions.len() + num_classes, TDFA_DEAD_STATE);
                        state_map.insert(canon_next.clone(), id);
                        worklist.push(canon_next);
                        id
                    }
                };
                transitions[row_offset + class] = target_id;
            }
        }

        let (start, transitions, accepting) = rewrite_with_sentinels(
            start_id,
            num_classes,
            &transitions,
            &accepting,
        );

        Ok(Tdfa {
            start,
            num_classes,
            byte_to_class,
            transitions,
            accepting,
        })
    }

    pub fn num_states(&self) -> usize {
        self.accepting.len()
    }

    pub fn num_classes(&self) -> usize {
        self.num_classes
    }

    pub fn start(&self) -> TdfaStateId {
        self.start
    }

    pub fn byte_to_class(&self) -> &[u8; 256] {
        &self.byte_to_class
    }

    pub fn transitions(&self) -> &[TdfaStateId] {
        &self.transitions
    }

    pub fn accepting(&self) -> &[bool] {
        &self.accepting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(state: StateHandle, tags: &[u32]) -> TaggedNfaState {
        TaggedNfaState {
            state,
            tag_map: tags.iter().map(|&v| Some(InputMark(v))).collect(),
        }
    }

    fn cfg(entries: &[TaggedNfaState]) -> TdfaState {
        TdfaState(entries.iter().cloned().collect())
    }

    #[test]
    fn configuration_eq_is_order_sensitive() {
        let a = entry(1, &[3, 5]);
        let b = entry(2, &[3, 5]);
        assert_ne!(cfg(&[a.clone(), b.clone()]), cfg(&[b, a]));
    }

    #[test]
    fn configuration_hash_is_order_sensitive() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let a = entry(1, &[3, 5]);
        let b = entry(2, &[3, 5]);
        let ab = cfg(&[a.clone(), b.clone()]);
        let ba = cfg(&[b, a]);
        let mut ha = DefaultHasher::new();
        let mut hb = DefaultHasher::new();
        ab.hash(&mut ha);
        ba.hash(&mut hb);
        assert_ne!(ha.finish(), hb.finish());
    }

    #[test]
    fn canonicalize_first_appearance_order() {
        // Raw versions 7, 3, 7, 9 canonicalize to 0, 1, 0, 2.
        let c = cfg(&[entry(0, &[7, 3]), entry(1, &[7, 9])]);
        let (canon, _) = canonicalize(c);
        assert_eq!(canon, cfg(&[entry(0, &[0, 1]), entry(1, &[0, 2])]));
    }

    #[test]
    fn canonicalize_is_idempotent() {
        let c = cfg(&[entry(0, &[7, 3]), entry(1, &[7, 9])]);
        let (once, _) = canonicalize(c.clone());
        let (twice, cmds) = canonicalize(once.clone());
        assert_eq!(once, twice);
        assert!(cmds.is_empty());
    }

    #[test]
    fn canonicalize_iso_configs_collapse() {
        let a = cfg(&[entry(0, &[3, 5]), entry(1, &[5, 3])]);
        let b = cfg(&[entry(0, &[100, 200]), entry(1, &[200, 100])]);
        assert_eq!(canonicalize(a).0, canonicalize(b).0);
    }

    #[test]
    fn canonicalize_emits_copy_commands_in_canonical_order() {
        // Raw 7 -> canonical 0, raw 3 -> canonical 1.
        let c = cfg(&[entry(0, &[7, 3])]);
        let (_, cmds) = canonicalize(c);
        assert_eq!(
            cmds.as_slice(),
            &[
                TagCommand {
                    dst: InputMark(0),
                    src: MarkValue::Copy(InputMark(7)),
                },
                TagCommand {
                    dst: InputMark(1),
                    src: MarkValue::Copy(InputMark(3)),
                },
            ]
        );
    }

    #[test]
    fn canonicalize_already_canonical_emits_no_commands() {
        let c = cfg(&[entry(0, &[0, 1]), entry(1, &[0, 2])]);
        let (canon, cmds) = canonicalize(c.clone());
        assert_eq!(canon, c);
        assert!(cmds.is_empty());
    }

    #[test]
    fn empty_configuration_canonicalizes_to_empty() {
        let empty = TdfaState::default();
        let (canon, cmds) = canonicalize(empty.clone());
        assert_eq!(canon, empty);
        assert!(cmds.is_empty());
    }
}
