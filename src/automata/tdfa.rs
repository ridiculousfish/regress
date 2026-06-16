//! Tagged DFA — priority-ordered subset construction over a TNFA.
//!
//! Phase A (this milestone): epsilon writes mint fresh `InputMark`s during
//! determinization, and commands ride on incoming TDFA transitions to update
//! a runtime `marks` array. On accept, per-state `finals` copy canonical
//! marks into the runtime register slots used by `NfaMatch`.

use crate::automata::dfa::{compute_byte_classes, representative_bytes};
use crate::automata::nfa::{EpsCondition, GOAL_STATE, Nfa, OpKind, StateHandle, TagIdx, TagOp};

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};

pub type TdfaStateId = u32;

/// The dead state: all transitions loop to self, not accepting. The executor
/// short-circuits when it sees this state.
pub const TDFA_DEAD_STATE: TdfaStateId = 0;

/// Committed-accept sentinel: once entered, the match is committed.
pub const TDFA_COMMITTED_ACCEPT_STATE: TdfaStateId = 1;

/// Highest sentinel id; real states start at `TDFA_LAST_SENTINEL + 1`.
pub const TDFA_LAST_SENTINEL: TdfaStateId = TDFA_COMMITTED_ACCEPT_STATE;

/// Maximum number of TDFA states before we bail out. Matches
/// `dfa::DFA_STATE_BUDGET`.
const TDFA_STATE_BUDGET: usize = 4096;

#[derive(Debug)]
pub enum Error {
    BudgetExceeded,
    /// The source NFA contains predicated eps edges (`^`/`$`/`\b`) that the
    /// current TDFA construction doesn't yet handle. Use the NFA backend
    /// directly until TDFA-side support lands.
    PredicatedEpsNotSupported,
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
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum MarkValue {
    CurrentPos,
    Copy(InputMark),
}

/// A single tag-mark assignment performed on a transition or on accept.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct TagCommand {
    pub dst: InputMark,
    pub src: MarkValue,
}

/// An ordered list of tag commands, applied in sequence on a transition or at
/// scan start.
pub type TagCommandList = SmallVec<[TagCommand; 4]>;

