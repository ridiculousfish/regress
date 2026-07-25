//! Tagged DFA — priority-ordered subset construction over a TNFA.
//!
//! Phase A (this milestone): epsilon writes mint fresh `InputMark`s during
//! determinization, and commands ride on incoming TDFA transitions to update
//! a runtime `marks` array. On accept, per-state `finals` copy canonical
//! marks into the runtime register slots used by `NfaMatch`.

mod opt;

#[cfg(feature = "tdfa-jit")]
pub mod jit;

#[cfg(any(feature = "tdfa-jit", feature = "codegen"))]
pub(crate) mod plan;

#[cfg(feature = "codegen")]
pub(crate) mod rustgen;

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

/// Bit 0 of `trans_flags[idx]`: the transition's target state is accepting.
pub(crate) const TF_ACCEPT: u8 = 1;
/// Bit 1 of `trans_flags[idx]`: the target accepting state has `accept_fallback`
/// (an eager mark snapshot is needed on acceptance).
pub(crate) const TF_FALLBACK: u8 = 2;
/// Bit 2 of `trans_flags[idx]`: the target state carries switch guards
/// (multiline `^`, `\b`/`\B`). Read by the per-byte-guards loop so the common
/// no-boundary byte never touches the guard tables.
pub(crate) const TF_SWITCHES: u8 = 4;
/// Bit 3 of `trans_flags[idx]`: the target state carries `$`-style accept
/// guards. Like `TF_SWITCHES`, only meaningful while the live state is still
/// the transition target (a fired switch invalidates both bits).
pub(crate) const TF_ACCEPTS: u8 = 8;

/// `guard_index` sentinel: the state has no zero-width guards.
const GUARD_NONE: u32 = u32::MAX;

/// `AnchorConditional::prune` sentinel: no leftmost-cut successor — either the
/// accept can only fire at end of input (nothing left to prune) or the cut
/// would keep the state unchanged.
pub(crate) const NO_PRUNE: TdfaStateId = u32::MAX;

/// Maximum number of TDFA states before we bail out. Matches
/// `dfa::DFA_STATE_BUDGET`.
pub(crate) const TDFA_STATE_BUDGET: usize = 65536;

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

/// Scratch instrumentation for estimating TDFA construction-time memory: how
/// many live NFA threads accumulate per registered TDFA state as construction
/// progresses. Opt-in via `REGRESS_TDFA_MEM_TRACE=<n>` (report every `n`
/// newly-registered states to stderr); zero cost when unset. Temporary —
/// not part of the public API, delete once the quadratic-peak-memory
/// investigation (see automata/CLAUDE.md) is resolved.
#[cfg(feature = "std")]
struct MemTrace {
    interval: usize,
    states: usize,
    thread_sum: u64,
    thread_max: usize,
    slot_sum: u64,
    populated_sum: u64,
}

#[cfg(feature = "std")]
impl MemTrace {
    fn init() -> Option<Self> {
        let n: usize = std::env::var("REGRESS_TDFA_MEM_TRACE").ok()?.parse().ok()?;
        (n > 0).then_some(Self {
            interval: n,
            states: 0,
            thread_sum: 0,
            thread_max: 0,
            slot_sum: 0,
            populated_sum: 0,
        })
    }

    fn record(&mut self, cfg: &TdfaState, num_marks: u32, interner: &TagMapStore) {
        let num_threads = cfg.0.len();
        self.states += 1;
        self.thread_sum += num_threads as u64;
        self.thread_max = self.thread_max.max(num_threads);
        for t in &cfg.0 {
            let tag_map = interner.get(t.tag_map);
            self.slot_sum += tag_map.len() as u64;
            self.populated_sum += tag_map.iter().filter(|m| m.is_some()).count() as u64;
        }
        if self.states % self.interval == 0 {
            eprintln!(
                "tdfa_mem_trace states={:7} threads_last={:5} avg_threads={:8.2} max_threads={:6} marks={:8} tag_slots_populated={:6.2}%",
                self.states,
                num_threads,
                self.thread_sum as f64 / self.states as f64,
                self.thread_max,
                num_marks,
                100.0 * self.populated_sum as f64 / self.slot_sum.max(1) as f64,
            );
        }
    }
}

/// Scratch counters: of every `tag_map` handle copy performed during
/// construction, how many are pure duplicates (shared `TagMapId`, no interning
/// needed) versus how many are a genuine write (needs `TagMapStore::intern`)?
/// Same temporary-instrumentation status as [`MemTrace`].
#[cfg(feature = "std")]
static TAGMAP_BYTE_STEP_CLONES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "std")]
static TAGMAP_EPS_CLONES_EMPTY_OPS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "std")]
static TAGMAP_EPS_CLONES_NONEMPTY_OPS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "std")]
fn report_tagmap_clone_counts() {
    use std::sync::atomic::Ordering::Relaxed;
    if std::env::var("REGRESS_TDFA_MEM_TRACE").is_err() {
        return;
    }
    let byte_step = TAGMAP_BYTE_STEP_CLONES.load(Relaxed);
    let eps_empty = TAGMAP_EPS_CLONES_EMPTY_OPS.load(Relaxed);
    let eps_write = TAGMAP_EPS_CLONES_NONEMPTY_OPS.load(Relaxed);
    let shared = byte_step + eps_empty;
    let total = shared + eps_write;
    eprintln!(
        "tdfa_tagmap_clones byte_step={byte_step} eps_empty_ops={eps_empty} eps_write_ops={eps_write} \
         shared={shared} ({:.2}% of {total} total copies needed no interning)",
        100.0 * shared as f64 / total.max(1) as f64,
    );
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

/// Per-tag version map: indexed by the TNFA's global tag/register index,
/// `None` where the tag hasn't been written on the path reaching this thread.
type TagMap = SmallVec<[Option<InputMark>; 4]>;

/// Interned handle to a [`TagMap`] value. Threads that carry the same tag
/// values — the overwhelming majority; a byte-consuming transition never
/// writes a tag, and most eps edges don't either — share the same id instead
/// of each holding their own copy.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
struct TagMapId(u32);

/// Interning arena for every distinct `TagMap` value minted during one
/// `Tdfa::try_from_with_budget` call.
///
/// Equal `TagMap`s must always get the same `TagMapId` — this is load-bearing,
/// not just a memory optimization. `TdfaState`'s `Eq`/`Hash` need to recognize
/// value-identical configurations regardless of how they were built (e.g. a
/// loop's steady-state iteration re-deriving the same canonical tag pattern
/// via a different construction path each time around); comparing raw ids
/// instead of values would only be a valid shortcut if equal values always
/// shared an id, which is exactly what `intern` guarantees. Without it,
/// determinization could fail to recognize a repeated state and never
/// converge. See automata/CLAUDE.md.
struct TagMapStore {
    /// Every interned `TagMap`'s payload, back to back, `num_tags` slots per
    /// value — *not* one `Vec` entry per value. Every `TagMap` minted during
    /// one build has exactly `num_tags` elements (see [`TagMap`]'s own doc),
    /// so unlike `CsrTable` elsewhere in this file there's no need for a
    /// per-entry `(offset, len)` cell: entry `i`'s slice always starts at
    /// `i * num_tags`. `TagMapId(i)` is that entry index `i`, *not* a byte
    /// offset and *not* an index into anything ragged — `get` below is the
    /// only place that arithmetic should happen.
    ///
    /// This is the payoff over storing `Vec<TagMap>` directly: a `TagMap`
    /// (`SmallVec<[Option<InputMark>; 4]>`) costs 48 bytes per value
    /// regardless of `num_tags` (padded out to the inline capacity, or to
    /// the spilled-pointer layout, whichever the type needs room for); flat
    /// storage costs exactly `num_tags * 8` bytes per value, no headroom,
    /// no per-value allocation.
    arena: Vec<Option<InputMark>>,
    /// Every `TagMap` minted in this build has this many tags — fixed once
    /// per `Tdfa::try_from_with_budget` call. What makes the flat, fixed-
    /// stride `arena` above possible instead of needing per-entry lengths.
    num_tags: usize,
    /// value -> `TagMapId`, for `intern`'s dedup check. Necessarily holds a
    /// second copy of each unique value (a plain `HashMap` must own its
    /// keys) in its own `SmallVec`-shaped representation, separate from
    /// `arena`'s flat one; at the unique-value counts observed in practice
    /// (thousands, not millions — one entry per genuine write, not per
    /// thread) this is noise next to what interning saves overall.
    index: HashMap<TagMap, TagMapId>,
}

impl TagMapStore {
    fn new(num_tags: usize) -> Self {
        Self {
            arena: Vec::new(),
            num_tags,
            index: HashMap::new(),
        }
    }

    /// Return `value`'s id, minting a fresh one only if this exact value
    /// hasn't been seen before. `value.len()` must equal `num_tags` (true of
    /// every `TagMap` this module ever constructs).
    fn intern(&mut self, value: TagMap) -> TagMapId {
        if let Some(&id) = self.index.get(&value) {
            return id;
        }
        // The new entry's index is "how many entries are already in the flat
        // arena", i.e. `arena.len() / num_tags` — *not* `arena.len()` itself,
        // since arena.len() counts individual `Option<InputMark>` slots, not
        // `TagMap` values.
        debug_assert_eq!(self.arena.len() % self.num_tags.max(1), 0);
        let id = TagMapId((self.arena.len() / self.num_tags.max(1)) as u32);
        self.arena.extend_from_slice(&value);
        self.index.insert(value, id);
        id
    }

    /// The `num_tags`-long slice of marks `id` was interned with.
    fn get(&self, id: TagMapId) -> &[Option<InputMark>] {
        let start = id.0 as usize * self.num_tags;
        &self.arena[start..start + self.num_tags]
    }
}

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
// `repr(C)` (declaration-order layout, no padding for two u16 fields) is load-
// bearing for the AoT table tier's `le_moveops` cast, which reinterprets a raw
// little-endian byte blob as `&[MoveOp]` — every `(u16, u16)` bit pattern is a
// valid `MoveOp`, so the cast is sound given the guaranteed layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct MoveOp {
    pub dst: u16,
    pub src: u16,
}

