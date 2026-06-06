//! Tagged DFA — priority-ordered subset construction over a TNFA.
//!
//! Phase A (this milestone): epsilon writes mint fresh `InputMark`s during
//! determinization, and commands ride on incoming TDFA transitions to update
//! a runtime `marks` array. On accept, per-state `finals` copy canonical
//! marks into the runtime register slots used by `NfaMatch`.

use crate::automata::dfa::{compute_byte_classes, representative_bytes};
use crate::automata::nfa::{EpsCondition, GOAL_STATE, Nfa, OpKind, StateHandle, TagIdx, TagOp};

/// Whether any state in the NFA has an eps edge gated by `^`.
///
/// We bail on ANY `^` for now, even though the `seed_initial_state` /
/// `at_start_of_input` machinery handles non-multiline `^` correctly
/// in principle.
///
/// The reason is unrelated to anchors: capture-heavy patterns
/// (e.g. `run_regexp_capture_test`'s `^(((N({)?)|(R)|...)+` with 58
/// capture groups / 116 tags) blow past the 4096-state TDFA budget on
/// the regex BODY alone — stripping `^` from that pattern still hits
/// BudgetExceeded after ~14s in release mode. The `^` bail was
/// incidentally masking this pre-existing TDFA scaling issue; lifting
/// it without addressing the budget would regress
/// `run_regexp_capture_test` to a multi-minute hang in debug builds.
///
/// Re-enabling `^` should land alongside either a smaller state budget
/// or a per-step work cap. Multiline `^` additionally needs the
/// alt-state mechanism.
fn nfa_has_start_of_line(nfa: &Nfa) -> bool {
    for state in nfa.states.iter() {
        for edge in &state.eps {
            if matches!(edge.cond, EpsCondition::StartOfLine { .. }) {
                return true;
            }
        }
    }
    false
}
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use smallvec::SmallVec;
use std::collections::HashMap;

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
/// Returns the canonical configuration and the command sequence that moves
/// each raw `InputMark`'s value into its canonical destination. The command
/// list is empty when the input is already canonical.
pub fn canonicalize(cfg: TdfaState) -> (TdfaState, TagCommandList) {
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
    num_tags: usize,
    at_start_of_input: bool,
    conditionals: &mut SmallVec<[AnchorConditional; 1]>,
) -> Result<(TdfaState, TagCommandList), Error> {
    let mut threads: SmallVec<[TaggedNfaState; 4]> = SmallVec::new();
    let mut commands = TagCommandList::new();
    let mut seen = vec![false; nfa.states.len()];

    // DFS with an explicit stack: push in reverse so the first seed / first
    // eps edge comes off the stack first, preserving source order in output.
    // Mark `seen` at push time (not pop) so we don't mint duplicate marks for
    // a child that will later be dropped by dedup — and so that lower-priority
    // siblings correctly see "already reached" once a higher-priority path has
    // claimed an NFA state.
    let mut stack: Vec<TaggedNfaState> = Vec::new();
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
                    // Multiline `^` requires the `anchor_alt` mechanism
                    // (alt-state per state where ^ might fire mid-input);
                    // bail until that lands. We pre-screen at try_from to
                    // make this unreachable in practice, but keep the arm
                    // here for soundness.
                    return Err(Error::PredicatedEpsNotSupported);
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
                        &mut sub_conds,
                    )?;
                    let sub_closure = truncate_at_first_goal(sub_closure);
                    if !sub_closure.0.iter().all(|t| t.state == GOAL_STATE) {
                        // Non-GOAL threads survive the mini-closure: the `$`
                        // is followed by more byte matching. We don't yet
                        // know how to represent that in the TDFA — caller
                        // falls back to the NFA backend.
                        return Err(Error::PredicatedEpsNotSupported);
                    }
                    if sub_closure.0.is_empty() {
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
            if seen[edge.target as usize] {
                continue;
            }
            seen[edge.target as usize] = true;
            let mut child_tag_map = parent_tag_map.clone();
            apply_eps_ops(&edge.ops, alloc, &mut child_tag_map, &mut commands);
            stack.push(TaggedNfaState {
                state: edge.target,
                tag_map: child_tag_map,
            });
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

/// Build an initial TDFA state's closure under the given `at_start_of_input`
/// flag and register it in `state_map`. Returns `(id, entry_commands)`.
/// If the resulting canonical subset is already in `state_map`, the existing
/// id is reused (e.g. when anchored and unanchored closures coincide because
/// the pattern has no `^`).
#[allow(clippy::too_many_arguments)]
fn seed_initial_state(
    nfa: &Nfa,
    alloc: &mut MarkAlloc,
    num_tags: usize,
    at_start_of_input: bool,
    state_map: &mut HashMap<TdfaState, TdfaStateId>,
    transitions: &mut Vec<TdfaStateId>,
    accepting: &mut Vec<bool>,
    transition_commands: &mut Vec<TagCommandList>,
    finals: &mut Vec<SmallVec<[FinalCommand; 4]>>,
    anchor_conditionals: &mut Vec<SmallVec<[AnchorConditional; 1]>>,
    worklist: &mut Vec<TdfaState>,
    num_classes: usize,
) -> Result<(TdfaStateId, TagCommandList), Error> {
    let seed = TaggedNfaState {
        state: nfa.start(),
        tag_map: empty_tag_map(num_tags),
    };
    let mut conds: SmallVec<[AnchorConditional; 1]> = SmallVec::new();
    let (closure, current_cmds) =
        close_priority(alloc, nfa, &[seed], num_tags, at_start_of_input, &mut conds)?;
    let closure = truncate_at_first_goal(closure);
    let (canon, copy_cmds) = canonicalize(closure);
    let mut entry_commands = TagCommandList::new();
    entry_commands.extend(current_cmds);
    entry_commands.extend(copy_cmds);

    if let Some(&id) = state_map.get(&canon) {
        return Ok((id, entry_commands));
    }
    let id = accepting.len() as TdfaStateId;
    if id as usize >= TDFA_STATE_BUDGET {
        return Err(Error::BudgetExceeded);
    }
    let is_accepting = canon.0.iter().any(|t| t.state == GOAL_STATE);
    let state_finals = synthesize_finals(&canon, num_tags);
    accepting.push(is_accepting);
    finals.push(state_finals);
    anchor_conditionals.push(conds);
    transitions.resize(transitions.len() + num_classes, TDFA_DEAD_STATE);
    transition_commands.resize(transition_commands.len() + num_classes, SmallVec::new());
    state_map.insert(canon.clone(), id);
    worklist.push(canon);
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
}

impl Tdfa {
    pub fn try_from(nfa: &Nfa) -> Result<Self, Error> {
        if nfa_has_start_of_line(nfa) {
            return Err(Error::PredicatedEpsNotSupported);
        }

        let (byte_to_class, num_classes) = compute_byte_classes(nfa);
        let rep_bytes = representative_bytes(&byte_to_class, num_classes);
        let num_tags = nfa.num_tags();

        let mut alloc = MarkAlloc::new();

        let mut state_map: HashMap<TdfaState, TdfaStateId> = HashMap::new();
        let mut transitions: Vec<TdfaStateId> = Vec::new();
        let mut accepting: Vec<bool> = Vec::new();
        let mut transition_commands: Vec<TagCommandList> = Vec::new();
        let mut finals: Vec<SmallVec<[FinalCommand; 4]>> = Vec::new();
        let mut anchor_conditionals: Vec<SmallVec<[AnchorConditional; 1]>> = Vec::new();
        let mut worklist: Vec<TdfaState> = Vec::new();

        // State 0 = dead state (self-loops, not accepting). Represented as
        // the empty TdfaState so that an exhausted step() lands here.
        state_map.insert(TdfaState::default(), TDFA_DEAD_STATE);
        transitions.resize(num_classes, TDFA_DEAD_STATE);
        transition_commands.resize(num_classes, SmallVec::new());
        accepting.push(false);
        finals.push(SmallVec::new());
        anchor_conditionals.push(SmallVec::new());

        // Build both initial states. `start_anchored` is the closure under
        // `at_start_of_input = true`, i.e. with non-multiline `^` eps edges
        // traversable. `start_unanchored` is the closure with them skipped;
        // it's the right starting subset when the executor calls in with a
        // non-zero start offset. They may dedup to the same id if the regex
        // has no `^`.
        let (start_anchored, entry_commands_anchored) = seed_initial_state(
            nfa,
            &mut alloc,
            num_tags,
            /* at_start_of_input */ true,
            &mut state_map,
            &mut transitions,
            &mut accepting,
            &mut transition_commands,
            &mut finals,
            &mut anchor_conditionals,
            &mut worklist,
            num_classes,
        )?;
        let (start_unanchored, entry_commands_unanchored) = seed_initial_state(
            nfa,
            &mut alloc,
            num_tags,
            /* at_start_of_input */ false,
            &mut state_map,
            &mut transitions,
            &mut accepting,
            &mut transition_commands,
            &mut finals,
            &mut anchor_conditionals,
            &mut worklist,
            num_classes,
        )?;

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

                let mut next_conds: SmallVec<[AnchorConditional; 1]> = SmallVec::new();
                let (next, current_cmds) = close_priority(
                    &mut alloc,
                    nfa,
                    &seeds,
                    num_tags,
                    /* at_start_of_input */ false,
                    &mut next_conds,
                )?;
                let next = truncate_at_first_goal(next);
                let (canon_next, copy_cmds) = canonicalize(next);

                let mut combined = TagCommandList::new();
                combined.extend(current_cmds);
                combined.extend(copy_cmds);

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
                        anchor_conditionals.push(next_conds);
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
            transitions: transitions.into_boxed_slice(),
            accepting: accepting.into_boxed_slice(),
            transition_commands: transition_commands.into_boxed_slice(),
            finals: finals.into_boxed_slice(),
            anchor_conditionals: anchor_conditionals.into_boxed_slice(),
        })
    }

    /// `$`-style accept conditionals for `state`. Empty for most states.
    pub fn anchor_conditionals(&self, state: TdfaStateId) -> &[AnchorConditional] {
        &self.anchor_conditionals[state as usize]
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