/// A conditional-accept hook attached to a TDFA state. Records what happens
/// if a `$`-style predicate fires at this state (the eps target's mini-
/// closure runs entirely via epsilon, terminating at `GOAL_STATE`).
///
/// At runtime the executor evaluates `cond` against the current input
/// position; if it holds, it snapshots the current marks, applies
/// `commands` (CurrentPos writes from the eps walk), and uses `finals` to
/// extract per-tag values for a match candidate.
#[derive(Clone, Debug)]
pub struct AnchorConditional {
    pub cond: EpsCondition,
    pub commands: TagCommandList,
    pub finals: SmallVec<[FinalCommand; 4]>,
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
/// Returns the canonical configuration, the command sequence that moves
/// each raw `InputMark`'s value into its canonical destination, and the
/// raw→canonical mapping (used by callers to retroactively renumber
/// per-state conditionals' tag references — see
/// `rewrite_conditional_finals`).
pub fn canonicalize(cfg: TdfaState) -> (TdfaState, TagCommandList, HashMap<InputMark, InputMark>) {
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
    let mut next_canonical = || -> InputMark {
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
    let mut pairs: Vec<(InputMark, InputMark)> =
        mapping.iter().map(|(&raw, &canon)| (canon, raw)).collect();
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

    (TdfaState(entries), commands, mapping)
}

/// Rewrite each conditional's `finals` Copy sources from raw marks to
/// canonical ones, using the main subset's canonicalize mapping. Marks
/// that aren't in the mapping (e.g. the mini-closure's own
/// `FULL_MATCH_END` write) are left as raw — those slots are written by
/// the conditional's own `commands` at fire time, not by the standard
/// transition's `Copy(raw → canon)` pass.
fn rewrite_conditional_finals(
    conditionals: &mut [AnchorConditional],
    mapping: &HashMap<InputMark, InputMark>,
) {
    for ac in conditionals {
        for cmd in &mut ac.finals {
            if let MarkValue::Copy(raw) = &mut cmd.src {
                if let Some(&canon) = mapping.get(raw) {
                    *raw = canon;
                }
            }
        }
    }
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
    num_tags: usize,
    at_start_of_input: bool,
    multiline_start_fires: bool,
    // List of `(invert, unicode_icase)` WordBoundary predicates that
    // should be traversed in this closure (i.e., "assume `\b`/`\B` fires").
    // Used by `compute_anchor_alt_for` to build the alt closures. The
    // primary closure passes an empty slice — WordBoundary edges are
    // skipped, leaving the predicate to fire at runtime via anchor_alt.
    wb_fires: &[(bool, bool)],
    conditionals: &mut SmallVec<[AnchorConditional; 1]>,
) -> Result<(TdfaState, TagCommandList), Error> {
    let mut threads: SmallVec<[TaggedNfaState; 4]> = SmallVec::new();
    let mut commands = TagCommandList::new();
    let mut seen = vec![false; nfa.states.len()];
    // Marks allocated below this id existed before this closure started.
    // Any mark with id >= closure_start_mark was created within this
    // closure — used by `ProgressSince` to detect "sentinel was written
    // in this same eps closure" (which means no input was consumed between
    // sentinel-write and the gated check, i.e. the body matched empty).
    let closure_start_mark = alloc.count();

    // Priority-ordered DFS with an explicit stack. `seen` is marked when a
    // state is *popped* (visited), not when pushed — this is the standard
    // iterative pre-order. Children are pushed in reverse, so the highest-
    // priority sibling (and its whole subtree) is drained before any lower-
    // priority sibling; the first pop of a state is therefore via the
    // highest-priority path that reaches it, and it claims the state (and its
    // tag map). Later, lower-priority arrivals pop and are skipped.
    //
    // Marking at pop (rather than push) matters when a state is reachable
    // both by a high-priority *descendant* of one sibling and by a lower-
    // priority sibling directly. Pushing marks every child up front, so a
    // lower-priority sibling — pushed before the earlier sibling's subtree is
    // expanded — would claim the state and the higher-priority continuation
    // would be dropped. That happens for a greedy loop over a non-greedy
    // nullable body like `(a*?)*`, where the "keep iterating" continuation
    // must outrank the loop-exit edge to GOAL.
    //
    // Seeds are drained one at a time, in priority order, so a higher-
    // priority seed's closure claims any shared state before a later seed is
    // considered — needed for adjacent loops like `(a*)(a{1,2})`.
    let mut stack: Vec<TaggedNfaState> = Vec::new();
    for seed in seeds {
        stack.push(seed.clone());
        while let Some(thread) = stack.pop() {
            if seen[thread.state as usize] {
                continue;
            }
            seen[thread.state as usize] = true;
            let parent_tag_map = thread.tag_map.clone();
            let state = thread.state;
            threads.push(thread);
            let eps = &nfa.states[state as usize].eps;
            for edge in eps.iter().rev() {
                match &edge.cond {
                    EpsCondition::Always => {
                        // Falls through to normal traversal below.
                    }
                    EpsCondition::StartOfLine { multiline: false } => {
                        // Non-multiline `^` only fires at start of input. We
                        // build two initial closures (anchored vs unanchored);
                        // this flag distinguishes them.
                        if !at_start_of_input {
                            continue;
                        }
                        // Fall through to normal traversal below.
                    }
                    EpsCondition::StartOfLine { multiline: true } => {
                        // Multiline `^` fires at pos 0 or right after a line
                        // terminator. `at_start_of_input` covers pos 0; the
                        // `multiline_start_fires` flag is used by the alt
                        // closure computed for `anchor_alt`, where the caller
                        // asserts "assume ^ fired here."
                        if !(at_start_of_input || multiline_start_fires) {
                            continue;
                        }
                        // Fall through to normal traversal below.
                    }
                    EpsCondition::WordBoundary {
                        invert,
                        unicode_icase,
                    } => {
                        // Treat as firing only if the caller passed this exact
                        // (invert, unicode_icase) tuple in `wb_fires`. The primary
                        // closure passes none — alt closures (computed by
                        // `compute_anchor_alt_for`) pass the specific tuple they
                        // represent. Mirrors the multiline-^ scheme.
                        if !wb_fires.contains(&(*invert, *unicode_icase)) {
                            continue;
                        }
                        // Fall through to normal traversal below.
                    }
                    EpsCondition::ProgressSince(sentinel) => {
                        // Runtime check `marks[sentinel] < current_pos` resolves
                        // statically here as provenance: the sentinel's value at
                        // the time of this check is whatever is in `parent_tag_map`.
                        // If that mark was allocated within this same eps closure,
                        // no input has been consumed since the write, so
                        // `marks[sentinel] == current_pos` → predicate FAILS.
                        // Otherwise (mark was inherited from a prior closure, or
                        // the slot is empty) → predicate HOLDS.
                        let sentinel_idx = *sentinel as usize;
                        let written_in_this_closure = match parent_tag_map.get(sentinel_idx) {
                            Some(Some(m)) => m.0 >= closure_start_mark,
                            _ => false,
                        };
                        if written_in_this_closure {
                            continue;
                        }
                        // Fall through to normal traversal.
                    }
                    EpsCondition::EndOfLine { .. } => {
                        // Don't expand `$` into the determinized subset — record a
                        // conditional accept hook instead. Mini-closure must
                        // terminate at `GOAL_STATE` via only-eps; otherwise the
                        // path can't be captured by a per-position accept and
                        // we have to bail.
                        let mut sub_tag_map = parent_tag_map.clone();
                        let mut pre_cmds = TagCommandList::new();
                        apply_eps_ops(&edge.ops, alloc, &mut sub_tag_map, &mut pre_cmds);
                        let seed = TaggedNfaState {
                            state: edge.target,
                            tag_map: sub_tag_map,
                        };
                        let mut sub_conds: SmallVec<[AnchorConditional; 1]> = SmallVec::new();
                        let (sub_closure, sub_cmds) = close_priority(
                            alloc,
                            nfa,
                            &[seed],
                            num_tags,
                            at_start_of_input,
                            multiline_start_fires,
                            wb_fires,
                            &mut sub_conds,
                        )?;
                        let sub_closure = truncate_at_first_goal(sub_closure);
                        // Bail iff the mini-closure could continue consuming
                        // bytes — that means `$` is followed by more byte
                        // matching, which the per-position accept hook can't
                        // represent. Pure-eps relay states on the path to GOAL
                        // (e.g. the synthetic `goal_start` build_goal creates
                        // to write FULL_MATCH_END) have no byte transitions
                        // and are harmless.
                        let has_byte_continuation = sub_closure.0.iter().any(|t| {
                            t.state != GOAL_STATE
                                && !nfa.states[t.state as usize].transitions.is_empty()
                        });
                        if has_byte_continuation {
                            return Err(Error::PredicatedEpsNotSupported);
                        }
                        if !sub_closure.0.iter().any(|t| t.state == GOAL_STATE) {
                            continue; // Mini-closure didn't reach GOAL — no accept.
                        }
                        let mut all_cmds = TagCommandList::new();
                        all_cmds.extend(pre_cmds);
                        all_cmds.extend(sub_cmds);
                        let finals = synthesize_finals(&sub_closure, num_tags);
                        conditionals.push(AnchorConditional {
                            cond: edge.cond.clone(),
                            commands: all_cmds,
                            finals,
                        });
                        continue;
                    }
                }
                // Early-out for states already claimed (popped). Not-yet-
                // popped duplicates are allowed onto the stack; the pop-time
                // `seen` check keeps the first (highest-priority) one and
                // drops the rest.
                if seen[edge.target as usize] {
                    continue;
                }
                let mut child_tag_map = parent_tag_map.clone();
                apply_eps_ops(&edge.ops, alloc, &mut child_tag_map, &mut commands);
                stack.push(TaggedNfaState {
                    state: edge.target,
                    tag_map: child_tag_map,
                });
            }
        }
    }

    Ok((TdfaState(threads), commands))
}

/// Build an all-`None` tag map of length `num_tags` for seeding a new entry.
fn empty_tag_map(num_tags: usize) -> SmallVec<[Option<InputMark>; 4]> {
    let mut v = SmallVec::with_capacity(num_tags);
    v.resize(num_tags, None);
    v
}

/// Apply an eps edge's tag-write ops to the child tag_map.
/// `CurrentPos` mints a fresh mark and emits a `TagCommand` so the
/// executor writes the input position at runtime. `Nil` clears the
/// slot to `None` directly — no mark, no command.
fn apply_eps_ops(
    ops: &[TagOp],
    alloc: &mut MarkAlloc,
    child_tag_map: &mut SmallVec<[Option<InputMark>; 4]>,
    commands: &mut TagCommandList,
) {
    for op in ops {
        match op.kind {
            OpKind::CurrentPos => {
                let m = alloc.next();
                child_tag_map[op.tag as usize] = Some(m);
                commands.push(TagCommand {
                    dst: m,
                    src: MarkValue::CurrentPos,
                });
            }
            OpKind::Nil => {
                child_tag_map[op.tag as usize] = None;
            }
        }
    }
}

/// Working state for the TDFA construction. Bundles every per-state Vec
/// so `register_or_get_state` and friends don't need a dozen
/// out-parameters.
struct Build<'a> {
    nfa: &'a Nfa,
    alloc: &'a mut MarkAlloc,
    num_tags: usize,
    num_classes: usize,
    state_map: HashMap<TdfaState, TdfaStateId>,
    transitions: Vec<TdfaStateId>,
    accepting: Vec<bool>,
    transition_commands: Vec<TagCommandList>,
    finals: Vec<SmallVec<[FinalCommand; 4]>>,
    anchor_conditionals: Vec<SmallVec<[AnchorConditional; 1]>>,
    anchor_alts: Vec<SmallVec<[AnchorAlt; 1]>>,
    worklist: Vec<TdfaState>,
}

impl Build<'_> {
    /// Look up or register a canonical state. Returns `(id, true)` if a
    /// brand-new state was added (caller may want to compute its
    /// `anchor_alt`); `(id, false)` if it was already in the map.
    fn register_or_get_state(
        &mut self,
        canon: TdfaState,
        conds: SmallVec<[AnchorConditional; 1]>,
    ) -> Result<(TdfaStateId, bool), Error> {
        if let Some(&id) = self.state_map.get(&canon) {
            return Ok((id, false));
        }
        let id = self.accepting.len() as TdfaStateId;
        if id as usize >= TDFA_STATE_BUDGET {
            return Err(Error::BudgetExceeded);
        }
        let is_accepting = canon.0.iter().any(|t| t.state == GOAL_STATE);
        let state_finals = synthesize_finals(&canon, self.num_tags);
        self.accepting.push(is_accepting);
        self.finals.push(state_finals);
        self.anchor_conditionals.push(conds);
        self.anchor_alts.push(SmallVec::new());
        self.transitions
            .resize(self.transitions.len() + self.num_classes, TDFA_DEAD_STATE);
        self.transition_commands.resize(
            self.transition_commands.len() + self.num_classes,
            SmallVec::new(),
        );
        self.state_map.insert(canon.clone(), id);
        self.worklist.push(canon);
        Ok((id, true))
    }

