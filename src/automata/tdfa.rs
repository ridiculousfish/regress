//! Tagged DFA — priority-ordered subset construction over a TNFA.
//!
//! Phase A (this milestone): epsilon writes mint fresh `InputMark`s during
//! determinization, and commands ride on incoming TDFA transitions to update
//! a runtime `marks` array. On accept, per-state `finals` copy canonical
//! marks into the runtime register slots used by `NfaMatch`.

mod opt;

#[cfg(feature = "tdfa-jit")]
pub mod jit;

use crate::automata::dfa::{compute_byte_classes, representative_bytes};
use crate::automata::nfa::{
    EpsCondition, FULL_MATCH_START, GOAL_STATE, Nfa, OpKind, StateHandle, TagIdx, TagOp,
};

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

/// High bit marking an accepting target in the capture-free `exec_transitions`
/// fast table (see [`Tdfa::exec_transitions`]). State budget is far below this,
/// so it never collides with a real (premultiplied) state value.
pub const EXEC_ACCEPT_FLAG: u32 = 1 << 31;
/// Mask recovering the premultiplied state from an `exec_transitions` entry.
pub const EXEC_STATE_MASK: u32 = !EXEC_ACCEPT_FLAG;

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

/// One step of a transition's compiled mark update: `buf[dst] = buf[src]`,
/// applied in order, in place, by the executor.
///
/// The executor's mark file is laid out as `[marks[0..num_marks], clear,
/// current_pos, scratch]` — real marks keep their natural index; the trailing
/// lanes are `clear` (= `TEXT_POS_NO_MATCH`) at `num_marks`, the current input
/// offset at `num_marks + 1`, and a `scratch` slot at `num_marks + 2`. A `src`
/// of the `current_pos` lane stamps the position; the `scratch` lane is used
/// only to break copy cycles (see [`compile_moves`]).
///
/// Unlike a full-width gather, a move sequence touches **only** the lanes that
/// change — no width-proportional identity copy and no double buffer.
#[derive(Clone, Copy, Debug)]
pub struct MoveOp {
    pub dst: u16,
    pub src: u16,
}

/// Compile a [`TagCommandList`] into an ordered, in-place [`MoveOp`] sequence
/// over a mark file of `num_marks` real marks plus the trailing `clear` /
/// `current_pos` / `scratch` lanes.
///
/// The command list is a *parallel* (simultaneous) assignment: `CurrentPos`
/// writes stamp the current position, and a `Copy` reads its source's value as
/// of *before* the assignment — except a `Copy` whose source is itself stamped
/// by a `CurrentPos` in this same list takes the freshly stamped value. We
/// resolve every destination to one of: the constant `current_pos` lane, the
/// *old* value of some mark, or (when a cycle forces it) the `scratch` lane,
/// then sequentialize so every old-value read precedes the overwrite of that
/// mark (the classic parallel-copy ordering). Because each destination has a
/// single source, each dependency component holds at most one cycle, so a
/// single `scratch` lane — saved once per cycle and consumed before the
/// component finishes — suffices. An empty command list yields an empty
/// sequence (the executor skips it, leaving the mark file untouched).
fn compile_moves(cmds: &[TagCommand], num_marks: usize) -> Box<[MoveOp]> {
    if cmds.is_empty() {
        return Box::default();
    }
    debug_assert!(num_marks + 3 <= u16::MAX as usize, "caller guards the u16 fit");
    let curpos = (num_marks + 1) as u16;
    let scratch = (num_marks + 2) as u16;

    // Index every scratch array by a dense, *local* numbering of only the marks
    // this list mentions (as a destination or a `Copy` source). `compile_moves`
    // runs once per transition, so sizing the scratch arrays to the global mark
    // file (`num_marks`, which reaches tens of thousands for large character
    // classes) would make construction quadratic; a command list touches only a
    // handful of marks. `marks[local] -> global` recovers the real id on emit.
    let mut marks: SmallVec<[u16; 8]> = SmallVec::new();
    for c in cmds {
        let d = c.dst.0 as u16;
        if !marks.contains(&d) {
            marks.push(d);
        }
        if let MarkValue::Copy(s) = c.src {
            let s = s.0 as u16;
            if !marks.contains(&s) {
                marks.push(s);
            }
        }
    }
    let n = marks.len();
    // Every dst/src above was interned, so this lookup always succeeds.
    let local = |m: u16| marks.iter().position(|&x| x == m).unwrap();

    // Marks stamped by a `CurrentPos` write in this list: a `Copy` reading such
    // a mark takes the freshly stamped value (the constant `current_pos`).
    let mut stamped = vec![false; n];
    for c in cmds {
        if matches!(c.src, MarkValue::CurrentPos) {
            stamped[local(c.dst.0 as u16)] = true;
        }
    }

    // Resolve each destination's source. `Const` reads `current_pos` (order-
    // independent); `Mark(s)` reads the *old* value of (local) mark `s` (must
    // precede overwriting `s`); `Scratch` reads a value saved while breaking a
    // cycle.
    #[derive(Clone, Copy)]
    enum Src {
        Const,
        Mark(usize),
        Scratch,
    }
    let mut pred: Vec<Option<Src>> = vec![None; n];
    // `read_count[m]` = number of pending assignments still reading old `m`.
    let mut read_count = vec![0u32; n];
    for c in cmds {
        let dst = local(c.dst.0 as u16);
        let new = match c.src {
            MarkValue::CurrentPos => Src::Const,
            MarkValue::Copy(s) if stamped[local(s.0 as u16)] => Src::Const,
            MarkValue::Copy(s) if s.0 == c.dst.0 => {
                // `dst := old(dst)` is a no-op; drop it (and any prior write).
                if let Some(Src::Mark(old)) = pred[dst] {
                    read_count[old] -= 1;
                }
                pred[dst] = None;
                continue;
            }
            MarkValue::Copy(s) => Src::Mark(local(s.0 as u16)),
        };
        if let Some(Src::Mark(old)) = pred[dst] {
            read_count[old] -= 1;
        }
        if let Src::Mark(s) = new {
            read_count[s] += 1;
        }
        pred[dst] = Some(new);
    }

    let mut ops: Vec<MoveOp> = Vec::with_capacity(cmds.len());
    // A destination is ready to emit once nothing pending still needs its old
    // value (`read_count == 0`).
    let mut ready: Vec<usize> = (0..n)
        .filter(|&d| pred[d].is_some() && read_count[d] == 0)
        .collect();
    let mut remaining = pred.iter().filter(|p| p.is_some()).count();

    while remaining > 0 {
        if let Some(d) = ready.pop() {
            let s = pred[d].take().unwrap();
            remaining -= 1;
            let src_idx = match s {
                Src::Const => curpos,
                Src::Mark(m) => marks[m],
                Src::Scratch => scratch,
            };
            ops.push(MoveOp {
                dst: marks[d],
                src: src_idx,
            });
            if let Src::Mark(m) = s {
                read_count[m] -= 1;
                if read_count[m] == 0 && pred[m].is_some() {
                    ready.push(m);
                }
            }
        } else {
            // No ready assignment but some remain: a copy cycle. Save one of its
            // marks to `scratch` and redirect that mark's readers there; the
            // mark is then free to overwrite and the component drains in order.
            let m = (0..n)
                .find(|&x| pred[x].is_some() && read_count[x] > 0)
                .expect("a cycle node exists when stalled with work remaining");
            ops.push(MoveOp {
                dst: scratch,
                src: marks[m],
            });
            for d in 0..n {
                if matches!(pred[d], Some(Src::Mark(s)) if s == m) {
                    pred[d] = Some(Src::Scratch);
                    read_count[m] -= 1;
                }
            }
            debug_assert_eq!(read_count[m], 0);
            ready.push(m);
        }
    }
    ops.into_boxed_slice()
}

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