/// CSR-style table: one shared element arena plus an `(offset, len)` cell per
/// index. The flat layout is what lets the executor's tables be borrowed as
/// plain slices (see `TdfaView`) and, for the AoT table tier, emitted as
/// `static` data; interning by the builder lets identical sequences share one
/// arena range. Empty `cells` ⇔ the table was not built.
#[derive(Debug, Clone)]
pub(crate) struct CsrTable<T> {
    /// Flat `(offset, len)` pairs at `2i` / `2i + 1` — flat `u32`s rather
    /// than tuples so the table tier can reinterpret a byte blob soundly.
    cells: Box<[u32]>,
    arena: Box<[T]>,
}

// Manual impl: the derive would demand `T: Default`, which the element types
// don't (and needn't) satisfy — an empty table stores no elements.
impl<T> Default for CsrTable<T> {
    fn default() -> Self {
        CsrTable {
            cells: Box::default(),
            arena: Box::default(),
        }
    }
}

/// Compiled per-transition move sequences, same shape/indexing as
/// `transitions`. Identical sequences — common across a state's byte classes,
/// and across states thanks to `compile_moves`' canonical emission order —
/// share a single arena range (see [`Tdfa::has_moves`]).
pub(crate) type MoveTable = CsrTable<MoveOp>;

impl<T> core::ops::Index<usize> for CsrTable<T> {
    type Output = [T];
    #[inline]
    fn index(&self, idx: usize) -> &[T] {
        let (off, len) = (self.cells[2 * idx], self.cells[2 * idx + 1]);
        &self.arena[off as usize..off as usize + len as usize]
    }
}

/// Debug-checked CSR lookup over raw `(cells, arena)` slices — the borrowed
/// form of [`CsrTable::iat`], usable with `static` tables (the AoT table
/// tier) as well as heap ones. `cells` holds flat `(offset, len)` pairs at
/// `2 * idx` / `2 * idx + 1`.
#[inline(always)]
pub(crate) fn csr_iat<'a, T>(cells: &[u32], arena: &'a [T], idx: usize) -> &'a [T] {
    let off = *crate::util::DebugCheckIndex::iat(cells, 2 * idx);
    let len = *crate::util::DebugCheckIndex::iat(cells, 2 * idx + 1);
    let range = off as usize..off as usize + len as usize;
    debug_assert!(arena.get(range.clone()).is_some(), "CSR arena range out of bounds");
    if cfg!(feature = "prohibit-unsafe") {
        &arena[range]
    } else {
        unsafe { arena.get_unchecked(range) }
    }
}

impl<T> CsrTable<T> {
    /// Debug-checked `Index` (the `DebugCheckIndex` convention): the
    /// executor's per-byte path, where the two bounds checks of the safe
    /// `Index` impl are measurable. Offsets/lengths are constructed valid by
    /// the builders.
    #[inline(always)]
    pub(crate) fn iat(&self, idx: usize) -> &[T] {
        csr_iat(&self.cells, &self.arena, idx)
    }

    /// The raw `(cells, arena)` slice pair (for the table-view abstraction).
    pub(crate) fn as_raw(&self) -> (&[u32], &[T]) {
        (&self.cells, &self.arena)
    }

    /// Build from per-index lists, interning identical non-empty lists so they
    /// share one arena range.
    fn from_lists<L: AsRef<[T]>>(lists: impl Iterator<Item = L>) -> Self
    where
        T: Clone + Eq + core::hash::Hash,
    {
        let mut intern: HashMap<Box<[T]>, (u32, u32)> = HashMap::new();
        let mut arena: Vec<T> = Vec::new();
        let mut cells: Vec<u32> = Vec::new();
        for list in lists {
            let list = list.as_ref();
            let cell = if list.is_empty() {
                (0, 0)
            } else if let Some(&c) = intern.get(list) {
                c
            } else {
                let off = u32::try_from(arena.len()).expect("CSR arena exceeds u32 range");
                arena.extend_from_slice(list);
                let c = (off, list.len() as u32);
                intern.insert(list.into(), c);
                c
            };
            cells.push(cell.0);
            cells.push(cell.1);
        }
        CsrTable {
            cells: cells.into_boxed_slice(),
            arena: arena.into_boxed_slice(),
        }
    }
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
///
/// Among the valid linearizations the emitted one is canonical: ties are
/// broken toward the smallest global mark id, yielding the lexicographically
/// least order. Semantically equal parallel assignments therefore compile to
/// identical `MoveOp` sequences regardless of command-list order.
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

