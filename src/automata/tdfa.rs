//! Tagged DFA — priority-ordered subset construction over a TNFA.
//!
//! Phase A (this milestone): epsilon writes mint fresh `InputMark`s during
//! determinization, and commands ride on incoming TDFA transitions to update
//! a runtime `marks` array. On accept, per-state `finals` copy canonical
//! marks into the runtime register slots used by `NfaMatch`.

use crate::automata::dfa::{compute_byte_classes, representative_bytes};
use crate::automata::nfa::{GOAL_STATE, Nfa, StateHandle, TagIdx};
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

/// Source of a `FinalCommand`: copy a canonical mark into the runtime tag
/// slot, or write the unset sentinel.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct FinalCommand {
    pub tag: TagIdx,
    pub src: MarkValue,
}

/// Mints fresh, globally-unique `InputMark` IDs during construction.
struct MarkAlloc(u32);
impl MarkAlloc {
    fn new() -> Self {
        Self(0)
    }
    fn next(&mut self) -> InputMark {
        let m = InputMark(self.0);
        self.0 += 1;
        m
    }
    fn count(&self) -> u32 {
        self.0
    }
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
/// Each child thread inherits its parent's `tag_map`. When traversing an
/// `EpsEdge` whose `ops` is non-empty, fresh `InputMark`s are minted for the
/// named tags and a `CurrentPos` command is emitted per minted mark — the
/// caller stitches these onto the incoming TDFA transition's command list.
fn close_priority(
    alloc: &mut MarkAlloc,
    nfa: &Nfa,
    seeds: &[TaggedNfaState],
) -> (TdfaState, SmallVec<[TagCommand; 4]>) {
    let mut threads: SmallVec<[TaggedNfaState; 4]> = SmallVec::new();
    let mut commands: SmallVec<[TagCommand; 4]> = SmallVec::new();
    let mut seen = vec![false; nfa.states.len()];

    // DFS with an explicit stack: push in reverse so the first seed / first
    // eps edge comes off the stack first, preserving source order in output.
    // Mark `seen` at push time (not pop) so we don't mint duplicate marks for
    // a child that will later be dropped by dedup — and so that lower-priority
    // siblings correctly see "already reached" once a higher-priority path has
    // claimed an NFA state.
    let mut stack: Vec<TaggedNfaState> = seeds.iter().rev().cloned().collect();
    for seed in seeds.iter().rev() {
        if seen[seed.state as usize] {
            continue;
        }
        seen[seed.state as usize] = true;
        stack.push(seed.clone());
    }
    while let Some(thread) = stack.pop() {
        let parent_tag_map = thread.tag_map.clone();
        let state = thread.state;
        threads.push(thread);
        let eps = &nfa.states[state as usize].eps;
        for edge in eps.iter().rev() {
            if seen[edge.target as usize] {
                continue;
            }
            seen[edge.target as usize] = true;
            let mut child_tag_map = parent_tag_map.clone();
            for &reg in &edge.ops {
                let m = alloc.next();
                child_tag_map[reg as usize] = Some(m);
                commands.push(TagCommand {
                    dst: m,
                    src: MarkValue::CurrentPos,
                });
            }
            stack.push(TaggedNfaState {
                state: edge.target,
                tag_map: child_tag_map,
            });
        }
    }

    (TdfaState(threads), commands)
}

/// Build an all-`None` tag map of length `num_tags` for seeding a new entry.
fn empty_tag_map(num_tags: usize) -> SmallVec<[Option<InputMark>; 4]> {
    let mut v = SmallVec::with_capacity(num_tags);
    v.resize(num_tags, None);
    v
}

/// Synthesize the `finals` row for a (canonicalized) configuration. Reads the
/// first GOAL thread's `tag_map` — leftmost-greedy / leftmost-first semantics
/// already baked in by truncate-at-first-GOAL. Non-accepting states get an
/// empty list.
fn synthesize_finals(canon: &TdfaState, num_tags: usize) -> SmallVec<[FinalCommand; 4]> {
    let goal = match canon.0.iter().find(|t| t.state == GOAL_STATE) {
        Some(t) => t,
        None => return SmallVec::new(),
    };
    let mut out: SmallVec<[FinalCommand; 4]> = SmallVec::new();
    for tag in 0..num_tags {
        let src = match goal.tag_map.get(tag).copied().flatten() {
            Some(mark) => MarkValue::Copy(mark),
            None => MarkValue::Nil,
        };
        out.push(FinalCommand {
            tag: tag as TagIdx,
            src,
        });
    }
    out
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
    num_tags: usize,
    num_marks: usize,
    byte_to_class: [u8; 256],
    transitions: Box<[TdfaStateId]>,
    accepting: Box<[bool]>,
    /// Same shape as `transitions`: indexed by `state * num_classes + class`.
    /// Each entry is the sequence of `TagCommand`s to apply when that
    /// transition fires (CurrentPos writes first, then Copy writes from
    /// canonicalization).
    transition_commands: Box<[SmallVec<[TagCommand; 4]>]>,
    /// Per-state finalization. Indexed by state. For accepting states, this is
    /// `num_tags` commands (one per tag); for non-accepting states, empty.
    finals: Box<[SmallVec<[FinalCommand; 4]>]>,
    /// Commands to apply once at scan start (before consuming any input). These
    /// come from the start state's epsilon closure.
    entry_commands: SmallVec<[TagCommand; 4]>,
}

/// Post-construction pass: detect *committed-dead* states (can never reach an
/// accepting state) and merge them into `TDFA_DEAD_STATE`. Renumber survivors
/// to IDs `>= 2`. The committed-accept sentinel slot is preserved for
/// invariants, but folding into it is disabled in M3 because it would discard
/// per-state `finals` and outbound command lists.
fn rewrite_with_sentinels(
    old_start: TdfaStateId,
    num_classes: usize,
    num_tags: usize,
    old_trans: &[TdfaStateId],
    old_accept: &[bool],
    old_trans_cmds: &[SmallVec<[TagCommand; 4]>],
    old_finals: &[SmallVec<[FinalCommand; 4]>],
) -> (
    TdfaStateId,
    Box<[TdfaStateId]>,
    Box<[bool]>,
    Box<[SmallVec<[TagCommand; 4]>]>,
    Box<[SmallVec<[FinalCommand; 4]>]>,
) {
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

    // (2) Committed-accept folding disabled in M3: would lose per-state finals
    // and outbound command lists. The sentinel slot still exists but no real
    // state remaps to it.

    // (3) Remap. Sentinels occupy IDs 0 and 1; everyone else gets IDs >= 2 in
    // original order.
    let mut remap = vec![0u32; n];
    let mut next_id: u32 = TDFA_LAST_SENTINEL + 1;
    for s in 0..n {
        if !reaches_accept[s] {
            remap[s] = TDFA_DEAD_STATE;
        } else {
            remap[s] = next_id;
            next_id += 1;
        }
    }

    let new_n = next_id as usize;
    let mut new_trans = vec![TDFA_DEAD_STATE; new_n * num_classes];
    let mut new_accept = vec![false; new_n];
    let mut new_trans_cmds: Vec<SmallVec<[TagCommand; 4]>> =
        vec![SmallVec::new(); new_n * num_classes];
    let mut new_finals: Vec<SmallVec<[FinalCommand; 4]>> = vec![SmallVec::new(); new_n];

    // Sentinels: self-loop; dead non-accepting, committed-accept accepting.
    for c in 0..num_classes {
        new_trans[TDFA_DEAD_STATE as usize * num_classes + c] = TDFA_DEAD_STATE;
        new_trans[TDFA_COMMITTED_ACCEPT_STATE as usize * num_classes + c] =
            TDFA_COMMITTED_ACCEPT_STATE;
    }
    new_accept[TDFA_DEAD_STATE as usize] = false;
    new_accept[TDFA_COMMITTED_ACCEPT_STATE as usize] = true;
    // The committed-accept sentinel slot needs a finals row even though no real
    // state currently remaps to it (folding is disabled in M3). Synthesize a
    // trivial all-Nil finalization so the executor would still produce a valid
    // (empty-capture) NfaMatch if it ever did land there.
    let mut sentinel_finals: SmallVec<[FinalCommand; 4]> = SmallVec::new();
    for tag in 0..num_tags {
        sentinel_finals.push(FinalCommand {
            tag: tag as TagIdx,
            src: MarkValue::Nil,
        });
    }
    new_finals[TDFA_COMMITTED_ACCEPT_STATE as usize] = sentinel_finals;

    // Real states: translate transitions through remap.
    for s in 0..n {
        let new_s = remap[s] as usize;
        if (new_s as TdfaStateId) <= TDFA_LAST_SENTINEL {
            continue;
        }
        new_accept[new_s] = old_accept[s];
        new_finals[new_s] = old_finals[s].clone();
        for c in 0..num_classes {
            let t = old_trans[s * num_classes + c] as usize;
            new_trans[new_s * num_classes + c] = remap[t];
            // Drop command lists on transitions that are now dead — the
            // executor never inspects them once it sees the sentinel.
            if remap[t] == TDFA_DEAD_STATE {
                new_trans_cmds[new_s * num_classes + c] = SmallVec::new();
            } else {
                new_trans_cmds[new_s * num_classes + c] =
                    old_trans_cmds[s * num_classes + c].clone();
            }
        }
    }

    (
        remap[old_start as usize],
        new_trans.into_boxed_slice(),
        new_accept.into_boxed_slice(),
        new_trans_cmds.into_boxed_slice(),
        new_finals.into_boxed_slice(),
    )
}

impl Tdfa {
    pub fn try_from(nfa: &Nfa) -> Result<Self, Error> {
        let (byte_to_class, num_classes) = compute_byte_classes(nfa);
        let rep_bytes = representative_bytes(&byte_to_class, num_classes);
        let num_tags = nfa.num_tags();

        let mut alloc = MarkAlloc::new();

        let mut state_map: HashMap<TdfaState, TdfaStateId> = HashMap::new();
        let mut transitions: Vec<TdfaStateId> = Vec::new();
        let mut accepting: Vec<bool> = Vec::new();
        let mut transition_commands: Vec<SmallVec<[TagCommand; 4]>> = Vec::new();
        let mut finals: Vec<SmallVec<[FinalCommand; 4]>> = Vec::new();
        let mut worklist: Vec<TdfaState> = Vec::new();

        // State 0 = dead state (self-loops, not accepting). Represented as
        // the empty TdfaState so that an exhausted step() lands here.
        state_map.insert(TdfaState::default(), TDFA_DEAD_STATE);
        transitions.resize(num_classes, TDFA_DEAD_STATE);
        transition_commands.resize(num_classes, SmallVec::new());
        accepting.push(false);
        finals.push(SmallVec::new());

        // Start state = priority-ordered closure of {nfa.start}, canonicalized.
        // The initial closure may emit CurrentPos commands (e.g. the
        // FULL_MATCH_START write); we capture them as `entry_commands` to fire
        // once at scan start before any input is consumed.
        let start_seed = TaggedNfaState {
            state: nfa.start(),
            tag_map: empty_tag_map(num_tags),
        };
        let (start_closure, start_current_cmds) = close_priority(&mut alloc, nfa, &[start_seed]);
        let start_closure = truncate_at_first_goal(start_closure);
        let (canon_start, start_copy_cmds) = canonicalize(start_closure);
        let mut entry_commands: SmallVec<[TagCommand; 4]> = SmallVec::new();
        entry_commands.extend(start_current_cmds.into_iter());
        entry_commands.extend(start_copy_cmds.into_iter());

        let start_id: TdfaStateId = 1;
        let start_accepting = canon_start.0.iter().any(|t| t.state == GOAL_STATE);
        let start_finals = synthesize_finals(&canon_start, num_tags);
        state_map.insert(canon_start.clone(), start_id);
        transitions.resize(transitions.len() + num_classes, TDFA_DEAD_STATE);
        transition_commands.resize(transition_commands.len() + num_classes, SmallVec::new());
        accepting.push(start_accepting);
        finals.push(start_finals);
        worklist.push(canon_start);

        while let Some(state) = worklist.pop() {
            let dfa_state = state_map[&state];
            let row_offset = dfa_state as usize * num_classes;

            for class in 0..num_classes {
                let rep = rep_bytes[class];

                // Priority-ordered step: walk threads in order, take each
                // byte transition, seed the next closure. Threads carry their
                // tag_map verbatim across the byte step (byte transitions
                // don't write registers in the NFA).
                let mut seeds: SmallVec<[TaggedNfaState; 4]> = SmallVec::new();
                for thread in &state.0 {
                    if let Some(tgt) = nfa.states[thread.state as usize].transition_for_byte(rep) {
                        seeds.push(TaggedNfaState {
                            state: tgt,
                            tag_map: thread.tag_map.clone(),
                        });
                    }
                }
                if seeds.is_empty() {
                    continue; // Already TDFA_DEAD_STATE.
                }

                let (next, current_cmds) = close_priority(&mut alloc, nfa, &seeds);
                let next = truncate_at_first_goal(next);
                let (canon_next, copy_cmds) = canonicalize(next);

                let mut combined: SmallVec<[TagCommand; 4]> = SmallVec::new();
                combined.extend(current_cmds.into_iter());
                combined.extend(copy_cmds.into_iter());

                let target_id = match state_map.get(&canon_next) {
                    Some(&id) => id,
                    None => {
                        let id = accepting.len() as TdfaStateId;
                        if id as usize >= TDFA_STATE_BUDGET {
                            return Err(Error::BudgetExceeded);
                        }
                        let is_accepting = canon_next.0.iter().any(|t| t.state == GOAL_STATE);
                        let state_finals = synthesize_finals(&canon_next, num_tags);
                        accepting.push(is_accepting);
                        finals.push(state_finals);
                        transitions.resize(transitions.len() + num_classes, TDFA_DEAD_STATE);
                        transition_commands
                            .resize(transition_commands.len() + num_classes, SmallVec::new());
                        state_map.insert(canon_next.clone(), id);
                        worklist.push(canon_next);
                        id
                    }
                };
                transitions[row_offset + class] = target_id;
                transition_commands[row_offset + class] = combined;
            }
        }

        let num_marks = alloc.count() as usize;
        let (start, transitions, accepting, transition_commands, finals) = rewrite_with_sentinels(
            start_id,
            num_classes,
            num_tags,
            &transitions,
            &accepting,
            &transition_commands,
            &finals,
        );

        Ok(Tdfa {
            start,
            num_classes,
            num_tags,
            num_marks,
            byte_to_class,
            transitions,
            accepting,
            transition_commands,
            finals,
            entry_commands,
        })
    }

    pub fn num_tags(&self) -> usize {
        self.num_tags
    }

    pub fn num_marks(&self) -> usize {
        self.num_marks
    }

    pub fn transition_commands(&self) -> &[SmallVec<[TagCommand; 4]>] {
        &self.transition_commands
    }

    pub fn finals(&self) -> &[SmallVec<[FinalCommand; 4]>] {
        &self.finals
    }

    pub fn entry_commands(&self) -> &[TagCommand] {
        &self.entry_commands
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
