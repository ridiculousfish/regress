//! Optional optimization passes over a built `Tdfa`.
//!
//! These never run as part of construction — `Tdfa::try_from` returns a
//! correct, unoptimized automaton. `Tdfa::optimize` (which delegates to
//! `optimize` here) applies them as a separate, skippable step. As a child
//! module of `tdfa`, this file accesses `Tdfa`'s private fields directly.
//!
//! Pipeline order: `compact_marks` first (copy fold + dead-mark elimination +
//! dense renumbering + register allocation), then state minimization. Running
//! register cleanup first empties tag-free transition command lists, making more
//! equivalent states byte-identical for minimization.

use super::{
    FinalCommand, InputMark, MarkValue, StateGuards, TDFA_DEAD_STATE, TagCommand, TagCommandList,
    Tdfa,
};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::collections::HashSet;

/// Run every optimization pass, in order, on `t`. `compact_marks` runs first:
/// folding and dead-mark elimination empty the tag-free transition commands, so
/// equivalent states become byte-identical and `minimize` can merge them.
pub(crate) fn optimize(t: &mut Tdfa) {
    compact_marks(t);
    minimize(t);
}

/// Exact-equality state minimization (Moore partition refinement). Merges
/// states that are byte-for-byte interchangeable: same `accepting`/`finals`,
/// and for every byte class the same target block **and identical transition
/// commands**. Because the commands must match exactly (same marks), this is
/// sound without any register renaming — it mainly merges tag-free regions
/// (e.g. the four equivalent "expecting b" states of `ab|cb|db|eb`).
///
/// States carrying anchor conditionals/alts (`$`, multiline `^`, `\b`) are
/// pinned to their own block (those structures aren't compared here); they're
/// rare.
pub(crate) fn minimize(t: &mut Tdfa) {
    let n = t.accepting.len();
    let k = t.num_classes;
    if n <= 1 {
        return;
    }

    // Intern transition command lists to small ids (exact equality). Key by the
    // `SmallVec` directly and clone only on a miss: the common (small) list is
    // inline, so a hit costs no allocation and a miss clones inline data — vs.
    // the old `.to_vec()` which heap-allocated for every one of `n * k` cells.
    let mut cmd_intern: HashMap<TagCommandList, u32> = HashMap::new();
    let mut cmd_id = vec![0u32; n * k];
    for (idx, slot) in cmd_id.iter_mut().enumerate() {
        let cmds = &t.transition_commands[idx];
        *slot = match cmd_intern.get(cmds) {
            Some(&id) => id,
            None => {
                let id = cmd_intern.len() as u32;
                cmd_intern.insert(cmds.clone(), id);
                id
            }
        };
    }

    // Initial partition by output: accepting + finals, with anchor states pinned.
    let mut block = vec![0u32; n];
    let mut num_blocks: u32 = 0;
    {
        let mut out_intern: HashMap<(bool, Vec<FinalCommand>), u32> = HashMap::new();
        for s in 0..n {
            let pinned = !t.guards[s].is_empty();
            block[s] = if pinned {
                let b = num_blocks;
                num_blocks += 1;
                b
            } else {
                let key = (t.accepting[s], t.finals[s].to_vec());
                match out_intern.get(&key) {
                    Some(&b) => b,
                    None => {
                        let b = num_blocks;
                        num_blocks += 1;
                        out_intern.insert(key, b);
                        b
                    }
                }
            };
        }
    }

    // Refine until the partition stops splitting. Each state's signature is its
    // current block followed by, per class, the target's block and command id.
    // We materialize signatures as fixed-width rows in one reusable buffer and
    // group equal rows by sorting an index permutation — no per-state heap
    // allocation or hashing of long keys (the previous `HashMap<(u32, Vec<_>)>`
    // approach allocated and hashed a length-`k` vector for every state every
    // round, which dominated `optimize` for large automata).
    let row_len = 1 + 2 * k;
    let mut rows = vec![0u32; n * row_len];
    let mut perm: Vec<u32> = (0..n as u32).collect();
    let mut next = vec![0u32; n];
    loop {
        for s in 0..n {
            let base = s * row_len;
            rows[base] = block[s];
            let trow = s * k;
            for c in 0..k {
                let idx = trow + c;
                rows[base + 1 + 2 * c] = block[t.transitions[idx] as usize];
                rows[base + 2 + 2 * c] = cmd_id[idx];
            }
        }
        perm.sort_unstable_by(|&a, &b| {
            let a = a as usize * row_len;
            let b = b as usize * row_len;
            rows[a..a + row_len].cmp(&rows[b..b + row_len])
        });
        let mut next_blocks: u32 = 0;
        for i in 0..n {
            let s = perm[i] as usize;
            if i > 0 {
                let p = perm[i - 1] as usize;
                if rows[s * row_len..s * row_len + row_len]
                    != rows[p * row_len..p * row_len + row_len]
                {
                    next_blocks += 1;
                }
            }
            next[s] = next_blocks;
        }
        next_blocks += 1; // ids are 0-based; the count is the last id + 1.
        std::mem::swap(&mut block, &mut next);
        if next_blocks == num_blocks {
            break;
        }
        num_blocks = next_blocks;
    }

    // Assign dense new ids by first appearance (so the dead state, id 0, stays
    // id 0), recording one representative old state per block.
    let mut block_to_new: HashMap<u32, u32> = HashMap::new();
    let mut old_to_new = vec![0u32; n];
    let mut rep: Vec<usize> = Vec::new();
    for s in 0..n {
        let nid = match block_to_new.get(&block[s]) {
            Some(&id) => id,
            None => {
                let id = rep.len() as u32;
                block_to_new.insert(block[s], id);
                rep.push(s);
                id
            }
        };
        old_to_new[s] = nid;
    }

    let nn = rep.len();
    if nn == n {
        return; // nothing merged
    }

    // Rebuild the per-state arrays from each block's representative, remapping
    // transition targets and anchor-alt targets to the new ids.
    let mut accepting = vec![false; nn];
    let mut finals: Vec<SmallVec<[FinalCommand; 4]>> = vec![SmallVec::new(); nn];
    let mut guards = vec![StateGuards::default(); nn];
    let mut transitions = vec![TDFA_DEAD_STATE; nn * k];
    let mut transition_commands: Vec<TagCommandList> = vec![SmallVec::new(); nn * k];
    for (nid, &r) in rep.iter().enumerate() {
        accepting[nid] = t.accepting[r];
        finals[nid] = t.finals[r].clone();
        let mut g = t.guards[r].clone();
        for sw in g.switches.iter_mut() {
            sw.alt = old_to_new[sw.alt as usize];
        }
        guards[nid] = g;
        for c in 0..k {
            transitions[nid * k + c] = old_to_new[t.transitions[r * k + c] as usize];
            transition_commands[nid * k + c] = t.transition_commands[r * k + c].clone();
        }
    }

    t.accepting = accepting.into_boxed_slice();
    t.finals = finals.into_boxed_slice();
    t.guards = guards.into_boxed_slice();
    t.transitions = transitions.into_boxed_slice();
    t.transition_commands = transition_commands.into_boxed_slice();
    t.start_anchored = old_to_new[t.start_anchored as usize];
    t.start_unanchored = old_to_new[t.start_unanchored as usize];
}