/// Whether a `$`-style accept must be checked at every byte (`true`) or only
/// once at end-of-input (`false`). Only multiline `$`
/// (`EndOfLine { multiline: true }`) can fire mid-input, right before a line
/// terminator; non-multiline `$` fires solely at `pos == input.len()`, so it
/// needs no per-byte work — one check in the EOI pass suffices. Lifting it out
/// keeps `has_perbyte_guards` (and thus the capture-free fast path and the JIT)
/// off for the common `…$` / `^…$` family.
fn conditional_needs_perbyte(c: &AnchorConditional) -> bool {
    !matches!(c.cond, EpsCondition::EndOfLine { multiline: false })
}

/// Whether a state's guards require the per-byte guard pass: any `switch`
/// (multiline `^`, `\b`/`\B`) or any `accept` that can fire mid-input
/// (multiline `$`). See [`Tdfa::has_perbyte_guards`].
fn state_guards_need_perbyte(g: &StateGuards) -> bool {
    !g.switches.is_empty() || g.accepts.iter().any(conditional_needs_perbyte)
}

/// Whether any `\b`/`\B` switch widens its word-char test with the icase folds
/// (ſ / Kelvin). This is the regex-global `iu` property, so it is uniform across
/// the automaton; OR-ing is just a robust way to read it off the built guards.
fn guards_word_icase(guards: &[StateGuards]) -> bool {
    guards.iter().any(|g| {
        g.switches.iter().any(|sw| {
            matches!(
                sw.cond,
                EpsCondition::WordBoundary {
                    unicode_icase: true,
                    ..
                }
            )
        })
    })
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

/// Interpreter scan-skip data for a non-accepting state whose self-loop
/// transitions have *empty* move-op lists (no mark updates at all).  When
/// the executor is in such a state it can scan ahead to the first byte that
/// does *not* self-loop with an empty list, advancing the position with zero
/// mark-file overhead — no transition-table lookup, no move-op iteration,
/// no accepting check.  This is the common case for the implicit `.*?`
/// scanning prefix of an unanchored TDFA.
///
/// Two variants (selected at compile time, uniform across all self-loop byte
/// classes of the state):
/// - **Pure skip** (`stamp_marks` is empty): all self-loop moves are empty —
///   fast-scan without any mark update.
/// - **Scan-stamp** (`stamp_marks` is non-empty): all self-loop moves are of
///   the form `mark_j := curpos`.  Fast-scan the run, then stamp each
///   `mark_j` with `pos` (the first non-skip byte offset) — equivalent to
///   the per-byte curpos writes, but in one shot.
///
/// For common patterns the runtime uses a faster scan than the generic per-byte
/// bitmap check; see [`ScanFast`].
#[derive(Debug, Clone)]
pub(crate) struct ScanSkip {
    /// 256-bit bitmap: bit `b` is set iff byte `b` triggers a self-loop on
    /// this state (with either empty or curpos-only move ops).
    pub(crate) byte_bitmap: [u64; 4],
    /// Marks to write with `pos` after the fast scan.  Empty ⇒ pure skip.
    pub(crate) stamp_marks: SmallVec<[u16; 4]>,
    /// Accelerated scan mode derived at compile time from the bitmap.
    pub(crate) fast: ScanFast,
}

/// Accelerated inner scan for a [`ScanSkip`] state.
///
/// When the scan class (set bits in `byte_bitmap`) has structure we can
/// exploit, we skip or simplify the per-byte bitmap lookup:
///
/// - **`Memchr`** (all 0x80–0xFF bits set in bitmap): the only stopping bytes
///   are the 1–3 listed ASCII bytes; use `memchr`/`memchr2`/`memchr3`.
/// - **`AsciiBarrier`** (some 0x80–0xFF bits clear, ≤2 excluded ASCII bytes):
///   stop on `b ≥ 0x80 || b == excl…`.  Auto-vectorises; good for `[^"]`.
/// - **`AsciiRanges`** (all non-ASCII excluded; set decomposes into ≤8 byte ranges):
///   SSE2 saturating-subtract range masks, 16 bytes per iteration.  Falls back
///   to scalar range check on non-x86-64.
/// - **`BitmapAscii`** (all non-ASCII excluded; too many ranges for `AsciiRanges`):
///   pre-store the two ASCII bitmap words; select with a conditional move.
/// - **`Bitmap`**: full 256-bit bitmap; non-ASCII bytes may be self-loop bytes.
pub(crate) const SCAN_MAX_RANGES: usize = 4;

#[derive(Debug, Clone, Copy)]
pub(crate) enum ScanFast {
    Bitmap,
    Memchr { count: u8, bytes: [u8; 3] },
    AsciiBarrier { count: u8, bytes: [u8; 3] },
    /// All non-ASCII excluded; set fits in ≤`SCAN_MAX_RANGES` byte ranges.
    /// `count` ranges packed as (lo, hi) pairs in `pairs[0..2*count]`.
    AsciiRanges { count: u8, pairs: [u8; 2 * SCAN_MAX_RANGES] },
    /// Non-ASCII excluded; `bm0` = `byte_bitmap[0]` (bytes 0x00-0x3F),
    /// `bm1` = `byte_bitmap[1]` (bytes 0x40-0x7F).
    BitmapAscii { bm0: u64, bm1: u64 },
}

/// Interpreter self-loop peel data for a state whose every self-loop
/// transition consists solely of `curpos → mark` stamping ops, with the
/// same destination marks across all self-loop byte classes.  When the
/// executor enters such an accepting state it can scan ahead to the first
/// byte that does *not* self-loop, stamp the marks once with the run end,
/// and record one accept — instead of iterating through each byte.
#[derive(Debug, Clone)]
pub(crate) struct PosStampLoop {
    /// Indices into `src_buf` that should be written with the run-end position.
    pub(crate) stamp_marks: SmallVec<[u16; 4]>,
    /// 256-bit bitmap: bit `b` is set iff byte `b` triggers a self-loop with
    /// only curpos-stamp move ops from this state.
    pub(crate) byte_bitmap: [u64; 4],
    /// Accelerated inner scan variant (same logic as for [`ScanSkip`]).
    pub(crate) fast: ScanFast,
    /// Cached `accept_fallback[state]`: whether `record_accept` at the
    /// PosStampLoop exit needs a best-snap snapshot. Cached here to avoid the
    /// double-indirect load on the hot PosStampLoop exit path.
    pub(crate) needs_snapshot: bool,
}

#[derive(Debug, Clone)]
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
    /// Whether the pattern has user capture groups (beyond the full match). When
    /// false, the executor's accept path skips the per-byte mark snapshot — the
    /// match is just `[start, end]` — which is a big win for accept-heavy
    /// capture-free patterns like `.*`.
    has_captures: bool,
    // Number of user-visible capture groups (not counting the full match,
    // not counting sentinel tags). Equals (nfa.num_capture_tags - 2) / 2.
    // Used to size norm_buf in Scratch::new.
    num_capture_groups: usize,
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

    // Per-state "fallback" flag. Indexed by state ID. True for an accepting
    // state that has a transition to a non-dead, non-accepting state — i.e. the
    // automaton can accept here, read further, clobber registers, then fail and
    // need to rewind. Only such accepts need the eager mark snapshot; for the
    // rest (e.g. `.*`, whose accept self-loops to an accepting state) the
    // executor records the accept cheaply and reads the registers at scan end.
    accept_fallback: Box<[bool]>,

    // Tag commands to apply when a transition fires. Same shape as
    // `transitions` (indexed by `state * num_classes + class`). Each entry
    // is a list of `TagCommand`s — CurrentPos writes first, then Copy writes
    // from canonicalization. May be empty when a transition has no tag effect.
    // Retained for display/debug and the scalar fallback; the executor's hot
    // loop applies `transition_moves`.
    transition_commands: Box<[TagCommandList]>,

    // Precompiled in-place move sequence per transition, same shape/indexing as
    // `transition_commands`. Built by `compile_moves_all` at the end of
    // `try_from` and rebuilt after `optimize` (which changes `num_marks` and the
    // command lists). The executor's per-byte hot loop applies these in order,
    // in place, instead of interpreting `TagCommand`s. An empty entry has no tag
    // effect (skip). See [`MoveOp`].
    transition_moves: Box<[Box<[MoveOp]>]>,

    // Per-state finalization commands. Indexed by state ID. For accepting
    // states this is `num_tags` commands (one per tag) describing how to read
    // the final capture positions out of the mark file. For non-accepting
    // states it's empty. Run once at scan end against the last-accepted
    // state's mark snapshot.
    finals: Box<[SmallVec<[FinalCommand; 4]>]>,

    // Per-state zero-width guards: the unified table for `^ $ \b \B`, indexed by
    // state ID. Each [`StateGuards`] holds the state's `switches` (multiline `^`,
    // `\b`/`\B` — change state and keep matching) and `accepts` (`$` — record a
    // match candidate without changing state). The executor decodes each guard's
    // `cond` from the position's `boundary_signature` (see `anchors.rs`).
    guards: Box<[StateGuards]>,

    // Whether `\b`/`^`/`$` word-char tests should widen with the icase folds
    // (ſ / Kelvin) — the regex-global `iu` property. Lets the executor compute a
    // position's `boundary_signature` once with the correct word bits.
    word_icase: bool,

    // Whether any state carries a guard that must be evaluated *per byte*: any
    // `switch` (multiline `^`, `\b`/`\B`) or any `accept` whose predicate can
    // fire mid-input (multiline `$`). Non-multiline `$` accepts fire only at EOI
    // and do NOT set this, so the capture-free fast path and the JIT stay
    // available for the common `…$` / `^…$` family. Drives the executor's
    // monomorphization and the fast-path / JIT gates. Recomputed after `optimize`.
    has_perbyte_guards: bool,

    // Whether any state carries any `$`-style accept guard at all (multiline or
    // not). Drives the once-per-run EOI accept pass and warm-start gating.
    // Recomputed after `optimize`.
    has_eoi_accepts: bool,

    // Whether the `FULL_MATCH_START` mark is fixed at entry — i.e. no transition
    // ever writes the mark(s) that accepting states read back as
    // `FULL_MATCH_START`. True for anchored/prefilter builds (the start is the
    // run's `start` offset); false for the unanchored `.*?`-prefixed scan, whose
    // handoff transition stamps the start mid-loop. Lets the capture-free hot
    // loop skip per-byte mark application entirely (the entry value survives, so
    // `snapshot_match_start` still reads the right start). Recomputed after
    // `optimize`.
    start_fixed: bool,

    // Premultiplied + accept-flagged transition table for the capture-free fast
    // loop. Same shape/indexing as `transitions` (`state * num_classes + class`),
    // but each entry holds `target * num_classes` (so the loop indexes with a bare
    // add, no per-byte multiply) with `EXEC_ACCEPT_FLAG` set when `target` is
    // accepting (so the accept check is a register bit-test, no `accepting[]`
    // load). `TDFA_DEAD_STATE` stays 0. Built only when the fast loop can run
    // (`start_fixed`, no captures/conditionals/anchor-alts); empty otherwise.
    exec_transitions: Box<[u32]>,

    // Entry commands pre-compiled to `MoveOp` sequences so the executor can
    // apply them with the same tight loop used for per-transition moves,
    // avoiding the `apply_cmds_scalar` overhead (SmallVec + two-pass scan).
    // Parallel structure to `entry_commands_anchored/unanchored`.
    entry_moves_anchored: Box<[MoveOp]>,
    entry_moves_unanchored: Box<[MoveOp]>,

    /// Per-state position-stamp self-loop info.  Indexed by state ID.
    /// `Some` only for accepting states whose every self-loop transition is a
    /// pure curpos-stamp (all `MoveOp::src == curpos_lane`) with consistent
    /// targets.  Empty slice when `transition_moves` was not compiled.
    pos_stamp_loops: Box<[Option<PosStampLoop>]>,

    /// Per-state scan-skip info.  Indexed by state ID.  `Some` only for
    /// non-accepting states that have at least one self-loop transition with
    /// empty move ops — the executor can bypass those bytes entirely.
    scan_skips: Box<[Option<ScanSkip>]>,
}