    /// Run the priority-ordered closure on the given seeds. The returned
    /// `(canon, current_cmds + copy_cmds, conditionals)` is what
    /// `register_or_get_state` consumes.
    fn closure_from_seeds(
        &mut self,
        seeds: &[TaggedNfaState],
        at_start_of_input: bool,
        multiline_start_fires: bool,
        wb_fires: &[(bool, bool)],
    ) -> Result<(TdfaState, TagCommandList, SmallVec<[AnchorConditional; 1]>), Error> {
        let mut conds: SmallVec<[AnchorConditional; 1]> = SmallVec::new();
        let (closure, current_cmds) = close_priority(
            self.alloc,
            self.nfa,
            seeds,
            self.num_tags,
            at_start_of_input,
            multiline_start_fires,
            wb_fires,
            &mut conds,
        )?;
        let closure = truncate_at_first_goal(closure);
        let (canon, copy_cmds, canon_mapping) = canonicalize(closure);
        // Conditionals were built during close_priority using *raw* mark
        // ids; the standard transition's `Copy(raw → canon)` commands
        // move values into canonical slots, but a self-loop into the same
        // state on subsequent bytes only overwrites the canonical slots,
        // leaving raw slots stale. Rewriting the finals to read canonical
        // slots fixes this. (Marks introduced by the mini-closure itself
        // — e.g. FULL_MATCH_END — aren't in the main mapping and stay raw,
        // since they're written by the conditional's own commands at
        // fire time.)
        rewrite_conditional_finals(&mut conds, &canon_mapping);
        let mut entry = TagCommandList::new();
        entry.extend(current_cmds);
        entry.extend(copy_cmds);
        Ok((canon, entry, conds))
    }

