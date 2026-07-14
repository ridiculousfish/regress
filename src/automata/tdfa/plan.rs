//! Shared lowering analysis for the TDFA code generators.
//!
//! Both the native-code JIT (`tdfa/jit`, feature `tdfa-jit`) and the Rust
//! source emitter (`tdfa/rustgen`, feature `codegen`) specialize a built
//! [`Tdfa`] into control flow: states become blocks, transitions become
//! branches or table dispatches, and dominant self-loops are peeled into hot
//! loops. The *decisions* — how a state dispatches, which self-loops are worth
//! peeling — live here so the two backends lower identically; only the
//! instruction/text emission differs.
//!
//! `L` is the backend's block-label type (the JIT's `asm::Label`, the Rust
//! emitter's edge target enum); it only needs `Copy + Eq`.

use crate::automata::tdfa::{MoveOp, TDFA_DEAD_STATE, Tdfa, TdfaStateId};

/// Max number of byte-range compares before a state prefers the jump table.
/// Below this, a compare-chain on the raw byte (no class table, no jump-table
/// memory access) is cheaper; above it, the table's constant cost wins. Tunable.
pub(crate) const RANGE_DISPATCH_THRESHOLD: usize = 8;

/// How a state dispatches on the next input byte.
pub(crate) enum Dispatch<L> {
    /// Every byte dead-ends — branch straight to `done`, skipping the fetch.
    AllDone,
    /// Sparse: a compare-chain on raw byte ranges, falling through to `default`.
    Ranges { runs: Vec<(u8, u8, L)>, default: L },
    /// Dense: load the byte class and dispatch through the per-class table.
    Table,
}

/// Decide how a state dispatches. `target_of(class)` resolves a byte class to
/// the label it branches to (a state block, a capture move stub, or `done`).
/// Coalesces the per-byte targets into runs, picks the most-covered target as
/// the fall-through `default` (so e.g. `[^x]` tests only `x`), and chooses the
/// compare-chain when there are few enough runs, else the jump table.
pub(crate) fn analyze_dispatch<L: Copy + Eq>(
    byte_to_class: &[u8; 256],
    done: L,
    target_of: impl Fn(usize) -> L,
) -> Dispatch<L> {
    let byte_target: Vec<L> = (0..256).map(|b| target_of(byte_to_class[b] as usize)).collect();
    // Coalesce contiguous equal labels into runs.
    let mut runs: Vec<(u8, u8, L)> = Vec::new();
    let mut i = 0usize;
    while i < 256 {
        let lbl = byte_target[i];
        let lo = i;
        while i + 1 < 256 && byte_target[i + 1] == lbl {
            i += 1;
        }
        runs.push((lo as u8, i as u8, lbl));
        i += 1;
    }
    // Most-covered label becomes the fall-through default (fewest compares).
    let mut coverage: Vec<(L, usize)> = Vec::new();
    for &(lo, hi, lbl) in &runs {
        let width = hi as usize - lo as usize + 1;
        match coverage.iter_mut().find(|(l, _)| *l == lbl) {
            Some(e) => e.1 += width,
            None => coverage.push((lbl, width)),
        }
    }
    let default = coverage
        .iter()
        .max_by_key(|(_, bytes)| *bytes)
        .map_or(done, |(l, _)| *l);
    let nondefault: Vec<(u8, u8, L)> =
        runs.into_iter().filter(|&(_, _, l)| l != default).collect();
    if nondefault.is_empty() && default == done {
        Dispatch::AllDone
    } else if nondefault.len() <= RANGE_DISPATCH_THRESHOLD {
        Dispatch::Ranges {
            runs: nondefault,
            default,
        }
    } else {
        Dispatch::Table
    }
}

/// Coalesced byte ranges `[lo, hi]` on which state `s` self-loops
/// (`transitions[s][class(byte)] == s`). Empty when `s` has no self-transition.
/// These are the bytes the peeled hot loop tests inline before falling through
/// to the state's regular exit dispatch.
pub(crate) fn self_loop_runs(
    byte_to_class: &[u8; 256],
    transitions: &[TdfaStateId],
    nc: usize,
    s: usize,
) -> Vec<(u8, u8)> {
    let is_self = |b: usize| transitions[s * nc + byte_to_class[b] as usize] == s as u32;
    let mut runs: Vec<(u8, u8)> = Vec::new();
    let mut b = 0usize;
    while b < 256 {
        if is_self(b) {
            let lo = b;
            while b + 1 < 256 && is_self(b + 1) {
                b += 1;
            }
            runs.push((lo as u8, b as u8));
        }
        b += 1;
    }
    runs
}

/// The complement of `runs` over the full byte range: the coalesced ranges on
/// which a peeled state *exits* its self-loop. `runs` must be sorted ascending
/// and non-overlapping (as produced by [`self_loop_runs`]).
// The Rust emitter tests the self set directly, so only the JIT needs this.
#[cfg_attr(not(feature = "tdfa-jit"), allow(dead_code))]
pub(crate) fn complement_runs(runs: &[(u8, u8)]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut next = 0u32;
    for &(lo, hi) in runs {
        if (lo as u32) > next {
            out.push((next as u8, lo - 1));
        }
        next = hi as u32 + 1;
    }
    if next <= 255 {
        out.push((next as u8, 255));
    }
    out
}