#[derive(Debug, Clone)]
pub struct AnchorAlt {
    pub cond: EpsCondition,
    pub alt: TdfaStateId,
    pub commands: TagCommandList,
}

/// The zero-width guards on one TDFA state — the unified replacement for the old
/// parallel `anchor_alts` / `anchor_conditionals` tables. At a guarded position
/// the executor computes the `boundary_signature` once, follows matching
/// `switch` entries to a fixpoint (changing state), then records every matching
/// `accept`.
#[derive(Debug, Clone, Default)]
pub struct StateGuards {
    /// Multiline `^` and `\b`/`\B`: when the predicate holds, rearrange marks
    /// (`commands`) and switch the live state to `alt`, continuing to match.
    /// Priority order — the first whose predicate holds wins.
    pub switches: SmallVec<[AnchorAlt; 1]>,
    /// `$`: when the predicate holds, record a match candidate via `finals`
    /// without changing state. All matching accepts are considered.
    pub accepts: SmallVec<[AnchorConditional; 1]>,
}

impl StateGuards {
    fn is_empty(&self) -> bool {
        self.switches.is_empty() && self.accepts.is_empty()
    }
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

/// Set bit `i` in a `u64` bitset (mark-id indexed). Local twin of the `opt`
/// module's helper of the same name (private there).
#[inline]
fn bs_set(bits: &mut [u64], i: u32) {
    bits[(i >> 6) as usize] |= 1u64 << (i & 63);
}

/// Largest mark count for which the precise [`compute_accept_fallback`] dataflow
/// runs. Above it we keep the conservative structural flag (always sound — it
/// only over-snapshots) to bound the size-proportional fixpoint. Mirrors the
/// register allocator's `MAX_RA_MARKS`; such automata are rare and already on the
/// scalar command fallback.
const MAX_FALLBACK_MARKS: usize = 1 << 14;

/// Structural over-approximation of [`compute_accept_fallback`]: flag an
/// accepting state whenever *any* transition leaves it to a non-dead,
/// non-accepting state. Always sound (it only ever over-snapshots); used as the
/// fallback when the precise analysis is over budget or there are no marks.
fn accept_fallback_structural(
    accepting: &[bool],
    transitions: &[TdfaStateId],
    num_classes: usize,
) -> Box<[bool]> {
    let mut out = vec![false; accepting.len()].into_boxed_slice();
    for (s, &acc) in accepting.iter().enumerate() {
        if !acc {
            continue;
        }
        let row = &transitions[s * num_classes..(s + 1) * num_classes];
        out[s] = row
            .iter()
            .any(|&t| t != TDFA_DEAD_STATE && !accepting[t as usize]);
    }
    out
}

/// Per-state fallback flag: an accepting state `S` needs the eager mark snapshot
/// only when some register the accept reads (a `Copy` source in `finals[S]`) can
/// be overwritten on a continuation from `S` that passes through non-accepting
/// states before the run ends or reaches another accept. If no such write is
/// possible, the winner's registers survive untouched in the live mark file and
/// the executor reads them at scan end (the cheap `read_live` path) — no
/// snapshot. See `tdfa_backend::run_anchored` / `record_accept`.
///
/// This refines the older purely-structural check (any live non-accepting
/// successor), which flagged states whose continuation writes only *other*
/// registers (e.g. `(\w+)(\s+\w+)?`, where the trailing group's transitions never
/// touch group 1's registers). We compute, per state, the set of registers
/// writable before the next accept:
///
/// ```text
/// RW(s) = ⋃ over edges s→t with t ≠ DEAD and t non-accepting:
///            written(s→t) ∪ RW(t)
/// ```
///
/// where `written(s→t)` is the edge command list's `dst` marks. Edges into
/// accepting targets contribute nothing: reaching that accept makes it the winner
/// (last accept wins), so `R_S` no longer matters. `S` is a fallback iff
/// `RW(S) ∩ R_S ≠ ∅`.
///
/// Soundness: any runtime path from `S` either dead-ends / hits end-of-input at a
/// non-accepting state — every write along it is in `RW(S)`, including the
/// stranding edge, since end-of-input can strand at *any* non-accepting state —
/// or reaches a later accept that supersedes `S` and is analyzed independently.
/// So `RW(S) ∩ R_S = ∅` guarantees the accept's registers still hold their
/// accept-time values at scan end.
fn compute_accept_fallback(
    accepting: &[bool],
    transitions: &[TdfaStateId],
    transition_commands: &[TagCommandList],
    finals: &[SmallVec<[FinalCommand; 4]>],
    num_classes: usize,
    num_marks: usize,
) -> Box<[bool]> {
    let n = accepting.len();
    let k = num_classes;
    // No marks → nothing to clobber; a huge mark file keeps the conservative
    // structural flag to bound the fixpoint (opt.rs's `register_allocate` caps
    // itself the same way).
    if num_marks == 0 || num_marks > MAX_FALLBACK_MARKS {
        return accept_fallback_structural(accepting, transitions, num_classes);
    }
    let words = num_marks.div_ceil(64);

    // `rw[s]` seeded with the marks written by edges leaving `s` to a non-dead,
    // non-accepting target; `preds` collects those same edges' sources for the
    // backward worklist that unions successors' `rw` in.
    let mut rw = vec![0u64; n * words];
    let mut preds: Vec<Vec<u32>> = vec![Vec::new(); n];
    for s in 0..n {
        for c in 0..k {
            let t = transitions[s * k + c];
            if t == TDFA_DEAD_STATE || accepting[t as usize] {
                continue;
            }
            for cmd in &transition_commands[s * k + c] {
                bs_set(&mut rw[s * words..(s + 1) * words], cmd.dst.0);
            }
            preds[t as usize].push(s as u32);
        }
    }

    // Worklist fixpoint: `rw[s] |= rw[t]` for every edge `s→t` to a non-accepting
    // `t`; when `rw[s]` grows, re-enqueue its predecessors. `acc` is reused.
    let mut in_wl = vec![true; n];
    let mut wl: std::collections::VecDeque<u32> = (0..n as u32).collect();
    let mut acc = vec![0u64; words];
    while let Some(s) = wl.pop_front() {
        let s = s as usize;
        in_wl[s] = false;
        acc.copy_from_slice(&rw[s * words..(s + 1) * words]);
        for c in 0..k {
            let t = transitions[s * k + c];
            if t == TDFA_DEAD_STATE || accepting[t as usize] {
                continue;
            }
            let t = t as usize;
            for w in 0..words {
                acc[w] |= rw[t * words + w];
            }
        }
        if acc[..] != rw[s * words..(s + 1) * words] {
            rw[s * words..(s + 1) * words].copy_from_slice(&acc);
            for &p in &preds[s] {
                if !in_wl[p as usize] {
                    in_wl[p as usize] = true;
                    wl.push_back(p);
                }
            }
        }
    }

    // An accepting state is a fallback iff a register it reads can be clobbered.
    let mut out = vec![false; n].into_boxed_slice();
    let mut reads = vec![0u64; words];
    for s in 0..n {
        if !accepting[s] {
            continue;
        }
        reads.iter_mut().for_each(|w| *w = 0);
        for fc in &finals[s] {
            if let MarkValue::Copy(mk) = fc.src {
                bs_set(&mut reads, mk.0);
            }
        }
        let rw_s = &rw[s * words..(s + 1) * words];
        out[s] = reads.iter().zip(rw_s).any(|(&r, &w)| r & w != 0);
    }
    out
}

/// Classify a 256-bit self-loop `byte_bitmap` into the fastest available
/// [`ScanFast`] variant.  Called by both `compute_scan_skips` and
/// `compute_pos_stamp_loops` so the logic stays in one place.
fn classify_scan_fast(byte_bitmap: &[u64; 4]) -> ScanFast {
    let mut ascii_excl_bytes = [0u8; 3];
    let mut ascii_excl_count = 0u8;
    let mut ascii_excl_overflow = false;
    let mut has_nonascii_excl = false;
    let mut all_nonascii_excl = true;
    for b in 0u8..=255u8 {
        if (byte_bitmap[b as usize >> 6] >> (b as usize & 63)) & 1 == 0 {
            if b < 0x80 {
                if ascii_excl_count < 3 {
                    ascii_excl_bytes[ascii_excl_count as usize] = b;
                    ascii_excl_count += 1;
                } else {
                    ascii_excl_overflow = true;
                }
            } else {
                has_nonascii_excl = true;
            }
        } else if b >= 0x80 {
            all_nonascii_excl = false;
        }
    }
    if !has_nonascii_excl && !ascii_excl_overflow {
        ScanFast::Memchr { count: ascii_excl_count, bytes: ascii_excl_bytes }
    } else if has_nonascii_excl && ascii_excl_count <= 2 && !ascii_excl_overflow {
        ScanFast::AsciiBarrier { count: ascii_excl_count, bytes: ascii_excl_bytes }
    } else if all_nonascii_excl {
        if let Some((count, pairs)) = bitmap_to_ascii_ranges(byte_bitmap[0], byte_bitmap[1]) {
            ScanFast::AsciiRanges { count, pairs }
        } else {
            ScanFast::BitmapAscii { bm0: byte_bitmap[0], bm1: byte_bitmap[1] }
        }
    } else {
        ScanFast::Bitmap
    }
}

/// Convert the ASCII half of a bitmap (bm0 for 0x00-0x3F, bm1 for 0x40-0x7F)
/// into a compact list of (lo, hi) byte ranges.  Returns `None` if the set
/// requires more than `SCAN_MAX_RANGES` ranges.
fn bitmap_to_ascii_ranges(bm0: u64, bm1: u64) -> Option<(u8, [u8; 2 * SCAN_MAX_RANGES])> {
    let mut pairs = [0u8; 2 * SCAN_MAX_RANGES];
    let mut count = 0usize;
    let mut in_range = false;
    let mut range_start = 0u8;

    for b in 0u8..=0x7F {
        let word = if b < 0x40 { bm0 } else { bm1 };
        let in_set = (word >> (b as usize & 63)) & 1 != 0;
        if in_set && !in_range {
            range_start = b;
            in_range = true;
        } else if !in_set && in_range {
            if count >= SCAN_MAX_RANGES {
                return None;
            }
            pairs[2 * count] = range_start;
            pairs[2 * count + 1] = b - 1;
            count += 1;
            in_range = false;
        }
    }
    if in_range {
        if count >= SCAN_MAX_RANGES {
            return None;
        }
        pairs[2 * count] = range_start;
        pairs[2 * count + 1] = 0x7F;
        count += 1;
    }

    Some((count as u8, pairs))
}

impl Tdfa {
    /// Build the TDFA. The result is correct but **unoptimized** — every
    /// `CurrentPos` write keeps its own freshly-minted `InputMark` and no
    /// states are merged. Call [`Tdfa::optimize`] to apply the optional
    /// optimization passes.
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
        let accept_fallback = compute_accept_fallback(
            &build.accepting,
            &build.transitions,
            &build.transition_commands,
            &build.finals,
            num_classes,
            num_marks,
        );