    /// If `canon` could be enlarged by a predicated eps firing — multiline
    /// `^` or one of the `\b`/`\B` flavors — compute each alt closure and
    /// push it onto `self.anchor_alts[id]`. Each alt is independent; the
    /// executor evaluates predicates in registration order and switches
    /// to the first matching alt.
    fn compute_anchor_alt_for(
        &mut self,
        canon: &TdfaState,
        seeds: &[TaggedNfaState],
        at_start_of_input: bool,
        id: TdfaStateId,
    ) -> Result<(), Error> {
        // Collect the distinct predicate kinds reachable from this state's
        // threads. Each becomes a candidate alt.
        let mut has_multiline_caret = false;
        let mut wb_predicates: SmallVec<[(bool, bool); 2]> = SmallVec::new();
        for thread in &canon.0 {
            for edge in &self.nfa.states[thread.state as usize].eps {
                match &edge.cond {
                    EpsCondition::StartOfLine { multiline: true } => {
                        has_multiline_caret = true;
                    }
                    EpsCondition::WordBoundary {
                        invert,
                        unicode_icase,
                    } => {
                        let key = (*invert, *unicode_icase);
                        if !wb_predicates.contains(&key) {
                            wb_predicates.push(key);
                        }
                    }
                    _ => {}
                }
            }
        }
        if has_multiline_caret {
            self.register_alt(
                canon,
                seeds,
                at_start_of_input,
                /* multiline_start_fires */ true,
                &[],
                EpsCondition::StartOfLine { multiline: true },
                id,
            )?;
        }
        for (invert, unicode_icase) in wb_predicates {
            self.register_alt(
                canon,
                seeds,
                at_start_of_input,
                /* multiline_start_fires */ false,
                &[(invert, unicode_icase)],
                EpsCondition::WordBoundary {
                    invert,
                    unicode_icase,
                },
                id,
            )?;
        }
        Ok(())
    }

