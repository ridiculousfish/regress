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

use super::{FinalCommand, InputMark, MarkValue, TDFA_DEAD_STATE, TagCommand, TagCommandList, Tdfa};
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
}

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