        // Fuse the two per-state construction lists into the unified guard table
        // (switches = alts, accepts = conditionals), one entry per state.
        let guards: Box<[StateGuards]> = build
            .anchor_alts
            .into_iter()
            .zip(build.anchor_conditionals)
            .map(|(switches, accepts)| StateGuards { switches, accepts })
            .collect();
        let word_icase = guards_word_icase(&guards);
        let has_perbyte_guards = guards.iter().any(state_guards_need_perbyte);
        let has_eoi_accepts = guards.iter().any(|g| !g.accepts.is_empty());

        let mut tdfa = Tdfa {
            start_anchored,
            start_unanchored,
            entry_commands_anchored,
            entry_commands_unanchored,
            num_classes,
            num_tags,
            has_captures: nfa.num_capture_tags() > 2,
            num_capture_groups: (nfa.num_capture_tags().saturating_sub(2)) / 2,
            num_marks,
            group_names: nfa.group_names().to_vec().into_boxed_slice(),
            byte_to_class,
            transitions: build.transitions.into_boxed_slice(),
            accepting: build.accepting.into_boxed_slice(),
            accept_fallback,
            transition_commands: build.transition_commands.into_boxed_slice(),
            transition_moves: Box::default(),
            finals: build.finals.into_boxed_slice(),
            guards,
            word_icase,
            has_perbyte_guards,
            has_eoi_accepts,
            start_fixed: false,
            exec_transitions: Box::default(),
            entry_moves_anchored: Box::default(),
            entry_moves_unanchored: Box::default(),
            pos_stamp_loops: Box::default(),
            scan_skips: Box::default(),
        };
        tdfa.compile_moves_all();
        Ok(tdfa)
    }