    /// Compute and register one alt closure. If the resulting subset
    /// differs from `canon`, register it as a state and append an
    /// `AnchorAlt` entry on the source state's `anchor_alts` list.
    #[allow(clippy::too_many_arguments)]
    fn register_alt(
        &mut self,
        canon: &TdfaState,
        seeds: &[TaggedNfaState],
        at_start_of_input: bool,
        multiline_start_fires: bool,
        wb_fires: &[(bool, bool)],
        cond: EpsCondition,
        id: TdfaStateId,
    ) -> Result<(), Error> {
        let (canon_alt, _entry_alt, conds_alt) =
            self.closure_from_seeds(seeds, at_start_of_input, multiline_start_fires, wb_fires)?;
        if canon_alt == *canon {
            return Ok(());
        }
        let switch_commands = compute_alt_switch_commands(canon, &canon_alt);
        let (alt_id, _is_new) = self.register_or_get_state(canon_alt, conds_alt)?;
        self.anchor_alts[id as usize].push(AnchorAlt {
            cond,
            alt: alt_id,
            commands: switch_commands,
        });
        Ok(())
    }
}

/// Translate the marks array's layout from `canon_next` to `canon_alt`.
/// For each `(NFA_state, tag_idx)` entry present in both, emit a `Copy`
/// from the next-state's canonical slot to the alt-state's (when they
/// differ). For entries only in the alt — added by the ^-extension —
/// emit a `CurrentPos`, since their values are the position at which
/// ^ just fired.
fn compute_alt_switch_commands(canon_next: &TdfaState, canon_alt: &TdfaState) -> TagCommandList {
    let mut next_map: HashMap<(StateHandle, usize), InputMark> = HashMap::new();
    for thread in &canon_next.0 {
        for (idx, slot) in thread.tag_map.iter().enumerate() {
            if let Some(mark) = slot {
                next_map.insert((thread.state, idx), *mark);
            }
        }
    }
    let mut commands = TagCommandList::new();
    // A single alt-canonical mark can appear in multiple (state, tag)
    // slots when threads inherited it via eps without rewriting (the
    // common case for `\b` traversal, which has no ops). Track which
    // alt marks we've already emitted a write for so a later "this
    // (state, tag) isn't in primary" path doesn't clobber an earlier
    // correct Copy with a CurrentPos.
    let mut written: HashSet<InputMark> = HashSet::new();
    for thread in &canon_alt.0 {
        for (idx, slot) in thread.tag_map.iter().enumerate() {
            let Some(alt_mark) = slot else { continue };
            if written.contains(alt_mark) {
                continue;
            }
            match next_map.get(&(thread.state, idx)) {
                Some(&std_mark) if std_mark == *alt_mark => {
                    written.insert(*alt_mark);
                }
                Some(&std_mark) => {
                    commands.push(TagCommand {
                        dst: *alt_mark,
                        src: MarkValue::Copy(std_mark),
                    });
                    written.insert(*alt_mark);
                }
                None => {
                    commands.push(TagCommand {
                        dst: *alt_mark,
                        src: MarkValue::CurrentPos,
                    });
                    written.insert(*alt_mark);
                }
            }
        }
    }
    commands
}