    // Emission is canonical: wherever the dependency order leaves a choice
    // (which ready destination to emit, which cycle mark to save), take the
    // smallest *global* mark id. The output is then the lexicographically
    // least valid linearization, so equal parallel assignments compile to
    // byte-identical sequences no matter what order construction enumerated
    // the commands in — command-list order can't leak through.
    while remaining > 0 {
        let pick = ready
            .iter()
            .enumerate()
            .min_by_key(|&(_, &d)| marks[d])
            .map(|(i, _)| i);
        if let Some(i) = pick {
            let d = ready.swap_remove(i);
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
                .filter(|&x| pred[x].is_some() && read_count[x] > 0)
                .min_by_key(|&x| marks[x])
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
    /// Leftmost-cut successor: when this accept fires mid-scan the executor
    /// applies `prune_commands` to the live marks and switches to this state —
    /// the source state minus every thread the accept outranks. Without the
    /// cut, a fired `$` accept leaves the later-start scanner threads alive
    /// and the automaton never reaches the dead state, making `find` scan to
    /// end of input (O(n·matches) iteration — `\w+$`/m). The eager-accept path
    /// gets the same cut at construction time via `truncate_at_first_goal`;
    /// a conditional accept can only take it at runtime, once the predicate
    /// has actually held. `NO_PRUNE` when not applicable (EOI-only accepts,
    /// or the cut keeps the state unchanged).
    pub(crate) prune: TdfaStateId,
    /// Mark moves for the `prune` switch (source-canonical → pruned-canonical
    /// layout), computed like an anchor-alt's switch commands.
    pub(crate) prune_commands: TagCommandList,
    /// Construction-only: number of leading threads of the owning state that
    /// outrank this accept (the closure prefix at the moment the conditional
    /// was recorded — ending with the accepting thread itself). Consumed by
    /// `Build::resolve_accept_prunes`, which re-closes this prefix to build
    /// the pruned state; meaningless afterwards.
    prune_prefix: usize,
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

/// Pack a dense per-state guard list into the stored sparse form: a per-state
/// index (`GUARD_NONE` for guard-free states) plus a table of only the
/// non-empty records. See the `guard_index`/`guard_table` field docs.
fn pack_guards(dense: Vec<StateGuards>) -> (Box<[u32]>, Box<[StateGuards]>) {
    let mut index = vec![GUARD_NONE; dense.len()];
    let mut table = Vec::new();
    for (s, g) in dense.into_iter().enumerate() {
        if !g.is_empty() {
            index[s] = table.len() as u32;
            table.push(g);
        }
    }
    (index.into_boxed_slice(), table.into_boxed_slice())
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
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub struct TaggedNfaState {
    pub state: StateHandle,
    /// Interned handle to this thread's [`TagMap`] (see [`TagMapStore`]).
    /// Comparing/hashing the raw id is equivalent to comparing/hashing the
    /// underlying value, because `intern` guarantees equal values always
    /// share an id — that's what makes deriving `Eq`/`Hash` here still
    /// correct for `TdfaState`'s dedup, without needing interner access at
    /// comparison time.
    tag_map: TagMapId,
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
fn canonicalize(
    cfg: TdfaState,
    interner: &mut TagMapStore,
) -> (TdfaState, TagCommandList, HashMap<InputMark, InputMark>) {
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
        let src = interner.get(entry.tag_map);
        let mut new_tag_map = TagMap::with_capacity(src.len());
        for &slot in src.iter() {
            // `None` (unset tag) passes through unchanged — only real marks
            // get renumbered. First sight of a raw mark mints a fresh
            // canonical id; subsequent sights reuse the memoized one.
            let canon = slot.map(|raw| *mapping.entry(raw).or_insert_with(&mut next_canonical));
            new_tag_map.push(canon);
        }
        // This is the retained (permanent, one-per-live-thread) copy — the
        // transient values built in `close_priority` are only reachable via
        // the interner, which lives for the whole build. Renumbering is a
        // no-op whenever every raw mark already equals its canonical id (the
        // common case for a thread that isn't the newest one this step, or
        // whose ids were already canonical from a prior pass): `intern`
        // finds the source value already registered under `entry.tag_map`
        // and hands back that same id instead of growing the arena.
        let tag_map = interner.intern(new_tag_map);
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
    interner: &mut TagMapStore,
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
                        let written_in_this_closure =
                            match interner.get(parent_tag_map).get(sentinel_idx) {
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
                        let mut sub_tag_map = parent_tag_map;
                        let mut pre_cmds = TagCommandList::new();
                        apply_eps_ops(&edge.ops, alloc, interner, &mut sub_tag_map, &mut pre_cmds);
                        let seed = TaggedNfaState {
                            state: edge.target,
                            tag_map: sub_tag_map,
                        };
                        let mut sub_conds: SmallVec<[AnchorConditional; 1]> = SmallVec::new();
                        let (sub_closure, sub_cmds) = close_priority(
                            alloc,
                            interner,
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
                        let finals = synthesize_finals(&sub_closure, num_tags, interner);
                        conditionals.push(AnchorConditional {
                            cond: edge.cond.clone(),
                            commands: all_cmds,
                            finals,
                            prune: NO_PRUNE,
                            prune_commands: TagCommandList::new(),
                            // Threads popped so far (ending with the current
                            // thread) outrank this accept; the current thread's
                            // own higher-priority eps descendants — also above
                            // the accept — are regenerated when the prefix is
                            // re-closed during prune resolution.
                            prune_prefix: threads.len(),
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
                #[cfg(feature = "std")]
                {
                    use std::sync::atomic::Ordering::Relaxed;
                    if edge.ops.is_empty() {
                        TAGMAP_EPS_CLONES_EMPTY_OPS.fetch_add(1, Relaxed);
                    } else {
                        TAGMAP_EPS_CLONES_NONEMPTY_OPS.fetch_add(1, Relaxed);
                    }
                }
                let mut child_tag_map = parent_tag_map;
                apply_eps_ops(&edge.ops, alloc, interner, &mut child_tag_map, &mut commands);
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
fn empty_tag_map(num_tags: usize, interner: &mut TagMapStore) -> TagMapId {
    let mut v = TagMap::with_capacity(num_tags);
    v.resize(num_tags, None);
    interner.intern(v)
}

/// Apply an eps edge's tag-write ops to the child tag_map.
/// `CurrentPos` mints a fresh mark and emits a `TagCommand` so the
/// executor writes the input position at runtime. `Nil` clears the
/// slot to `None` directly — no mark, no command.
///
/// An empty `ops` (the common case — most eps edges write no tags) is a pure
/// no-op that leaves `child_tag_map`'s id untouched, i.e. still shared with
/// its source. Only a real write pays for cloning the current value out of
/// the interner, mutating the clone, and re-interning it.
fn apply_eps_ops(
    ops: &[TagOp],
    alloc: &mut MarkAlloc,
    interner: &mut TagMapStore,
    child_tag_map: &mut TagMapId,
    commands: &mut TagCommandList,
) {
    if ops.is_empty() {
        return;
    }
    let mut v: TagMap = TagMap::from_slice(interner.get(*child_tag_map));
    for op in ops {
        match op.kind {
            OpKind::CurrentPos => {
                let m = alloc.next();
                v[op.tag as usize] = Some(m);
                commands.push(TagCommand {
                    dst: m,
                    src: MarkValue::CurrentPos,
                });
            }
            OpKind::Nil => {
                v[op.tag as usize] = None;
            }
        }
    }
    *child_tag_map = interner.intern(v);
}

/// Working state for the TDFA construction. Bundles every per-state Vec
/// so `register_or_get_state` and friends don't need a dozen
/// out-parameters.
struct Build<'a> {
    nfa: &'a Nfa,
    alloc: &'a mut MarkAlloc,
    tag_interner: TagMapStore,
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
    /// State-count budget for this build (`TDFA_STATE_BUDGET` unless the
    /// caller can recover from `BudgetExceeded` and asked for less).
    budget: usize,
    /// States registered with a mid-input-capable (`multiline $`) conditional,
    /// awaiting leftmost-cut resolution (see `resolve_accept_prunes`). Kept
    /// with their canonical thread lists so the prune prefix can be re-closed.
    pending_prunes: Vec<(TdfaStateId, TdfaState)>,
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
        if id as usize >= self.budget {
            return Err(Error::BudgetExceeded);
        }
        let is_accepting = canon.0.iter().any(|t| t.state == GOAL_STATE);
        let state_finals = synthesize_finals(&canon, self.num_tags, &self.tag_interner);
        if conds.iter().any(conditional_needs_perbyte) {
            self.pending_prunes.push((id, canon.clone()));
        }
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
            &mut self.tag_interner,
            self.nfa,
            seeds,
            self.num_tags,
            at_start_of_input,
            multiline_start_fires,
            wb_fires,
            &mut conds,
        )?;
        let closure = truncate_at_first_goal(closure);
        let (canon, copy_cmds, canon_mapping) = canonicalize(closure, &mut self.tag_interner);
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
        let switch_commands = compute_alt_switch_commands(canon, &canon_alt, &self.tag_interner);
        let (alt_id, _is_new) = self.register_or_get_state(canon_alt, conds_alt)?;
        self.anchor_alts[id as usize].push(AnchorAlt {
            cond,
            alt: alt_id,
            commands: switch_commands,
        });
        Ok(())
    }

    /// Resolve the leftmost-cut successors for `sid`'s mid-input accept
    /// conditionals. For each, re-close the recorded thread prefix (the
    /// threads outranking the accept — the re-closure regenerates the
    /// accepting thread's own higher-priority descendants, and may keep some
    /// outranked descendants too, which is safe: `consider_accept` still
    /// adjudicates their candidates) and register the result as the state to
    /// switch to when the accept fires. Threads *not* reachable from the
    /// prefix — the later-start scanners the accept outranks — drop out, which
    /// is what lets the scan die instead of running to end of input. The
    /// switch commands are computed like an anchor-alt's (structural slot
    /// diff), so no raw-mark bookkeeping crosses the canonicalization.
    fn resolve_accept_prunes(&mut self, sid: TdfaStateId, canon: &TdfaState) -> Result<(), Error> {
        for cidx in 0..self.anchor_conditionals[sid as usize].len() {
            let ac = &self.anchor_conditionals[sid as usize][cidx];
            if !conditional_needs_perbyte(ac) {
                continue;
            }
            // Clamped: `truncate_at_first_goal` may have shortened the list
            // below the recorded prefix (an eager accept above this
            // conditional); re-closing everything then dedups to `sid` below.
            let count = ac.prune_prefix.min(canon.0.len());
            let seeds: Vec<TaggedNfaState> = canon.0[..count].to_vec();
            let (canon_prune, _entry, conds_prune) =
                self.closure_from_seeds(&seeds, false, false, &[])?;
            let switch_commands = compute_alt_switch_commands(canon, &canon_prune, &self.tag_interner);
            let canon_for_alt = canon_prune.clone();
            let (prune_id, is_new) = self.register_or_get_state(canon_prune, conds_prune)?;
            if is_new {
                self.compute_anchor_alt_for(&canon_for_alt, &seeds, false, prune_id)?;
            }
            if prune_id == sid {
                continue; // Cut keeps the state unchanged — nothing to gain.
            }
            let ac = &mut self.anchor_conditionals[sid as usize][cidx];
            ac.prune = prune_id;
            ac.prune_commands = switch_commands;
        }
        Ok(())
    }
}

/// Translate the marks array's layout from `canon_next` to `canon_alt`.
/// For each `(NFA_state, tag_idx)` entry present in both, emit a `Copy`
/// from the next-state's canonical slot to the alt-state's (when they
/// differ). For entries only in the alt — added by the ^-extension —
/// emit a `CurrentPos`, since their values are the position at which
/// ^ just fired.
fn compute_alt_switch_commands(
    canon_next: &TdfaState,
    canon_alt: &TdfaState,
    interner: &TagMapStore,
) -> TagCommandList {
    let mut next_map: HashMap<(StateHandle, usize), InputMark> = HashMap::new();
    for thread in &canon_next.0 {
        for (idx, slot) in interner.get(thread.tag_map).iter().enumerate() {
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
        for (idx, slot) in interner.get(thread.tag_map).iter().enumerate() {
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
        tag_map: empty_tag_map(build.num_tags, &mut build.tag_interner),
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
fn synthesize_finals(
    canon: &TdfaState,
    num_tags: usize,
    interner: &TagMapStore,
) -> SmallVec<[FinalCommand; 4]> {
    let goal = match canon.0.iter().find(|t| t.state == GOAL_STATE) {
        Some(t) => t,
        None => return SmallVec::new(),
    };
    let goal_tag_map = interner.get(goal.tag_map);
    let mut out: SmallVec<[FinalCommand; 4]> = SmallVec::new();
    for tag in 0..num_tags {
        // Skip tags with no surviving thread holding them. The executor
        // initializes tag values to TEXT_POS_NO_MATCH, so absence of a
        // FinalCommand is equivalent to writing "unset".
        if let Some(mark) = goal_tag_map.get(tag).copied().flatten() {
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
/// - **`AsciiRanges`** (all non-ASCII excluded; set decomposes into ≤SCAN_MAX_RANGES byte ranges):
///   SSE2 saturating-subtract range masks, 16 bytes per iteration.  Falls back
///   to scalar range check on non-x86-64.
/// - **`AsciiRangesStop`** (all non-ASCII are self-loop bytes; ASCII *exit* bytes
///   form ≤SCAN_MAX_RANGES ranges): SSE2 scan that stops at first byte *in* the
///   stop ranges.  Complement of `AsciiRanges`.  Covers unanchored start states
///   like `.*?` before `\w+` where non-ASCII bytes are continuations.
/// - **`BitmapAscii`** (all non-ASCII excluded; too many ranges for `AsciiRanges`):
///   pre-store the two ASCII bitmap words; select with a conditional move.
/// - **`Bitmap`**: full 256-bit bitmap; non-ASCII bytes may be self-loop bytes.
pub const SCAN_MAX_RANGES: usize = 4;

#[derive(Debug, Clone, Copy)]
pub enum ScanFast {
    Bitmap,
    Memchr { count: u8, bytes: [u8; 3] },
    AsciiBarrier { count: u8, bytes: [u8; 3] },
    /// All non-ASCII excluded; set fits in ≤`SCAN_MAX_RANGES` byte ranges.
    /// `count` ranges packed as (lo, hi) pairs in `pairs[0..2*count]`.
    /// `bm0`/`bm1` are the ASCII bitmap words (bytes 0x00-0x3F / 0x40-0x7F)
    /// for the scalar tail — same cost as BitmapAscii per-byte but no alloc.
    AsciiRanges { count: u8, pairs: [u8; 2 * SCAN_MAX_RANGES], bm0: u64, bm1: u64 },
    /// All non-ASCII are self-loop bytes; the *exit* (excluded) ASCII bytes
    /// fit in ≤`SCAN_MAX_RANGES` ranges.  Scan continues while the byte is
    /// NOT in the stop ranges (and is not non-ASCII).
    /// `bm0`/`bm1` mirror `BitmapAscii` for the scalar byte-by-byte tail.
    AsciiRangesStop { count: u8, pairs: [u8; 2 * SCAN_MAX_RANGES], bm0: u64, bm1: u64 },
    /// Non-ASCII excluded; `bm0` = `byte_bitmap[0]` (bytes 0x00-0x3F),
    /// `bm1` = `byte_bitmap[1]` (bytes 0x40-0x7F).
    BitmapAscii { bm0: u64, bm1: u64 },
}

/// Sentinel for the per-state accelerator indexes (`psl_index`,
/// `scan_skip_index`): the state has no accelerator record.
pub const ACCEL_NONE: u32 = u32::MAX;

/// Flat (static-representable) form of [`ScanSkip`]: the stamp list becomes a
/// range into the shared stamp arena. This is what the packed side tables
/// store; the rich SmallVec form exists only during computation.
#[derive(Debug, Clone, Copy)]
pub struct ScanSkipFlat {
    pub byte_bitmap: [u64; 4],
    pub fast: ScanFast,
    /// `(offset, len)` into [`Tdfa::stamp_arena`]; `len == 0` ⇒ pure skip.
    pub stamp: (u32, u32),
}

/// Flat form of [`PosStampLoop`] — see [`ScanSkipFlat`].
#[derive(Debug, Clone, Copy)]
pub struct PosStampLoopFlat {
    pub byte_bitmap: [u64; 4],
    pub fast: ScanFast,
    /// `(offset, len)` into [`Tdfa::stamp_arena`].
    pub stamp: (u32, u32),
    /// Cached `accept_fallback[state]` (see [`PosStampLoop::needs_snapshot`]).
    pub needs_snapshot: bool,
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

    // Tag commands to apply when a transition fires (CSR: per-transition cell
    // into a shared, interned arena — see [`CsrTable`]). Each entry is a list
    // of `TagCommand`s — CurrentPos writes first, then Copy writes from
    // canonicalization. May be empty when a transition has no tag effect,
    // which is the overwhelming majority of cells; that emptiness is why this
    // moved off a dense `Box<[TagCommandList]>` (64 bytes of `SmallVec`
    // baseline per cell regardless of content — measured at ~79% of a built
    // automaton's `heap_bytes` for a capture-free pattern). Retained for
    // display/debug and the scalar fallback; the executor's hot loop applies
    // `transition_moves`.
    transition_commands: CsrTable<TagCommand>,

    // Precompiled in-place move sequence per transition, same indexing as
    // `transition_commands` (arena + per-cell range — see [`MoveTable`]).
    // Built by `compile_moves_all` at the end of `try_from` and rebuilt after
    // `optimize` (which changes `num_marks` and the command lists). The
    // executor's per-byte hot loop applies these in order, in place, instead
    // of interpreting `TagCommand`s. An empty entry has no tag effect (skip).
    transition_moves: MoveTable,

    // Per-state finalization commands (CSR: per-state cell into a shared,
    // interned arena). For accepting states this is `num_tags` commands (one
    // per tag) describing how to read the final capture positions out of the
    // mark file. For non-accepting states it's empty. Run once at scan end
    // against the last-accepted state's mark snapshot.
    finals: CsrTable<FinalCommand>,

    // Per-state zero-width guards: the unified table for `^ $ \b \B`. Each
    // [`StateGuards`] holds a state's `switches` (multiline `^`, `\b`/`\B` —
    // change state and keep matching) and `accepts` (`$` — record a match
    // candidate without changing state). The executor decodes each guard's
    // `cond` from the position's `boundary_signature` (see `anchors.rs`).
    //
    // Stored packed: `guard_index` (indexed by state ID) holds `GUARD_NONE`
    // for the — usually all — guard-free states, else an index into
    // `guard_table`, which keeps a `StateGuards` record per guarded state
    // only. A `StateGuards` is ~250 bytes of inline `SmallVec`s even when
    // empty, so the dense per-state layout charged every automaton for the
    // rare anchored-guard patterns.
    guard_index: Box<[u32]>,
    guard_table: Box<[StateGuards]>,

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

    // Per-transition flag byte. Same shape/indexing as `transitions`
    // (`state * num_classes + class`). `TF_ACCEPT` (bit 0) is set when the target
    // state is accepting; `TF_FALLBACK` (bit 1) when it also has `accept_fallback`;
    // `TF_SWITCHES`/`TF_ACCEPTS` (bits 2/3) when it carries guard switches/
    // accepts. Loaded in parallel with `transitions[idx]` so these checks cost
    // no serial memory hops after the transition lookup; eliminates the per-byte
    // `accepting[]` / `accept_fallback[]` loads in the !HAS_PERBYTE_GUARDS path
    // and the per-byte guard-table touches in the HAS_PERBYTE_GUARDS path
    // (which falls back to the per-state tables only after a switch fires).
    trans_flags: Box<[u8]>,

    // Entry commands pre-compiled to `MoveOp` sequences so the executor can
    // apply them with the same tight loop used for per-transition moves,
    // avoiding the `apply_cmds_scalar` overhead (SmallVec + two-pass scan).
    // Parallel structure to `entry_commands_anchored/unanchored`.
    entry_moves_anchored: Box<[MoveOp]>,
    entry_moves_unanchored: Box<[MoveOp]>,

    /// Per-state position-stamp self-loop accelerator, stored sparse: a
    /// per-state index (`ACCEL_NONE` for most states) into a packed table.
    /// A state qualifies when it is accepting and every self-loop transition
    /// is a pure curpos-stamp (all `MoveOp::src == curpos_lane`) with
    /// consistent targets. Empty when `transition_moves` was not compiled.
    psl_index: Box<[u32]>,
    psl_table: Box<[PosStampLoopFlat]>,

    /// Per-state ASCII bitmap pair for the PSL byte set.  Indexed by state ID.
    /// For PSL-Some ASCII-only states: `(byte_bitmap[0], byte_bitmap[1])` — the
    /// two 64-bit words covering the ASCII range (bytes 0x00–0x7F) of the
    /// self-loop set.  `(0, 0)` for PSL-None states or states whose PSL set
    /// includes non-ASCII bytes.
    ///
    /// Stride is 16 bytes = `state << 4`, no multiply.  The executor uses this
    /// to peek at the next byte before committing to the full PSL scan.
    /// Non-zero ⇔ `psl_index[state] != ACCEL_NONE` with an ASCII-only set.
    /// Flat `(bm0, bm1)` pairs at `2s` / `2s + 1` (byte-blob friendly).
    psl_ascii_bms: Box<[u64]>,

    /// Per-state scan-skip accelerator, sparse like `psl_index`/`psl_table`.
    /// A state qualifies when it is non-accepting and has at least one
    /// self-loop transition with empty (or uniform curpos-stamp) move ops —
    /// the executor can bypass those bytes entirely.
    scan_skip_index: Box<[u32]>,
    scan_skip_table: Box<[ScanSkipFlat]>,

    /// Shared arena for the accelerator tables' stamp-mark lists (interned;
    /// `ScanSkipFlat::stamp` / `PosStampLoopFlat::stamp` are ranges into it).
    stamp_arena: Box<[u16]>,
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
    /// Compiled `MoveOp`s summed over every transition cell (what a
    /// per-transition layout would store).
    pub move_ops_total: usize,
    /// `MoveOp`s actually stored in the deduped arena (see [`MoveTable`]).
    pub move_ops_arena: usize,
    /// Estimated heap footprint of the automaton's tables in bytes: every
    /// per-transition and per-state table plus SmallVec spill. Inline struct
    /// fields (`byte_to_class` etc.) and allocator overhead are not counted.
    pub heap_bytes: usize,
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
    transition_commands: &CsrTable<TagCommand>,
    finals: &CsrTable<FinalCommand>,
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
            for cmd in transition_commands.iat(s * k + c) {
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
    if !has_nonascii_excl {
        // All non-ASCII bytes are self-loop bytes.
        if !ascii_excl_overflow {
            ScanFast::Memchr { count: ascii_excl_count, bytes: ascii_excl_bytes }
        } else if let Some((count, pairs)) = build_ascii_ranges(byte_bitmap[0], byte_bitmap[1], true) {
            ScanFast::AsciiRangesStop { count, pairs, bm0: byte_bitmap[0], bm1: byte_bitmap[1] }
        } else {
            ScanFast::Bitmap
        }
    } else if has_nonascii_excl && ascii_excl_count <= 2 && !ascii_excl_overflow {
        ScanFast::AsciiBarrier { count: ascii_excl_count, bytes: ascii_excl_bytes }
    } else if all_nonascii_excl {
        if let Some((count, pairs)) = build_ascii_ranges(byte_bitmap[0], byte_bitmap[1], false) {
            ScanFast::AsciiRanges { count, pairs, bm0: byte_bitmap[0], bm1: byte_bitmap[1] }
        } else {
            ScanFast::BitmapAscii { bm0: byte_bitmap[0], bm1: byte_bitmap[1] }
        }
    } else {
        ScanFast::Bitmap
    }
}

/// Convert the ASCII half of a bitmap (bm0 for 0x00–0x3F, bm1 for 0x40–0x7F)
/// into a compact list of (lo, hi) byte ranges covering the *selected* bytes.
/// When `complement` is false the selected bytes are the *set* bits (self-loop
/// bytes, for [`ScanFast::AsciiRanges`]); when true the selected bytes are the
/// *clear* bits (excluded/stop bytes, for [`ScanFast::AsciiRangesStop`]).
/// Returns `None` if the selected set requires more than `SCAN_MAX_RANGES` ranges.
fn build_ascii_ranges(bm0: u64, bm1: u64, complement: bool) -> Option<(u8, [u8; 2 * SCAN_MAX_RANGES])> {
    let mut pairs = [0u8; 2 * SCAN_MAX_RANGES];
    let mut count = 0usize;
    let mut in_range = false;
    let mut range_start = 0u8;

    for b in 0u8..=0x7F {
        let word = if b < 0x40 { bm0 } else { bm1 };
        let selected = ((word >> (b as usize & 63)) & 1 != 0) ^ complement;
        if selected && !in_range {
            range_start = b;
            in_range = true;
        } else if !selected && in_range {
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
        Self::try_from_with_budget(nfa, TDFA_STATE_BUDGET)
    }

    /// [`try_from`](Self::try_from) with an explicit state budget. Callers
    /// that can *recover* from `BudgetExceeded` (the Scan → Prefix strategy
    /// fallback) pass a small budget so a doomed subset blowup dies in
    /// milliseconds instead of grinding to the full build budget — at
    /// `regex!` expansion time that's the difference between instant and
    /// tens of seconds per pattern.
    pub fn try_from_with_budget(nfa: &Nfa, budget: usize) -> Result<Self, Error> {
        #[cfg(feature = "std")]
        {
            use std::sync::atomic::Ordering::Relaxed;
            TAGMAP_BYTE_STEP_CLONES.store(0, Relaxed);
            TAGMAP_EPS_CLONES_EMPTY_OPS.store(0, Relaxed);
            TAGMAP_EPS_CLONES_NONEMPTY_OPS.store(0, Relaxed);
        }
        let (byte_to_class, num_classes) = compute_byte_classes(nfa);
        let rep_bytes = representative_bytes(&byte_to_class, num_classes);
        let num_tags = nfa.num_tags();

        let mut alloc = MarkAlloc::new();
        let mut build = Build {
            nfa,
            alloc: &mut alloc,
            tag_interner: TagMapStore::new(num_tags),
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
            pending_prunes: Vec::new(),
            budget,
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

        #[cfg(feature = "std")]
        let mut mem_trace = MemTrace::init();

        // Outer fixpoint: drain the transition worklist, then resolve one
        // state's accept prunes (which may register new states, refilling the
        // worklist — and those states may carry prunable accepts of their
        // own). Both queues only grow with fresh states, so the budget bounds
        // the loop.
        loop {
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
                        #[cfg(feature = "std")]
                        TAGMAP_BYTE_STEP_CLONES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                    #[cfg(feature = "std")]
                    if let Some(mt) = mem_trace.as_mut() {
                        mt.record(&canon_for_alt, build.alloc.count(), &build.tag_interner);
                    }
                    build.compute_anchor_alt_for(
                        &canon_for_alt,
                        &seeds,
                        /* at_start_of_input */ false,
                        target_id,
                    )?;
                }
            }
        }
        let Some((sid, canon)) = build.pending_prunes.pop() else {
            break;
        };
        build.resolve_accept_prunes(sid, &canon)?;
        }

        let num_marks = build.alloc.count() as usize;
        let finals = CsrTable::from_lists(build.finals.iter());
        let transition_commands = CsrTable::from_lists(build.transition_commands.iter());
        let accept_fallback = compute_accept_fallback(
            &build.accepting,
            &build.transitions,
            &transition_commands,
            &finals,
            num_classes,
            num_marks,
        );

        // Fuse the two per-state construction lists into the unified guard table
        // (switches = alts, accepts = conditionals), then pack it sparse.
        let guards: Vec<StateGuards> = build
            .anchor_alts
            .into_iter()
            .zip(build.anchor_conditionals)
            .map(|(switches, accepts)| StateGuards { switches, accepts })
            .collect();
        let (guard_index, guard_table) = pack_guards(guards);
        let word_icase = guards_word_icase(&guard_table);
        let has_perbyte_guards = guard_table.iter().any(state_guards_need_perbyte);
        let has_eoi_accepts = guard_table.iter().any(|g| !g.accepts.is_empty());

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
            transition_commands,
            transition_moves: MoveTable::default(),
            finals,
            guard_index,
            guard_table,
            word_icase,
            has_perbyte_guards,
            has_eoi_accepts,
            start_fixed: false,
            exec_transitions: Box::default(),
            trans_flags: Box::default(),
            entry_moves_anchored: Box::default(),
            entry_moves_unanchored: Box::default(),
            psl_index: Box::default(),
            psl_table: Box::default(),
            psl_ascii_bms: Box::default(),
            scan_skip_index: Box::default(),
            scan_skip_table: Box::default(),
            stamp_arena: Box::default(),
        };
        tdfa.compile_moves_all();
        // Build after compile_moves_all so accept_fallback (computed above) is current.
        tdfa.build_trans_flags();
        #[cfg(feature = "std")]
        {
            report_tagmap_clone_counts();
            if std::env::var("REGRESS_TDFA_MEM_TRACE").is_ok() {
                let n = build.tag_interner.arena.len() / build.num_tags.max(1);
                let arena_bytes = build.tag_interner.arena.len() * size_of::<Option<InputMark>>();
                let index_bytes = n * (size_of::<TagMap>() + size_of::<TagMapId>());
                eprintln!(
                    "tdfa_tagmap_store unique_values={n} arena_bytes(flat)={arena_bytes} \
                     index_bytes(approx)={index_bytes} total(approx)={} bytes",
                    arena_bytes + index_bytes,
                );
            }
        }
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
            self.transition_moves = MoveTable::default();
            self.entry_moves_anchored = Box::default();
            self.entry_moves_unanchored = Box::default();
            self.psl_index = Box::default();
            self.psl_table = Box::default();
            self.psl_ascii_bms = Box::default();
            self.scan_skip_index = Box::default();
            self.scan_skip_table = Box::default();
            self.stamp_arena = Box::default();
            return;
        }
        // Compile each command list and intern the result: identical sequences
        // (exact after canonical emission) share one arena range.
        let moves = MoveTable::from_lists(
            (0..self.transitions.len())
                .map(|i| compile_moves(self.transition_commands.iat(i), num_marks)),
        );
        self.transition_moves = moves;
        self.entry_moves_anchored =
            compile_moves(&self.entry_commands_anchored, num_marks);
        self.entry_moves_unanchored =
            compile_moves(&self.entry_commands_unanchored, num_marks);
        let psls = self.compute_pos_stamp_loops();
        self.psl_ascii_bms = psls
            .iter()
            .flat_map(|opt| match opt {
                None => [0u64, 0u64],
                Some(psl) => {
                    // Only emit non-zero bitmaps for ASCII-only PSL sets (bytes[2]/[3] = 0).
                    // The executor treats (0,0) as "no peek optimisation" — non-ASCII PSL
                    // sets fall back to the old scan path.
                    if psl.byte_bitmap[2] == 0 && psl.byte_bitmap[3] == 0 {
                        [psl.byte_bitmap[0], psl.byte_bitmap[1]]
                    } else {
                        [0, 0]
                    }
                }
            })
            .collect();
        let skips = self.compute_scan_skips();

        // Pack both accelerators sparse: per-state sentinel index + packed
        // table, stamp lists interned into one shared arena.
        let mut stamp_arena: Vec<u16> = Vec::new();
        let mut stamp_intern: HashMap<Box<[u16]>, (u32, u32)> = HashMap::new();
        let mut intern_stamp = |marks: &[u16], arena: &mut Vec<u16>| -> (u32, u32) {
            if marks.is_empty() {
                return (0, 0);
            }
            if let Some(&r) = stamp_intern.get(marks) {
                return r;
            }
            let off = u32::try_from(arena.len()).expect("stamp arena exceeds u32 range");
            arena.extend_from_slice(marks);
            let r = (off, marks.len() as u32);
            stamp_intern.insert(marks.into(), r);
            r
        };
        let mut psl_index = vec![ACCEL_NONE; psls.len()];
        let mut psl_table: Vec<PosStampLoopFlat> = Vec::new();
        for (s, opt) in psls.iter().enumerate() {
            if let Some(psl) = opt {
                psl_index[s] = psl_table.len() as u32;
                psl_table.push(PosStampLoopFlat {
                    byte_bitmap: psl.byte_bitmap,
                    fast: psl.fast,
                    stamp: intern_stamp(&psl.stamp_marks, &mut stamp_arena),
                    needs_snapshot: psl.needs_snapshot,
                });
            }
        }
        let mut scan_skip_index = vec![ACCEL_NONE; skips.len()];
        let mut scan_skip_table: Vec<ScanSkipFlat> = Vec::new();
        for (s, opt) in skips.iter().enumerate() {
            if let Some(ss) = opt {
                scan_skip_index[s] = scan_skip_table.len() as u32;
                scan_skip_table.push(ScanSkipFlat {
                    byte_bitmap: ss.byte_bitmap,
                    fast: ss.fast,
                    stamp: intern_stamp(&ss.stamp_marks, &mut stamp_arena),
                });
            }
        }
        self.psl_index = psl_index.into_boxed_slice();
        self.psl_table = psl_table.into_boxed_slice();
        self.scan_skip_index = scan_skip_index.into_boxed_slice();
        self.scan_skip_table = scan_skip_table.into_boxed_slice();
        self.stamp_arena = stamp_arena.into_boxed_slice();
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

    /// Build `trans_flags`: per-transition flag bytes (`TF_ACCEPT` /
    /// `TF_FALLBACK` / `TF_SWITCHES` / `TF_ACCEPTS`). Must be called after
    /// `accept_fallback` and the guard tables are in their final state (see
    /// `optimize`).
    fn build_trans_flags(&mut self) {
        let accepting = &self.accepting;
        let accept_fallback = &self.accept_fallback;
        let guard_index = &self.guard_index;
        let guard_table = &self.guard_table;
        self.trans_flags = self
            .transitions
            .iter()
            .map(|&t| {
                if t == TDFA_DEAD_STATE {
                    0
                } else {
                    let mut f: u8 = 0;
                    if accepting[t as usize] {
                        f |= TF_ACCEPT;
                    }
                    if accept_fallback[t as usize] {
                        f |= TF_FALLBACK;
                    }
                    let gi = guard_index[t as usize];
                    if gi != GUARD_NONE {
                        let g = &guard_table[gi as usize];
                        if !g.switches.is_empty() {
                            f |= TF_SWITCHES;
                        }
                        if !g.accepts.is_empty() {
                            f |= TF_ACCEPTS;
                        }
                    }
                    f
                }
            })
            .collect();
    }

    /// Per-transition accept + fallback flag bytes for the capture-path hot loop.
    pub(crate) fn trans_flags(&self) -> &[u8] {
        &self.trans_flags
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
        let cond_finals = self.guard_table.iter().flat_map(|g| g.accepts.iter().map(|ac| &ac.finals));
        for finals in core::iter::once(&*self.finals.arena).chain(cond_finals.map(|f| f.as_slice())) {
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
            .arena
            .iter()
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
        !self.transition_moves.cells.is_empty()
    }

    /// Per-state position-stamp self-loop table.  Indexed by state ID.
    /// Returns an empty slice when `transition_moves` was not compiled.
    /// Sparse PSL accelerator: `(per-state index with ACCEL_NONE, packed table)`.
    pub(crate) fn psl_tables(&self) -> (&[u32], &[PosStampLoopFlat]) {
        (&self.psl_index, &self.psl_table)
    }

    /// Shared stamp-mark arena the accelerator `stamp` ranges point into.
    pub(crate) fn stamp_arena(&self) -> &[u16] {
        &self.stamp_arena
    }

    /// Per-state ASCII bitmap pair for the PSL byte set.  `(0,0)` for PSL-None
    /// states or PSL sets that include non-ASCII bytes.  Indexed by state ID;
    /// 16-byte stride (`state << 4`), no multiply.
    pub(crate) fn psl_ascii_bms(&self) -> &[u64] {
        &self.psl_ascii_bms
    }

    /// Per-state scan-skip table.  Indexed by state ID.
    /// Returns an empty slice when `transition_moves` was not compiled.
    /// Sparse scan-skip accelerator: `(per-state index with ACCEL_NONE, packed table)`.
    pub(crate) fn scan_skip_tables(&self) -> (&[u32], &[ScanSkipFlat]) {
        (&self.scan_skip_index, &self.scan_skip_table)
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
        self.has_perbyte_guards = self.guard_table.iter().any(state_guards_need_perbyte);
        self.has_eoi_accepts = self.guard_table.iter().any(|g| !g.accepts.is_empty());
        self.word_icase = guards_word_icase(&self.guard_table);
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
        // Rebuild after accept_fallback is refreshed (compile_moves_all above used the stale value).
        self.build_trans_flags();
    }

    /// The zero-width guards for `state` (switches + accepts). `None` for the
    /// — usually all — guard-free states; the stored form is a per-state index
    /// into a packed table of only the guarded states.
    pub(crate) fn guards(&self, state: TdfaStateId) -> Option<&StateGuards> {
        let i = self.guard_index[state as usize];
        if i == GUARD_NONE {
            None
        } else {
            Some(&self.guard_table[i as usize])
        }
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
        for i in 0..self.transitions.len() {
            tally(self.transition_commands.iat(i));
        }
        for g in self.guard_table.iter() {
            for sw in &g.switches {
                tally(&sw.commands);
            }
            for ac in &g.accepts {
                tally(&ac.commands);
                tally(&ac.prune_commands);
            }
        }
        TdfaStats {
            num_states: self.num_states(),
            num_marks: self.num_marks,
            total_commands: total,
            copy_commands: copy,
            currentpos_commands: cur,
            move_ops_total: self
                .transition_moves
                .cells
                .chunks_exact(2)
                .map(|c| c[1] as usize)
                .sum(),
            move_ops_arena: self.transition_moves.arena.len(),
            heap_bytes: self.heap_bytes(),
        }
    }

    /// Estimate the heap bytes held by the automaton's tables (see
    /// [`TdfaStats::heap_bytes`]).
    fn heap_bytes(&self) -> usize {
        use core::mem::size_of;
        fn smallvec_bytes<A: smallvec::Array>(v: &SmallVec<A>) -> usize {
            size_of::<SmallVec<A>>()
                + if v.spilled() {
                    v.capacity() * size_of::<A::Item>()
                } else {
                    0
                }
        }
        let mut bytes = 0usize;
        bytes += self.transitions.len() * size_of::<TdfaStateId>();
        bytes += self.trans_flags.len();
        bytes += self.exec_transitions.len() * size_of::<u32>();
        bytes += self.transition_commands.cells.len() * size_of::<u32>()
            + self.transition_commands.arena.len() * size_of::<TagCommand>();
        bytes += self.transition_moves.cells.len() * size_of::<u32>()
            + self.transition_moves.arena.len() * size_of::<MoveOp>();
        bytes += self.accepting.len() + self.accept_fallback.len();
        bytes += self.finals.cells.len() * size_of::<u32>()
            + self.finals.arena.len() * size_of::<FinalCommand>();
        bytes += self.guard_index.len() * size_of::<u32>();
        for g in self.guard_table.iter() {
            bytes += size_of::<StateGuards>();
            for sw in &g.switches {
                bytes += smallvec_bytes(&sw.commands);
            }
            for ac in &g.accepts {
                bytes += smallvec_bytes(&ac.commands)
                    + smallvec_bytes(&ac.prune_commands)
                    + smallvec_bytes(&ac.finals);
            }
        }
        bytes += self.psl_index.len() * size_of::<u32>()
            + self.psl_table.len() * size_of::<PosStampLoopFlat>();
        bytes += self.psl_ascii_bms.len() * size_of::<u64>();
        bytes += self.scan_skip_index.len() * size_of::<u32>()
            + self.scan_skip_table.len() * size_of::<ScanSkipFlat>();
        bytes += self.stamp_arena.len() * size_of::<u16>();
        bytes
    }

    pub fn transition_commands(&self, idx: usize) -> &[TagCommand] {
        self.transition_commands.iat(idx)
    }

    /// Precompiled in-place move sequences for each transition, same indexing
    /// as [`transition_commands`](Self::transition_commands) via
    /// `Index<usize>`. The executor applies these in its hot loop. See
    /// [`MoveOp`] and [`MoveTable`].
    pub(crate) fn transition_moves(&self) -> &MoveTable {
        &self.transition_moves
    }

    pub fn finals(&self, state: TdfaStateId) -> &[FinalCommand] {
        self.finals.iat(state as usize)
    }

    /// The finals table as raw `(cells, arena)` CSR slices (table-tier emit).
    pub(crate) fn finals_raw(&self) -> (&[u32], &[FinalCommand]) {
        self.finals.as_raw()
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

    fn entry(interner: &mut TagMapStore, state: StateHandle, tags: &[u32]) -> TaggedNfaState {
        let tag_map = interner.intern(tags.iter().map(|&v| Some(InputMark(v))).collect());
        TaggedNfaState { state, tag_map }
    }

    fn cfg(entries: &[TaggedNfaState]) -> TdfaState {
        TdfaState(entries.iter().copied().collect())
    }

    #[test]
    fn configuration_eq_is_order_sensitive() {
        let mut interner = TagMapStore::new(2);
        let a = entry(&mut interner, 1, &[3, 5]);
        let b = entry(&mut interner, 2, &[3, 5]);
        assert_ne!(cfg(&[a, b]), cfg(&[b, a]));
    }

    #[test]
    fn configuration_hash_is_order_sensitive() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut interner = TagMapStore::new(2);
        let a = entry(&mut interner, 1, &[3, 5]);
        let b = entry(&mut interner, 2, &[3, 5]);
        let ab = cfg(&[a, b]);
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
        let mut interner = TagMapStore::new(2);
        let c = cfg(&[entry(&mut interner, 0, &[7, 3]), entry(&mut interner, 1, &[7, 9])]);
        let (canon, _, _) = canonicalize(c, &mut interner);
        let expected = cfg(&[entry(&mut interner, 0, &[0, 1]), entry(&mut interner, 1, &[0, 2])]);
        assert_eq!(canon, expected);
    }

    #[test]
    fn canonicalize_is_idempotent() {
        let mut interner = TagMapStore::new(2);
        let c = cfg(&[entry(&mut interner, 0, &[7, 3]), entry(&mut interner, 1, &[7, 9])]);
        let (once, _, _) = canonicalize(c, &mut interner);
        let (twice, cmds, _) = canonicalize(once.clone(), &mut interner);
        assert_eq!(once, twice);
        assert!(cmds.is_empty());
    }

    #[test]
    fn canonicalize_iso_configs_collapse() {
        let mut interner = TagMapStore::new(2);
        let a = cfg(&[entry(&mut interner, 0, &[3, 5]), entry(&mut interner, 1, &[5, 3])]);
        let b = cfg(&[entry(&mut interner, 0, &[100, 200]), entry(&mut interner, 1, &[200, 100])]);
        let (canon_a, ..) = canonicalize(a, &mut interner);
        let (canon_b, ..) = canonicalize(b, &mut interner);
        assert_eq!(canon_a, canon_b);
    }

    #[test]
    fn canonicalize_emits_copy_commands_in_canonical_order() {
        // Raw 7 -> canonical 0, raw 3 -> canonical 1.
        let mut interner = TagMapStore::new(2);
        let c = cfg(&[entry(&mut interner, 0, &[7, 3])]);
        let (_, cmds, _) = canonicalize(c, &mut interner);
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
        let mut interner = TagMapStore::new(2);
        let c = cfg(&[entry(&mut interner, 0, &[0, 1]), entry(&mut interner, 1, &[0, 2])]);
        let (canon, cmds, _) = canonicalize(c, &mut interner);
        let expected = cfg(&[entry(&mut interner, 0, &[0, 1]), entry(&mut interner, 1, &[0, 2])]);
        assert_eq!(canon, expected);
        assert!(cmds.is_empty());
    }

    #[test]
    fn empty_configuration_canonicalizes_to_empty() {
        let mut interner = TagMapStore::new(2);
        let empty = TdfaState::default();
        let (canon, cmds, _) = canonicalize(empty.clone(), &mut interner);
        assert_eq!(canon, empty);
        assert!(cmds.is_empty());
    }
}