/// Cheap register cleanup: copy folding + dead-mark elimination + dense
/// renumbering. Value-preserving — see `tdfa_backend::apply_commands` for the
/// two-phase (simultaneous) semantics this must respect. Shrinks `num_marks`
/// (the per-search marks Vec) and the per-transition command lists.
pub(crate) fn compact_marks(t: &mut Tdfa) {
    fold_currentpos_copies(t);
    eliminate_dead_marks(t);
    renumber_marks(t);
    register_allocate(t);
    fold_currentpos_copies(t);
    eliminate_dead_marks(t);
    renumber_marks(t);
}

/// Marks of redundancy (above the `num_tags` floor) below which RA is skipped.
///
/// RA can't shrink the mark file below the number of tags that are
/// simultaneously live, and every accept reads *all* live tags into the result
/// at once — so `num_tags` is the floor. When `num_marks` is already within this
/// slack of `num_tags` there's essentially no over-minting to coalesce, and RA's
/// (automaton-size-proportional) liveness fixpoint would just confirm the count,
/// so we skip it. Tying the gate to `num_tags` rather than an absolute mark
/// count is what makes it scale: a many-capture pattern with no redundancy
/// (`num_marks ≈ num_tags`) is skipped regardless of its absolute mark count,
/// while an unanchored alternation's per-state private sets — which blow up to
/// ~O(n²) above the floor — run RA and coalesce back to O(1). (Historically this
/// was an absolute cap of 256, from a since-removed gather executor; that
/// wrongly skipped exactly the moderate-redundancy automata where RA pays off.)
const RA_REDUNDANCY_SLACK: usize = 2;