/// Build an initial TDFA state's closure under the given `at_start_of_input`
/// flag, register it, and compute its `anchor_alt`. Returns
/// `(id, entry_commands)`. If the canonical subset already exists in the
/// state map, the existing id is reused.
fn seed_initial_state(
    build: &mut Build<'_>,
    at_start_of_input: bool,
) -> Result<(TdfaStateId, TagCommandList), Error> {
    let seed = TaggedNfaState {
        state: build.nfa.start(),
        tag_map: empty_tag_map(build.num_tags),
    };
    let seeds = [seed];
    let (canon, entry_commands, conds) = build.closure_from_seeds(
        &seeds,
        at_start_of_input,
        /* multiline_start_fires */ false,
        /* wb_fires */ &[],
    )?;
    let canon_for_alt = canon.clone();
    let (id, is_new) = build.register_or_get_state(canon, conds)?;
    if is_new {
        build.compute_anchor_alt_for(&canon_for_alt, &seeds, at_start_of_input, id)?;
    }
    Ok((id, entry_commands))
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
        // Skip tags with no surviving thread holding them. The executor
        // initializes tag values to TEXT_POS_NO_MATCH, so absence of a
        // FinalCommand is equivalent to writing "unset".
        if let Some(mark) = goal.tag_map.get(tag).copied().flatten() {
            out.push(FinalCommand {
                tag: tag as TagIdx,
                src: MarkValue::Copy(mark),
            });
        }
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
    /// Initial state when matching is being attempted at byte offset 0 of
    /// the input — `^` non-multiline fires here.
    start_anchored: TdfaStateId,
    /// Initial state for non-zero start offsets — `^` doesn't fire.
    /// Equals `start_anchored` for patterns without `^`.
    start_unanchored: TdfaStateId,
    /// Entry commands paired with `start_anchored`. Run by the executor
    /// before the byte loop when `start == 0`.
    entry_commands_anchored: TagCommandList,
    /// Entry commands paired with `start_unanchored`. Run by the executor
    /// before the byte loop when `start > 0`.
    entry_commands_unanchored: TagCommandList,
    num_classes: usize, // Number of byte equivalence classes.
    num_tags: usize,    // Number of semantic tags (capture positions).
    // Capture-group names cloned from the source NFA so callers can attach
    // them to returned matches without keeping the NFA alive.
    group_names: Box<[Box<str>]>,

    // Size of the executor's memory file. Each `InputMark(N)` that
    // appears in any TagCommand or FinalCommand is an index into a flat array
    // of this size. TODO: register allocation.
    num_marks: usize,

    // 256-entry table mapping each byte to its class ID. The executor's hot
    // loop does `class = byte_to_class[byte]` then `transitions[state *
    // num_classes + class]` to step.
    byte_to_class: [u8; 256],

    // Dense transition table. Indexed by `state * num_classes + class`. The
    // value is the destination state ID. Cells with no real outgoing edge
    // hold `TDFA_DEAD_STATE` so the executor's short-circuit check works
    // without per-cell predicates.
    transitions: Box<[TdfaStateId]>,

    // Per-state accepting flag. Indexed by state ID. True if the regex
    // matches when scanning ends in that state.
    accepting: Box<[bool]>,

    // Tag commands to apply when a transition fires. Same shape as
    // `transitions` (indexed by `state * num_classes + class`). Each entry
    // is a list of `TagCommand`s — CurrentPos writes first, then Copy writes
    // from canonicalization. May be empty when a transition has no tag effect.
    transition_commands: Box<[TagCommandList]>,

    // Per-state finalization commands. Indexed by state ID. For accepting
    // states this is `num_tags` commands (one per tag) describing how to read
    // the final capture positions out of the mark file. For non-accepting
    // states it's empty. Run once at scan end against the last-accepted
    // state's mark snapshot.
    finals: Box<[SmallVec<[FinalCommand; 4]>]>,

    // Per-state list of `$`-style accept conditionals. Indexed by state ID.
    // The executor evaluates `cond` per step + at EOI; if true, applies the
    // entry's commands and uses its finals to produce a match candidate.
    anchor_conditionals: Box<[SmallVec<[AnchorConditional; 1]>]>,

    // Per-state forward-branching anchor alt. Indexed by state ID. `Some`
    // only when the state's subset can be enlarged by a multiline `^`
    // firing — the executor checks the predicate after each byte step;
    // when it holds, applies `commands` (a mix of Copy and CurrentPos
    // writes that rearranges the marks array from this state's layout to
    // the alt's) and switches to `alt`.
    anchor_alts: Box<[SmallVec<[AnchorAlt; 1]>]>,
}

