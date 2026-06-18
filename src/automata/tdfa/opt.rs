//! Optional optimization passes over a built `Tdfa`.
//!
//! These never run as part of construction — `Tdfa::try_from` returns a
//! correct, unoptimized automaton. `Tdfa::optimize` (which delegates to
//! `optimize` here) applies them as a separate, skippable step. As a child
//! module of `tdfa`, this file accesses `Tdfa`'s private fields directly.
//!
//! Pipeline order (RA last; see the module docs): state minimization, then
//! `compact_marks` (the register cleanup: copy fold + dead-mark elimination +
//! dense renumbering).

use super::{
    FinalCommand, InputMark, MarkValue, TDFA_DEAD_STATE, TagCommand, TagCommandList, Tdfa,
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

    // Intern transition command lists to small ids (exact equality).
    let mut cmd_intern: HashMap<Vec<TagCommand>, u32> = HashMap::new();
    let mut cmd_id = vec![0u32; n * k];
    for (idx, slot) in cmd_id.iter_mut().enumerate() {
        let key: Vec<TagCommand> = t.transition_commands[idx].to_vec();
        let next = cmd_intern.len() as u32;
        *slot = *cmd_intern.entry(key).or_insert(next);
    }

    // Initial partition by output: accepting + finals, with anchor states pinned.
    let mut block = vec![0u32; n];
    let mut num_blocks: u32 = 0;
    {
        let mut out_intern: HashMap<(bool, Vec<FinalCommand>), u32> = HashMap::new();
        for s in 0..n {
            let pinned = !t.anchor_conditionals[s].is_empty() || !t.anchor_alts[s].is_empty();
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

    // Refine until the partition stops splitting. The signature is the current
    // block plus, per class, the target's block and the command id.
    loop {
        let mut sig_intern: HashMap<(u32, Vec<(u32, u32)>), u32> = HashMap::new();
        let mut next = vec![0u32; n];
        let mut next_blocks: u32 = 0;
        for s in 0..n {
            let sig: Vec<(u32, u32)> = (0..k)
                .map(|c| {
                    let idx = s * k + c;
                    (block[t.transitions[idx] as usize], cmd_id[idx])
                })
                .collect();
            let key = (block[s], sig);
            next[s] = match sig_intern.get(&key) {
                Some(&b) => b,
                None => {
                    let b = next_blocks;
                    next_blocks += 1;
                    sig_intern.insert(key, b);
                    b
                }
            };
        }
        block = next;
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
    let mut conds = vec![SmallVec::new(); nn];
    let mut alts = vec![SmallVec::new(); nn];
    let mut transitions = vec![TDFA_DEAD_STATE; nn * k];
    let mut transition_commands: Vec<TagCommandList> = vec![SmallVec::new(); nn * k];
    for (nid, &r) in rep.iter().enumerate() {
        accepting[nid] = t.accepting[r];
        finals[nid] = t.finals[r].clone();
        conds[nid] = t.anchor_conditionals[r].clone();
        let mut a = t.anchor_alts[r].clone();
        for alt in a.iter_mut() {
            alt.alt = old_to_new[alt.alt as usize];
        }
        alts[nid] = a;
        for c in 0..k {
            transitions[nid * k + c] = old_to_new[t.transitions[r * k + c] as usize];
            transition_commands[nid * k + c] = t.transition_commands[r * k + c].clone();
        }
    }

    t.accepting = accepting.into_boxed_slice();
    t.finals = finals.into_boxed_slice();
    t.anchor_conditionals = conds.into_boxed_slice();
    t.anchor_alts = alts.into_boxed_slice();
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
}

/// Mark count at/under which the register allocator is skipped: such mark files
/// are already small, so RA would only marginally shrink the per-scan buffer and
/// the per-transition move compile, not worth its cost. (This was historically
/// the gather-eligibility cap; the executor no longer gathers, but the
/// "already small enough" threshold is still a reasonable RA skip.)
const RA_SKIP_MARKS: usize = 256;

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
/// `c := CurrentPos`, collapsing the raw→canonical indirection for a
/// freshly-stamped position. The freed `r` becomes dead (cleaned up by
/// `eliminate_dead_marks`). Guarded to stay correct under the simultaneous
/// command semantics:
///
/// - `r` must be read globally exactly once (this copy) and written by a
///   `CurrentPos` in this same list (so moving the stamp is value-equal);
/// - `c` must not be a `Copy` source within this same list — otherwise
///   moving `c`'s write from phase 2 to phase 1 would change what a sibling
///   copy reads from `c` (the parallel-shift case; those marks stay).
fn fold_currentpos_copies(t: &mut Tdfa) {
    let src_count = global_src_counts(t);
    fold_list(&mut t.entry_commands_anchored, &src_count);
    fold_list(&mut t.entry_commands_unanchored, &src_count);
    for cmds in t.transition_commands.iter_mut() {
        fold_list(cmds, &src_count);
    }
    for conds in t.anchor_conditionals.iter_mut() {
        for ac in conds.iter_mut() {
            fold_list(&mut ac.commands, &src_count);
        }
    }
    for alts in t.anchor_alts.iter_mut() {
        for alt in alts.iter_mut() {
            fold_list(&mut alt.commands, &src_count);
        }
    }
}

/// Per-mark count of reads (`Copy` sources in commands + `FinalCommand`
/// sources) across the whole automaton.
fn global_src_counts(t: &Tdfa) -> HashMap<InputMark, usize> {
    let mut counts: HashMap<InputMark, usize> = HashMap::new();
    let mut bump_cmds = |cmds: &[TagCommand]| {
        for c in cmds {
            if let MarkValue::Copy(m) = c.src {
                *counts.entry(m).or_insert(0) += 1;
            }
        }
    };
    bump_cmds(&t.entry_commands_anchored);
    bump_cmds(&t.entry_commands_unanchored);
    for cmds in t.transition_commands.iter() {
        bump_cmds(cmds);
    }
    for conds in t.anchor_conditionals.iter() {
        for ac in conds {
            bump_cmds(&ac.commands);
        }
    }
    for alts in t.anchor_alts.iter() {
        for alt in alts {
            bump_cmds(&alt.commands);
        }
    }
    // Final sources count as reads too.
    let mut bump_finals = |finals: &[FinalCommand]| {
        for fc in finals {
            if let MarkValue::Copy(m) = fc.src {
                *counts.entry(m).or_insert(0) += 1;
            }
        }
    };
    for fs in t.finals.iter() {
        bump_finals(fs);
    }
    for conds in t.anchor_conditionals.iter() {
        for ac in conds {
            bump_finals(&ac.finals);
        }
    }
    counts
}

/// Dead-mark elimination to a fixpoint: a command whose destination is read
/// nowhere is dead; removing a `Copy` can make its source dead too.
fn eliminate_dead_marks(t: &mut Tdfa) {
    loop {
        let used = read_marks(t);
        let mut changed = false;
        let mut prune = |cmds: &mut TagCommandList| {
            let before = cmds.len();
            cmds.retain(|c| used.contains(&c.dst));
            changed |= cmds.len() != before;
        };
        prune(&mut t.entry_commands_anchored);
        prune(&mut t.entry_commands_unanchored);
        for cmds in t.transition_commands.iter_mut() {
            prune(cmds);
        }
        for conds in t.anchor_conditionals.iter_mut() {
            for ac in conds.iter_mut() {
                prune(&mut ac.commands);
            }
        }
        for alts in t.anchor_alts.iter_mut() {
            for alt in alts.iter_mut() {
                prune(&mut alt.commands);
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
fn read_marks(t: &Tdfa) -> HashSet<InputMark> {
    let mut used = HashSet::new();
    collect_cmd_srcs(&t.entry_commands_anchored, &mut used);
    collect_cmd_srcs(&t.entry_commands_unanchored, &mut used);
    for cmds in t.transition_commands.iter() {
        collect_cmd_srcs(cmds, &mut used);
    }
    for fs in t.finals.iter() {
        collect_final_srcs(fs, &mut used);
    }
    for conds in t.anchor_conditionals.iter() {
        for ac in conds {
            collect_cmd_srcs(&ac.commands, &mut used);
            collect_final_srcs(&ac.finals, &mut used);
        }
    }
    for alts in t.anchor_alts.iter() {
        for alt in alts {
            collect_cmd_srcs(&alt.commands, &mut used);
        }
    }
    used
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
    for conds in t.anchor_conditionals.iter_mut() {
        for ac in conds.iter_mut() {
            visit_cmd_marks(&mut ac.commands, &mut f);
            visit_final_marks(&mut ac.finals, &mut f);
        }
    }
    for alts in t.anchor_alts.iter_mut() {
        for alt in alts.iter_mut() {
            visit_cmd_marks(&mut alt.commands, &mut f);
        }
    }
}

/// Renumber surviving marks densely (`0..k`) by first appearance in a fixed
/// walk, rewriting every reference, and set `num_marks = k`.
fn renumber_marks(t: &mut Tdfa) {
    let mut remap: HashMap<InputMark, InputMark> = HashMap::new();
    let mut next = 0u32;
    for_each_mark_mut(t, |m| {
        let old = *m;
        let id = match remap.get(&old) {
            Some(&id) => id,
            None => {
                let id = InputMark(next);
                next += 1;
                remap.insert(old, id);
                id
            }
        };
        *m = id;
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
    // Skip when the mark file is already at/under the gather cap: such automata
    // are already gather-eligible and RA would only marginally shrink the
    // buffer, so it isn't worth the per-call cost (this covers the vast majority
    // of patterns). Also skip absurdly large mark files to bound the liveness
    // fixpoint — those can't shrink below the cap and use the scalar fallback.
    if m <= RA_SKIP_MARKS || m > MAX_RA_MARKS {
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
        for ac in &t.anchor_conditionals[s] {
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
        for alt in &t.anchor_alts[s] {
            for c in &alt.commands {
                if let MarkValue::Copy(mk) = c.src {
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
                    bs_set(&mut tmp, src.0); // gen use
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
        for ac in &t.anchor_conditionals[s] {
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
        for alt in &t.anchor_alts[s] {
            for c in &alt.commands {
                bs_set(&mut edgeset, c.dst.0);
                if let MarkValue::Copy(mk) = c.src {
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
    for conds in t.anchor_conditionals.iter_mut() {
        for ac in conds.iter_mut() {
            prune(&mut ac.commands);
        }
    }
    for alts in t.anchor_alts.iter_mut() {
        for alt in alts.iter_mut() {
            prune(&mut alt.commands);
        }
    }
}

/// Fold `c := Copy(r)` into `c := CurrentPos` within one command list when `r`
/// is a once-read mark stamped by a `CurrentPos` in this list and `c` is not
/// itself a `Copy` source here. See `fold_currentpos_copies` for why.
fn fold_list(cmds: &mut TagCommandList, src_count: &HashMap<InputMark, usize>) {
    let mut stamped_here: HashSet<InputMark> = HashSet::new();
    let mut copy_src_here: HashSet<InputMark> = HashSet::new();
    for c in cmds.iter() {
        match c.src {
            MarkValue::CurrentPos => {
                stamped_here.insert(c.dst);
            }
            MarkValue::Copy(s) => {
                copy_src_here.insert(s);
            }
        }
    }
    for c in cmds.iter_mut() {
        if let MarkValue::Copy(r) = c.src {
            if stamped_here.contains(&r)
                && src_count.get(&r).copied().unwrap_or(0) == 1
                && !copy_src_here.contains(&c.dst)
            {
                c.src = MarkValue::CurrentPos;
            }
        }
    }
}

/// Add every `Copy` source in `cmds` to `used`.
fn collect_cmd_srcs(cmds: &[TagCommand], used: &mut HashSet<InputMark>) {
    for c in cmds {
        if let MarkValue::Copy(m) = c.src {
            used.insert(m);
        }
    }
}

/// Add every `Copy` source in `finals` to `used`.
fn collect_final_srcs(finals: &[FinalCommand], used: &mut HashSet<InputMark>) {
    for fc in finals {
        if let MarkValue::Copy(m) = fc.src {
            used.insert(m);
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