/// Largest densely-numbered mark count for which we run the register allocator.
/// Above this we keep the (already dead-eliminated, densely renumbered) mark
/// file as-is to bound the liveness fixpoint; such automata are rare and already
/// use the executor's allocation-free scalar command fallback.
const MAX_RA_MARKS: usize = 1 << 14;

/// Interference-graph budget: if `Σ_s |live(s)|²` (the cost of materializing the
/// per-state cliques) would exceed this, the allocator bails after liveness and
/// keeps the renumbered mark file. This caps work for high-register-pressure
/// automata (e.g. a capture group inside an unbounded `.*` loop), which couldn't
/// shrink below the gather cap anyway.
const MAX_RA_INTERFERENCE: u128 = 8_000_000;

/// Fold `r := CurrentPos` (phase 1) + `c := Copy(r)` (phase 2) into
/// `c := CurrentPos`, collapsing raw→canonical indirections for freshly stamped
/// positions. If `r` has no remaining reads, `eliminate_dead_marks` removes the
/// now-dead stamp. If `r` is still read elsewhere, keeping `r := CurrentPos`
/// preserves that value.
///
/// Guarded to stay correct under the simultaneous command semantics: `c` must
/// not be a `Copy` source within this same list, otherwise moving `c`'s write
/// from phase 2 to phase 1 would change what a sibling copy reads from `c` (the
/// parallel-shift case; those marks stay).
fn fold_currentpos_copies(t: &mut Tdfa) {
    fold_list(&mut t.entry_commands_anchored);
    fold_list(&mut t.entry_commands_unanchored);
    for cmds in t.transition_commands.iter_mut() {
        fold_list(cmds);
    }
    for g in t.guards.iter_mut() {
        for sw in g.switches.iter_mut() {
            fold_list(&mut sw.commands);
        }
        for ac in g.accepts.iter_mut() {
            fold_list(&mut ac.commands);
        }
    }
}

/// Dead-mark elimination to a fixpoint: a command whose destination is read
/// nowhere is dead; removing a `Copy` can make its source dead too.
fn eliminate_dead_marks(t: &mut Tdfa) {
    // `used` is a dense bitmap indexed by mark id (all `< num_marks`), reused
    // across fixpoint rounds — no hashing, no per-round reallocation.
    let mut used = vec![false; t.num_marks];
    loop {
        read_marks(t, &mut used);
        let mut changed = false;
        let mut prune = |cmds: &mut TagCommandList| {
            let before = cmds.len();
            cmds.retain(|c| used[c.dst.0 as usize]);
            changed |= cmds.len() != before;
        };
        prune(&mut t.entry_commands_anchored);
        prune(&mut t.entry_commands_unanchored);
        for cmds in t.transition_commands.iter_mut() {
            prune(cmds);
        }
        for g in t.guards.iter_mut() {
            for sw in g.switches.iter_mut() {
                prune(&mut sw.commands);
            }
            for ac in g.accepts.iter_mut() {
                prune(&mut ac.commands);
            }
        }
        if !changed {
            break;
        }
    }
}