    /// Recompile `transition_moves` from `transition_commands` and the current
    /// `num_marks`. Run at the end of `try_from` and again after `optimize`,
    /// which renumbers marks and rewrites the command lists.
    ///
    /// Each command list compiles (via [`compile_moves`]) to an ordered in-place
    /// move sequence; an empty command list compiles to an empty sequence (skip).
    /// Skipped entirely (leaving an empty table → the executor's scalar command
    /// fallback) only for the degenerate case of a mark file too large to index
    /// with the `u16` lane type — far beyond any realistic capture count.
    fn compile_moves_all(&mut self) {
        self.start_fixed = self.compute_start_fixed();
        self.build_exec_transitions();
        let num_marks = self.num_marks;
        if num_marks + 3 > u16::MAX as usize {
            self.transition_moves = Box::default();
            self.entry_moves_anchored = Box::default();
            self.entry_moves_unanchored = Box::default();
            self.pos_stamp_loops = Box::default();
            self.scan_skips = Box::default();
            return;
        }
        self.transition_moves = self
            .transition_commands
            .iter()
            .map(|cmds| compile_moves(cmds, num_marks))
            .collect();
        self.entry_moves_anchored =
            compile_moves(&self.entry_commands_anchored, num_marks);
        self.entry_moves_unanchored =
            compile_moves(&self.entry_commands_unanchored, num_marks);
        self.pos_stamp_loops = self.compute_pos_stamp_loops();
        self.scan_skips = self.compute_scan_skips();
    }