#[derive(Debug, Clone)]
pub struct AnchorAlt {
    pub cond: EpsCondition,
    pub alt: TdfaStateId,
    pub commands: TagCommandList,
}

/// Static size metrics for a built `Tdfa`. Captures the cost of the current
/// (naive) mark allocation so future register-allocation work can be measured
/// against a recorded baseline. Command counts cover every list the executor
/// can run: per-transition commands, both entry-command lists, and per-state
/// anchor-conditional and anchor-alt commands.
#[derive(Debug, Clone, Copy, Default)]
pub struct TdfaStats {
    pub num_states: usize,
    /// Size of the executor's mark file (the per-search `marks` Vec).
    pub num_marks: usize,
    pub total_commands: usize,
    pub copy_commands: usize,
    pub currentpos_commands: usize,
}

impl Tdfa {
    pub fn try_from(nfa: &Nfa) -> Result<Self, Error> {
        let (byte_to_class, num_classes) = compute_byte_classes(nfa);
        let rep_bytes = representative_bytes(&byte_to_class, num_classes);
        let num_tags = nfa.num_tags();

        let mut alloc = MarkAlloc::new();
        let mut build = Build {
            nfa,
            alloc: &mut alloc,
            num_tags,
            num_classes,
            state_map: HashMap::new(),
            transitions: Vec::new(),
            accepting: Vec::new(),
            transition_commands: Vec::new(),
            finals: Vec::new(),
            anchor_conditionals: Vec::new(),
            anchor_alts: Vec::new(),
            worklist: Vec::new(),
        };

        // State 0 = dead state (self-loops, not accepting). Represented as
        // the empty TdfaState so that an exhausted step() lands here.
        build
            .state_map
            .insert(TdfaState::default(), TDFA_DEAD_STATE);
        build.transitions.resize(num_classes, TDFA_DEAD_STATE);
        build
            .transition_commands
            .resize(num_classes, SmallVec::new());
        build.accepting.push(false);
        build.finals.push(SmallVec::new());
        build.anchor_conditionals.push(SmallVec::new());
        build.anchor_alts.push(SmallVec::new());

        // Build both initial states. `start_anchored` is the closure under
        // `at_start_of_input = true`, i.e. with non-multiline `^` eps edges
        // traversable. `start_unanchored` is the closure with them skipped;
        // it's the right starting subset when the executor calls in with a
        // non-zero start offset. They may dedup to the same id if the regex
        // has no `^`.
        let (start_anchored, entry_commands_anchored) =
            seed_initial_state(&mut build, /* at_start_of_input */ true)?;
        let (start_unanchored, entry_commands_unanchored) =
            seed_initial_state(&mut build, /* at_start_of_input */ false)?;

        while let Some(state) = build.worklist.pop() {
            let dfa_state = build.state_map[&state];
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

                let (canon_next, combined, next_conds) = build.closure_from_seeds(
                    &seeds,
                    /* at_start_of_input */ false,
                    /* multiline_start_fires */ false,
                    /* wb_fires */ &[],
                )?;
                let canon_for_alt = canon_next.clone();
                let (target_id, is_new) = build.register_or_get_state(canon_next, next_conds)?;
                build.transitions[row_offset + class] = target_id;
                build.transition_commands[row_offset + class] = combined;
                if is_new {
                    build.compute_anchor_alt_for(
                        &canon_for_alt,
                        &seeds,
                        /* at_start_of_input */ false,
                        target_id,
                    )?;
                }
            }
        }

        let num_marks = build.alloc.count() as usize;