/// States reachable from the automaton's entry points, following live (non-dead)
/// transitions. Unreachable state blocks are pointed at by nothing, so the
/// drivers skip emitting them. Seeds from both starts, plus `extra_seed` — the
/// warm-start `post_state` the prologue can branch straight into, which the cold
/// starts might not reach.
pub(crate) fn reachable_states(tdfa: &Tdfa, extra_seed: Option<usize>) -> Vec<bool> {
    let nc = tdfa.num_classes();
    let num_states = tdfa.num_states();
    let transitions = tdfa.transitions();
    let mut seen = vec![false; num_states];
    let mut stack: Vec<usize> = Vec::new();
    let seeds = [Some(tdfa.start(0) as usize), Some(tdfa.start(1) as usize), extra_seed];
    for s in seeds.into_iter().flatten() {
        if s < num_states && !seen[s] {
            seen[s] = true;
            stack.push(s);
        }
    }
    while let Some(s) = stack.pop() {
        for &t in &transitions[s * nc..s * nc + nc] {
            let t = t as usize;
            if t != TDFA_DEAD_STATE as usize && t < num_states && !seen[t] {
                seen[t] = true;
                stack.push(t);
            }
        }
    }
    seen
}

/// Decide whether state `s`'s self-loop is worth peeling in the capture-free
/// tier, returning its self byte-runs if so. Worth it when: the self-set is a
/// handful of runs (so the inline test is cheap), the state has no `$`-accept
/// EOI landing pad (whose accept the peel doesn't emit), and either the state
/// uses the indirect `Table` dispatch (peel removes the class-table load +
/// indirect branch on the self path) or it's an accepting `Ranges` state (peel
/// hoists the per-byte accept record out of the loop).
pub(crate) fn peel_capture_free<L: Copy + Eq>(
    tdfa: &Tdfa,
    s: usize,
    has_eoi_pad: bool,
    plan: &Dispatch<L>,
) -> Option<Vec<(u8, u8)>> {
    if has_eoi_pad {
        return None;
    }
    let runs = self_loop_runs(tdfa.byte_to_class(), tdfa.transitions(), tdfa.num_classes(), s);
    if runs.is_empty() || runs.len() > RANGE_DISPATCH_THRESHOLD {
        return None;
    }
    let worth = matches!(plan, Dispatch::Table)
        || (tdfa.accepting()[s] && matches!(plan, Dispatch::Ranges { .. }));
    worth.then_some(runs)
}

/// Decide whether state `s`'s self-loop is worth peeling in the capture tier,
/// returning its self byte-runs and the position-stamp lanes its self edges
/// write. Peelable when every self edge carries the *same* move sequence made
/// purely of position stamps (`src == curpos`, so the mark value depends only
/// on the final `pos` — the peeled loop applies them once per scalar byte /
/// bulk advance instead of through a move stub per byte). Moveless self edges
/// are the `dsts = []` case of the same scheme. A self edge with a
/// mark-to-mark copy (order-sensitive) declines. (No EOI-pad gate here: the
/// capture tier declines `has_eoi_accepts` outright.) Worth it for a `Table`
/// state (sheds the class-table load + indirect branch), an accepting state
/// (hoists the accept record and, for fallback accepts, the whole mark-file
/// snapshot out of the loop), or a stamping loop (sheds the per-byte stub
/// bounce and enables bulk skips).
pub(crate) fn peel_capture<L: Copy + Eq>(
    tdfa: &Tdfa,
    s: usize,
    plan: &Dispatch<L>,
) -> Option<(Vec<(u8, u8)>, Vec<u16>)> {
    let nc = tdfa.num_classes();
    let transitions = tdfa.transitions();
    let trans_moves = tdfa.transition_moves();
    let curpos_idx = (tdfa.num_marks() + 1) as u32;
    let mut self_moves: Option<&[MoveOp]> = None;
    for c in 0..nc {
        let idx = s * nc + c;
        if transitions[idx] != s as u32 {
            continue;
        }
        let mv = &trans_moves[idx][..];
        if mv.iter().any(|m| m.src as u32 != curpos_idx) {
            return None; // mark-to-mark copies: not pure stamps
        }
        match self_moves {
            None => self_moves = Some(mv),
            Some(prev) => {
                if prev.iter().map(|m| (m.dst, m.src)).ne(mv.iter().map(|m| (m.dst, m.src))) {
                    return None; // self edges disagree on their moves
                }
            }
        }
    }
    let dsts: Vec<u16> = self_moves?.iter().map(|m| m.dst).collect();
    let runs = self_loop_runs(tdfa.byte_to_class(), transitions, nc, s);
    if runs.is_empty() || runs.len() > RANGE_DISPATCH_THRESHOLD {
        return None;
    }
    let worth = matches!(plan, Dispatch::Table) || tdfa.accepting()[s] || !dsts.is_empty();
    worth.then_some((runs, dsts))
}