/// Set of marks read anywhere — as a `Copy` source in any command or as a
/// `FinalCommand` source. A conservative (never per-path) global use-set, so a
/// mark absent here is read on no path and its writes are dead.
fn read_marks(t: &Tdfa, used: &mut [bool]) {
    used.fill(false);
    collect_cmd_srcs(&t.entry_commands_anchored, used);
    collect_cmd_srcs(&t.entry_commands_unanchored, used);
    for cmds in t.transition_commands.iter() {
        collect_cmd_srcs(cmds, used);
    }
    for fs in t.finals.iter() {
        collect_final_srcs(fs, used);
    }
    for g in t.guards.iter() {
        for sw in &g.switches {
            collect_cmd_srcs(&sw.commands, used);
        }
        for ac in &g.accepts {
            collect_cmd_srcs(&ac.commands, used);
            collect_final_srcs(&ac.finals, used);
        }
    }
}

/// Visit every `InputMark` slot (each command `dst`, and each `Copy` source in
/// commands and finals) across all command-bearing structures.
fn for_each_mark_mut(t: &mut Tdfa, mut f: impl FnMut(&mut InputMark)) {
    visit_cmd_marks(&mut t.entry_commands_anchored, &mut f);
    visit_cmd_marks(&mut t.entry_commands_unanchored, &mut f);
    for cmds in t.transition_commands.iter_mut() {
        visit_cmd_marks(cmds, &mut f);
    }
    for fs in t.finals.iter_mut() {
        visit_final_marks(fs, &mut f);
    }
    for g in t.guards.iter_mut() {
        for sw in g.switches.iter_mut() {
            visit_cmd_marks(&mut sw.commands, &mut f);
        }
        for ac in g.accepts.iter_mut() {
            visit_cmd_marks(&mut ac.commands, &mut f);
            visit_final_marks(&mut ac.finals, &mut f);
        }
    }
}

/// Renumber surviving marks densely (`0..k`) by first appearance in a fixed
/// walk, rewriting every reference, and set `num_marks = k`.
fn renumber_marks(t: &mut Tdfa) {
    // Old ids are all `< num_marks`, so a plain array (sentinel = unassigned)
    // remaps in O(1) without hashing.
    let mut remap = vec![u32::MAX; t.num_marks];
    let mut next = 0u32;
    for_each_mark_mut(t, |m| {
        let old = m.0 as usize;
        if remap[old] == u32::MAX {
            remap[old] = next;
            next += 1;
        }
        *m = InputMark(remap[old]);
    });
    t.num_marks = next as usize;
}

// ---------------------------------------------------------------------------
// Register allocation: coalesce marks with disjoint live ranges.
//
// Marks come in densely numbered (`renumber_marks`) but vastly over-counted:
// `canonicalize` gives each DFA state its own private register set, so
// `num_marks ≈ Σ_states(registers/state)`. At runtime the executor is in one
// state at a time, so those per-state register sets overwhelmingly have
// disjoint lifetimes and can share physical slots. We compute liveness over the
// transition graph, build an interference graph, color it greedily, and rewrite
// every mark reference to its color — collapsing `num_marks` to roughly the
// maximum number of simultaneously-live marks.
//
// Soundness: liveness is over-approximated (gen = all `Copy` sources, kill = all
// command dsts, ignoring the two-phase ordering; conditional/alt sources and
// dsts are all treated as touched at their state). Over-approximation only adds
// interference edges — never removes them — so coalesced marks are guaranteed to
// have non-overlapping live ranges and the match semantics are preserved.
// ---------------------------------------------------------------------------

#[inline]
fn bs_set(bits: &mut [u64], i: u32) {
    bits[(i >> 6) as usize] |= 1u64 << (i & 63);
}

/// Append the indices of set bits in `bits` to `out` (cleared first).
fn bits_to_vec(bits: &[u64], out: &mut Vec<u32>) {
    out.clear();
    for (wi, &word) in bits.iter().enumerate() {
        let mut w = word;
        while w != 0 {
            out.push((wi as u32) * 64 + w.trailing_zeros());
            w &= w - 1;
        }
    }
}