        Ok(Tdfa {
            start_anchored,
            start_unanchored,
            entry_commands_anchored,
            entry_commands_unanchored,
            num_classes,
            num_tags,
            num_marks,
            group_names: nfa.group_names().to_vec().into_boxed_slice(),
            byte_to_class,
            transitions: build.transitions.into_boxed_slice(),
            accepting: build.accepting.into_boxed_slice(),
            transition_commands: build.transition_commands.into_boxed_slice(),
            finals: build.finals.into_boxed_slice(),
            anchor_conditionals: build.anchor_conditionals.into_boxed_slice(),
            anchor_alts: build.anchor_alts.into_boxed_slice(),
        })
    }

    /// `$`-style accept conditionals for `state`. Empty for most states.
    pub fn anchor_conditionals(&self, state: TdfaStateId) -> &[AnchorConditional] {
        &self.anchor_conditionals[state as usize]
    }

    /// Forward-branching anchor alt for `state` (multiline `^`). `None`
    /// for most states. The executor evaluates the predicate after each
    /// byte step; on a hit, applies the carried `commands` to rearrange
    /// the marks array to the alt's layout and switches to the alt id.
    pub(crate) fn anchor_alts(&self, state: TdfaStateId) -> &[AnchorAlt] {
        &self.anchor_alts[state as usize]
    }

    pub fn num_tags(&self) -> usize {
        self.num_tags
    }

    /// Capture-group names indexed by group id (empty when no group is named).
    pub fn group_names(&self) -> &[Box<str>] {
        &self.group_names
    }

    pub fn num_marks(&self) -> usize {
        self.num_marks
    }

    /// Compute static size metrics (see `TdfaStats`).
    pub fn stats(&self) -> TdfaStats {
        let mut total = 0usize;
        let mut copy = 0usize;
        let mut cur = 0usize;
        let mut tally = |cmds: &[TagCommand]| {
            for c in cmds {
                total += 1;
                match c.src {
                    MarkValue::CurrentPos => cur += 1,
                    MarkValue::Copy(_) => copy += 1,
                }
            }
        };
        tally(&self.entry_commands_anchored);
        tally(&self.entry_commands_unanchored);
        for cmds in self.transition_commands.iter() {
            tally(cmds);
        }
        for conds in self.anchor_conditionals.iter() {
            for ac in conds {
                tally(&ac.commands);
            }
        }
        for alts in self.anchor_alts.iter() {
            for alt in alts {
                tally(&alt.commands);
            }
        }
        TdfaStats {
            num_states: self.num_states(),
            num_marks: self.num_marks,
            total_commands: total,
            copy_commands: copy,
            currentpos_commands: cur,
        }
    }

    pub fn transition_commands(&self) -> &[TagCommandList] {
        &self.transition_commands
    }

    pub fn finals(&self) -> &[SmallVec<[FinalCommand; 4]>] {
        &self.finals
    }

    /// Entry commands paired with the chosen initial state for the given
    /// `start` byte offset.
    pub fn entry_commands(&self, start: usize) -> &[TagCommand] {
        if start == 0 {
            &self.entry_commands_anchored
        } else {
            &self.entry_commands_unanchored
        }
    }

    pub fn num_states(&self) -> usize {
        self.accepting.len()
    }

    pub fn num_classes(&self) -> usize {
        self.num_classes
    }

    /// Initial state for the given byte offset. `start == 0` picks the
    /// anchored start (where `^` non-multiline fires); any non-zero offset
    /// picks the unanchored start.
    pub fn start(&self, start: usize) -> TdfaStateId {
        if start == 0 {
            self.start_anchored
        } else {
            self.start_unanchored
        }
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
        let (canon, _, _) = canonicalize(c);
        assert_eq!(canon, cfg(&[entry(0, &[0, 1]), entry(1, &[0, 2])]));
    }

    #[test]
    fn canonicalize_is_idempotent() {
        let c = cfg(&[entry(0, &[7, 3]), entry(1, &[7, 9])]);
        let (once, _, _) = canonicalize(c.clone());
        let (twice, cmds, _) = canonicalize(once.clone());
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
        let (_, cmds, _) = canonicalize(c);
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
        let (canon, cmds, _) = canonicalize(c.clone());
        assert_eq!(canon, c);
        assert!(cmds.is_empty());
    }

    #[test]
    fn empty_configuration_canonicalizes_to_empty() {
        let empty = TdfaState::default();
        let (canon, cmds, _) = canonicalize(empty.clone());
        assert_eq!(canon, empty);
        assert!(cmds.is_empty());
    }
}