    /// Compute per-state position-stamp self-loop info from the compiled
    /// `transition_moves`.  A state qualifies when it is accepting and every
    /// byte that self-loops from it has only `curpos → mark` move ops with
    /// consistent target marks across all such byte classes.
    fn compute_pos_stamp_loops(&self) -> Box<[Option<PosStampLoop>]> {
        let curpos_lane = (self.num_marks + 1) as u16;
        let num_states = self.accepting.len();
        let num_classes = self.num_classes;

        (0..num_states)
            .map(|state| {
                if !self.accepting[state] {
                    return None;
                }
                let mut stamp_marks: SmallVec<[u16; 4]> = SmallVec::new();
                let mut byte_bitmap = [0u64; 4];
                let mut initialized = false;

                for b in 0u8..=255u8 {
                    let class = self.byte_to_class[b as usize] as usize;
                    let idx = state * num_classes + class;
                    if self.transitions[idx] as usize != state {
                        continue; // not a self-loop
                    }
                    let moves = &self.transition_moves[idx];
                    if moves.is_empty() {
                        continue; // no stamp ops
                    }
                    if moves.iter().any(|op| op.src != curpos_lane) {
                        continue; // not all curpos-stamps
                    }
                    let class_marks: SmallVec<[u16; 4]> =
                        moves.iter().map(|op| op.dst).collect();
                    if !initialized {
                        stamp_marks = class_marks;
                        initialized = true;
                    } else if stamp_marks != class_marks {
                        return None; // inconsistent targets across classes
                    }
                    byte_bitmap[b as usize >> 6] |= 1u64 << (b as usize & 63);
                }

                if !initialized {
                    return None;
                }
                let fast = classify_scan_fast(&byte_bitmap);
                let needs_snapshot = self.accept_fallback[state];
                Some(PosStampLoop { stamp_marks, byte_bitmap, fast, needs_snapshot })
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    /// Compute per-state scan-skip info from `transition_moves`.  A state
    /// qualifies when it is non-accepting and every self-loop byte class has
    /// move ops of a *uniform* kind across the whole state:
    /// - all empty (pure skip), OR
    /// - all curpos-stamps with the same destination marks (scan-stamp).
    /// A mix of the two, or any non-curpos move, disqualifies the state.
    fn compute_scan_skips(&self) -> Box<[Option<ScanSkip>]> {
        let curpos_lane = (self.num_marks + 1) as u16;
        let num_states = self.accepting.len();
        let num_classes = self.num_classes;

        (0..num_states)
            .map(|state| {
                if self.accepting[state] {
                    return None; // only for non-accepting states
                }
                let mut byte_bitmap = [0u64; 4];
                let mut any = false;
                // Tracks the expected stamps for every self-loop byte class.
                // None = not yet seen any self-loop class.
                let mut stamp_marks: Option<SmallVec<[u16; 4]>> = None;

                for b in 0u8..=255u8 {
                    let class = self.byte_to_class[b as usize] as usize;
                    let idx = state * num_classes + class;
                    if self.transitions[idx] as usize != state {
                        continue; // not a self-loop
                    }
                    let moves = &self.transition_moves[idx];
                    let class_stamps: SmallVec<[u16; 4]> = if moves.is_empty() {
                        SmallVec::new() // pure skip
                    } else {
                        // All ops must be curpos → mark; any other src disqualifies.
                        if moves.iter().any(|op| op.src != curpos_lane) {
                            return None;
                        }
                        moves.iter().map(|op| op.dst).collect()
                    };
                    // Require uniform stamps across all self-loop byte classes.
                    match &stamp_marks {
                        None => stamp_marks = Some(class_stamps),
                        Some(sm) if *sm == class_stamps => {}
                        Some(_) => return None,
                    }
                    byte_bitmap[b as usize >> 6] |= 1u64 << (b as usize & 63);
                    any = true;
                }

                if !any {
                    return None;
                }
                let fast = classify_scan_fast(&byte_bitmap);
                Some(ScanSkip {
                    byte_bitmap,
                    stamp_marks: stamp_marks.unwrap_or_default(),
                    fast,
                })
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    /// Build the capture-free fast-loop transition table (see
    /// [`exec_transitions`](Self::exec_transitions)). Only worthwhile when the
    /// fast loop can actually run, so it's gated on the same conditions the
    /// executor's dispatcher checks; otherwise the table is left empty.
    fn build_exec_transitions(&mut self) {
        let fast_ok = !self.has_captures && self.start_fixed && !self.has_perbyte_guards;
        if !fast_ok {
            self.exec_transitions = Box::default();
            return;
        }
        let nc = self.num_classes as u32;
        let accepting = &self.accepting;
        self.exec_transitions = self
            .transitions
            .iter()
            .map(|&t| {
                if t == TDFA_DEAD_STATE {
                    0
                } else {
                    let premult = t * nc;
                    if accepting[t as usize] {
                        premult | EXEC_ACCEPT_FLAG
                    } else {
                        premult
                    }
                }
            })
            .collect();
    }

    /// The capture-free fast-loop transition table, or empty when the fast loop
    /// is not applicable to this automaton.
    pub(crate) fn exec_transitions(&self) -> &[u32] {
        &self.exec_transitions
    }

    /// Whether `FULL_MATCH_START` is written only by the entry commands (never by
    /// a transition). Collect every mark an accepting state reads back as
    /// `FULL_MATCH_START`, then check no transition command writes one of them.
    /// Conservatively `false` if no accepting state names a start mark.
    fn compute_start_fixed(&self) -> bool {
        let mut start_marks: SmallVec<[u32; 4]> = SmallVec::new();
        // Plain accepting-state finals, plus the `$`-conditional accept finals
        // (`Holmes$`) — a `$` accept reads the start mark back through its own
        // finals, not the state's plain finals, so both must be scanned for an
        // anchored `…$` pattern to be recognized as start-fixed.
        let cond_finals = self.guards.iter().flat_map(|g| g.accepts.iter().map(|ac| &ac.finals));
        for finals in self.finals.iter().map(|f| f.as_slice()).chain(cond_finals.map(|f| f.as_slice())) {
            for cmd in finals {
                if cmd.tag == FULL_MATCH_START {
                    if let MarkValue::Copy(m) = cmd.src {
                        if !start_marks.contains(&m.0) {
                            start_marks.push(m.0);
                        }
                    }
                }
            }
        }
        if start_marks.is_empty() {
            return false;
        }
        !self
            .transition_commands
            .iter()
            .flat_map(|cmds| cmds.iter())
            .any(|cmd| start_marks.contains(&cmd.dst.0))
    }

    /// Whether the match start is fixed at the run's `start` offset (no
    /// transition writes the `FULL_MATCH_START` mark). See the field docs.
    pub(crate) fn start_fixed(&self) -> bool {
        self.start_fixed
    }

    /// Whether `transition_moves` was compiled (effectively always) or skipped
    /// for a mark file too large to index with `u16` (the executor then falls
    /// back to interpreting [`transition_commands`](Self::transition_commands)).
    pub fn has_moves(&self) -> bool {
        !self.transition_moves.is_empty()
    }

    /// Per-state position-stamp self-loop table.  Indexed by state ID.
    /// Returns an empty slice when `transition_moves` was not compiled.
    pub(crate) fn pos_stamp_loops(&self) -> &[Option<PosStampLoop>] {
        &self.pos_stamp_loops
    }

    /// Per-state scan-skip table.  Indexed by state ID.
    /// Returns an empty slice when `transition_moves` was not compiled.
    pub(crate) fn scan_skips(&self) -> &[Option<ScanSkip>] {
        &self.scan_skips
    }

    /// Entry commands pre-compiled to [`MoveOp`] sequences. Returns the
    /// anchored list when `start == 0` and the unanchored list otherwise.
    /// Empty when `transition_moves` was not compiled (rare: mark file too
    /// large) or when there are no entry commands for this start mode.
    pub(crate) fn entry_moves(&self, start: usize) -> &[MoveOp] {
        if start == 0 {
            &self.entry_moves_anchored
        } else {
            &self.entry_moves_unanchored
        }
    }

    /// Apply the optional optimization passes (state minimization + register
    /// cleanup) in place. Skippable — a freshly `try_from`'d automaton matches
    /// correctly without it; this only shrinks the automaton.
    pub fn optimize(&mut self) {
        opt::optimize(self);
        // State minimization can remove guard-bearing states, so refresh the
        // precomputed flags the executor's dispatcher reads.
        self.has_perbyte_guards = self.guards.iter().any(state_guards_need_perbyte);
        self.has_eoi_accepts = self.guards.iter().any(|g| !g.accepts.is_empty());
        self.word_icase = guards_word_icase(&self.guards);
        // `optimize` renumbers marks and rewrites the command lists, so the
        // precompiled move sequences must be rebuilt from the new state. The
        // capture-free fast table built there also depends on the refreshed
        // dispatcher flags above.
        self.compile_moves_all();
        self.accept_fallback = compute_accept_fallback(
            &self.accepting,
            &self.transitions,
            &self.transition_commands,
            &self.finals,
            self.num_classes,
            self.num_marks,
        );
    }

    /// The zero-width guards for `state` (switches + accepts). Empty for most
    /// states.
    pub(crate) fn guards(&self, state: TdfaStateId) -> &StateGuards {
        &self.guards[state as usize]
    }

    /// Whether `\b`/`^`/`$` word tests widen with the icase folds (regex-global
    /// `iu`). Passed to `boundary_signature` so word bits are computed correctly.
    pub(crate) fn word_icase(&self) -> bool {
        self.word_icase
    }

    /// Whether any state carries a guard that must be evaluated per byte (any
    /// switch, or a multiline-`$` accept). Drives the executor's choice of
    /// monomorphization (see `TdfaExecConfig`), the capture-free fast-path gate,
    /// and JIT eligibility. A pure non-multiline `$` does not set this — see
    /// [`has_eoi_accepts`](Self::has_eoi_accepts).
    pub(crate) fn has_perbyte_guards(&self) -> bool {
        self.has_perbyte_guards
    }

    /// Whether any state carries any `$`-style accept (multiline or not). Drives
    /// the once-per-run EOI accept pass and warm-start gating.
    pub(crate) fn has_eoi_accepts(&self) -> bool {
        self.has_eoi_accepts
    }

    /// Whether the pattern has user capture groups (beyond the full match).
    /// When false the executor skips the per-byte accept snapshot.
    pub(crate) fn has_captures(&self) -> bool {
        self.has_captures
    }

    /// Per-state fallback flags (see the `accept_fallback` field): an accepting
    /// state needs the eager snapshot only when its entry here is true.
    pub(crate) fn accept_fallback(&self) -> &[bool] {
        &self.accept_fallback
    }

    pub fn num_tags(&self) -> usize {
        self.num_tags
    }

    /// Number of user-visible capture groups (not counting the full match,
    /// not counting sentinel tags). Use this to size `norm_buf` in `Scratch`.
    pub fn num_capture_groups(&self) -> usize {
        self.num_capture_groups
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
        for g in self.guards.iter() {
            for sw in &g.switches {
                tally(&sw.commands);
            }
            for ac in &g.accepts {
                tally(&ac.commands);
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

    /// Precompiled in-place move sequences for each transition, same
    /// shape/indexing as [`transition_commands`](Self::transition_commands). The
    /// executor applies these in its hot loop. See [`MoveOp`].
    pub(crate) fn transition_moves(&self) -> &[Box<[MoveOp]>] {
        &self.transition_moves
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