fn register_allocate(t: &mut Tdfa) {
    let m = t.num_marks;
    // Skip when the mark file has no redundancy to coalesce — within a small
    // slack of the `num_tags` floor (see `RA_REDUNDANCY_SLACK`) — so RA's
    // size-proportional liveness fixpoint isn't spent confirming the count. Also
    // skip absurdly large mark files to bound the fixpoint; those can't shrink
    // below the cap and use the scalar fallback.
    if m <= t.num_tags + RA_REDUNDANCY_SLACK || m > MAX_RA_MARKS {
        return;
    }
    let n = t.accepting.len();
    let k = t.num_classes;
    let words = m.div_ceil(64);

    // --- Backward liveness: `live[s]` = marks live when control is at state s.
    // `reads_at[s]` seeds it with the marks read while at s (finals, plus the
    // sources of conditional/alt command + final lists, treated conservatively).
    let mut live = vec![0u64; n * words];
    let mut reads_at = vec![0u64; n * words];
    for s in 0..n {
        let r = &mut reads_at[s * words..(s + 1) * words];
        for fc in &t.finals[s] {
            if let MarkValue::Copy(mk) = fc.src {
                bs_set(r, mk.0);
            }
        }
        for sw in &t.guards[s].switches {
            for c in &sw.commands {
                if let MarkValue::Copy(mk) = c.src {
                    bs_set(r, mk.0);
                }
            }
        }
        for ac in &t.guards[s].accepts {
            for c in &ac.commands {
                if let MarkValue::Copy(mk) = c.src {
                    bs_set(r, mk.0);
                }
            }
            for fc in &ac.finals {
                if let MarkValue::Copy(mk) = fc.src {
                    bs_set(r, mk.0);
                }
            }
        }
    }

    // Predecessor lists for the worklist.
    let mut preds: Vec<Vec<u32>> = vec![Vec::new(); n];
    for s in 0..n {
        for c in 0..k {
            let tgt = t.transitions[s * k + c];
            if tgt != TDFA_DEAD_STATE {
                preds[tgt as usize].push(s as u32);
            }
        }
    }

    // Worklist fixpoint. `acc` accumulates the new `live[s]`.
    let mut in_wl = vec![true; n];
    let mut wl: std::collections::VecDeque<u32> = (0..n as u32).collect();
    let mut acc = vec![0u64; words];
    let mut tmp = vec![0u64; words];
    while let Some(s) = wl.pop_front() {
        let s = s as usize;
        in_wl[s] = false;
        acc.copy_from_slice(&reads_at[s * words..(s + 1) * words]);
        for c in 0..k {
            let tgt = t.transitions[s * k + c];
            if tgt == TDFA_DEAD_STATE {
                continue;
            }
            // Per-edge: live_before = use ∪ (live[tgt] \ def), computed in `tmp`
            // then unioned into `acc` (so edges don't corrupt each other). Over-
            // approximate: def = all dsts, use = all Copy srcs of this edge.
            let cmds = &t.transition_commands[s * k + c];
            tmp.copy_from_slice(&live[tgt as usize * words..(tgt as usize + 1) * words]);
            for cmd in cmds {
                bs_clear(&mut tmp, cmd.dst.0); // kill def
            }
            for cmd in cmds {
                if let MarkValue::Copy(src) = cmd.src {
                    // A `Copy` source stamped by a `CurrentPos` in this same list
                    // reads the fresh stamp (two-phase: CurrentPos = phase 1, Copy
                    // reads = phase 2), so it is *not* live-before. Excluding it
                    // avoids spurious interference from canonicalize's parallel-
                    // shift pattern (`m := pos; x := m`), which otherwise keeps
                    // every per-state stamp mark mutually live and defeats RA.
                    let stamped_here = cmds
                        .iter()
                        .any(|d| d.dst == src && matches!(d.src, MarkValue::CurrentPos));
                    if !stamped_here {
                        bs_set(&mut tmp, src.0); // gen use
                    }
                }
            }
            for (w, &tw) in tmp.iter().enumerate() {
                acc[w] |= tw;
            }
        }
        let cur = &mut live[s * words..(s + 1) * words];
        if &*cur != acc.as_slice() {
            cur.copy_from_slice(&acc);
            for &p in &preds[s] {
                if !in_wl[p as usize] {
                    in_wl[p as usize] = true;
                    wl.push_back(p);
                }
            }
        }
    }

    // Bail if materializing the per-state cliques would be too expensive — a
    // high-register-pressure automaton that can't shrink below the gather cap
    // regardless. The renumbered (un-coalesced) mark file is kept; correctness
    // is unaffected.
    let mut clique_work: u128 = 0;
    for s in 0..n {
        let pc: u128 = live[s * words..(s + 1) * words]
            .iter()
            .map(|w| w.count_ones() as u128)
            .sum();
        clique_work += pc * pc;
        if clique_work > MAX_RA_INTERFERENCE {
            return;
        }
    }

    // --- Interference graph. Marks simultaneously live interfere. Per state s
    // we clique over everything live/touched at s; per non-empty edge we clique
    // over the marks coexisting during its (two-phase) command application.
    let mut adj: Vec<HashSet<u32>> = vec![HashSet::new(); m];
    let mut members: Vec<u32> = Vec::new();
    let mut edgeset = vec![0u64; words];
    let add_clique = |adj: &mut [HashSet<u32>], members: &[u32]| {
        for (i, &a) in members.iter().enumerate() {
            for &b in &members[i + 1..] {
                adj[a as usize].insert(b);
                adj[b as usize].insert(a);
            }
        }
    };
    for s in 0..n {
        // Touched-at-s: live[s] plus all marks appearing in finals/conditional/
        // alt lists at s (sources and dsts), so nothing touched there is wrongly
        // coalesced with a live mark.
        edgeset.copy_from_slice(&live[s * words..(s + 1) * words]);
        for fc in &t.finals[s] {
            if let MarkValue::Copy(mk) = fc.src {
                bs_set(&mut edgeset, mk.0);
            }
        }
        for sw in &t.guards[s].switches {
            for c in &sw.commands {
                bs_set(&mut edgeset, c.dst.0);
                if let MarkValue::Copy(mk) = c.src {
                    bs_set(&mut edgeset, mk.0);
                }
            }
        }
        for ac in &t.guards[s].accepts {
            for c in &ac.commands {
                bs_set(&mut edgeset, c.dst.0);
                if let MarkValue::Copy(mk) = c.src {
                    bs_set(&mut edgeset, mk.0);
                }
            }
            for fc in &ac.finals {
                if let MarkValue::Copy(mk) = fc.src {
                    bs_set(&mut edgeset, mk.0);
                }
            }
        }
        bits_to_vec(&edgeset, &mut members);
        add_clique(&mut adj, &members);

        // Non-empty transition edges: dsts coexist with sources and survivors.
        for c in 0..k {
            let tgt = t.transitions[s * k + c];
            if tgt == TDFA_DEAD_STATE {
                continue;
            }
            let cmds = &t.transition_commands[s * k + c];
            if cmds.is_empty() {
                continue; // covered by the state cliques of s and tgt
            }
            edgeset.copy_from_slice(&live[tgt as usize * words..(tgt as usize + 1) * words]);
            for cmd in cmds {
                bs_set(&mut edgeset, cmd.dst.0);
                if let MarkValue::Copy(src) = cmd.src {
                    bs_set(&mut edgeset, src.0);
                }
            }
            bits_to_vec(&edgeset, &mut members);
            add_clique(&mut adj, &members);
        }
    }

    // --- Greedy coloring, largest-degree-first. Colors become physical slots.
    let mut order: Vec<u32> = (0..m as u32).collect();
    order.sort_unstable_by_key(|&v| core::cmp::Reverse(adj[v as usize].len()));
    let mut color = vec![u32::MAX; m];
    let mut used: Vec<bool> = Vec::new();
    for &v in &order {
        used.clear();
        for &nb in &adj[v as usize] {
            let cc = color[nb as usize];
            if cc != u32::MAX {
                if cc as usize >= used.len() {
                    used.resize(cc as usize + 1, false);
                }
                used[cc as usize] = true;
            }
        }
        let chosen = used.iter().position(|&u| !u).unwrap_or(used.len());
        color[v as usize] = chosen as u32;
    }
    let num_colors = color.iter().map(|&c| c + 1).max().unwrap_or(0) as usize;

    // --- Rewrite every mark reference to its color, then drop self-copies.
    for_each_mark_mut(t, |mk| mk.0 = color[mk.0 as usize]);
    t.num_marks = num_colors;
    drop_identity_copies(t);
}

#[inline]
fn bs_clear(bits: &mut [u64], i: u32) {
    bits[(i >> 6) as usize] &= !(1u64 << (i & 63));
}

/// After register coloring, a `Copy` whose source and destination map to the
/// same slot is a no-op; remove such commands everywhere.
fn drop_identity_copies(t: &mut Tdfa) {
    let prune = |cmds: &mut TagCommandList| {
        cmds.retain(|c| !matches!(c.src, MarkValue::Copy(s) if s == c.dst));
    };
    prune(&mut t.entry_commands_anchored);
    prune(&mut t.entry_commands_unanchored);
    for cmds in t.transition_commands.iter_mut() {
        prune(cmds);
    }
    for g in t.guards.iter_mut() {
        for sw in g.switches.iter_mut() {
            prune(&mut sw.commands);
        }
        for ac in g.accepts.iter_mut() {
            prune(&mut ac.commands);
        }
    }
}

/// Fold `c := Copy(r)` into `c := CurrentPos` within one command list when `r`
/// is stamped by a `CurrentPos` in this list and `c` is not itself a `Copy`
/// source here. See `fold_currentpos_copies` for why.
fn fold_list(cmds: &mut TagCommandList) {
    // Command lists are tiny (typically ≤ a handful), so inline `SmallVec`s with
    // linear membership beat per-call `HashSet` allocations.
    let mut stamped_here: SmallVec<[InputMark; 8]> = SmallVec::new();
    let mut copy_src_here: SmallVec<[InputMark; 8]> = SmallVec::new();
    for c in cmds.iter() {
        match c.src {
            MarkValue::CurrentPos => {
                if !stamped_here.contains(&c.dst) {
                    stamped_here.push(c.dst);
                }
            }
            MarkValue::Copy(s) => {
                if !copy_src_here.contains(&s) {
                    copy_src_here.push(s);
                }
            }
        }
    }
    for c in cmds.iter_mut() {
        if let MarkValue::Copy(r) = c.src {
            if stamped_here.contains(&r) && !copy_src_here.contains(&c.dst) {
                c.src = MarkValue::CurrentPos;
            }
        }
    }
}

/// Add every `Copy` source in `cmds` to `used`.
fn collect_cmd_srcs(cmds: &[TagCommand], used: &mut [bool]) {
    for c in cmds {
        if let MarkValue::Copy(m) = c.src {
            used[m.0 as usize] = true;
        }
    }
}

/// Add every `Copy` source in `finals` to `used`.
fn collect_final_srcs(finals: &[FinalCommand], used: &mut [bool]) {
    for fc in finals {
        if let MarkValue::Copy(m) = fc.src {
            used[m.0 as usize] = true;
        }
    }
}

/// Visit each command's `dst` mark and `Copy` source mark.
fn visit_cmd_marks(cmds: &mut [TagCommand], f: &mut impl FnMut(&mut InputMark)) {
    for c in cmds {
        f(&mut c.dst);
        if let MarkValue::Copy(m) = &mut c.src {
            f(m);
        }
    }
}

/// Visit each final's `Copy` source mark.
fn visit_final_marks(finals: &mut [FinalCommand], f: &mut impl FnMut(&mut InputMark)) {
    for fc in finals {
        if let MarkValue::Copy(m) = &mut fc.src {
            f(m);
        }
    }
}
