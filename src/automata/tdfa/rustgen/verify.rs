//! Emit the unrolled `__verify` function (both tiers).
//!
//! Mirrors the JIT's `emit_capture_free` / `emit_capture` drivers: each
//! reachable state becomes a `match state` arm holding its accept record, EOI
//! check, and per-byte dispatch; dominant self-loops are peeled into `while`
//! loops (LLVM vectorizes the byte-range membership test, replacing the JIT's
//! hand-rolled SIMD skip). Non-multiline `$` accepts record in the state's
//! EOI branch — the analog of the JIT's EOI landing pads. In the capture
//! tier, mark-file lanes become local variables (LLVM register-allocates
//! them), `MoveOp` sequences become inline assignments, and `finalize` is
//! unrolled per winning state. All lowering decisions come from the shared
//! [`plan`] module so the two backends agree.

use super::fmt_run;
use crate::automata::tdfa::plan::{self, Dispatch};
use crate::automata::tdfa::{MoveOp, TDFA_DEAD_STATE, Tdfa};
use crate::automata::tdfa_backend::PrefixSkip;
use core::fmt::Write;
use std::collections::HashMap;

/// A dispatch target in the emitted source: a state id to assign, or `Done`
/// (`break 'scan`).
#[derive(Copy, Clone, PartialEq, Eq)]
enum RTarget {
    State(u32),
    Done,
}

/// Emit `fn __verify(input, start, _caps) -> Option<(usize, usize)>` for a
/// capture-free automaton (caller has checked the tier gates). Returns the
/// match extent `start..end`; `_caps` is untouched (no capture groups).
pub(super) fn emit_capture_free(w: &mut String, tdfa: &Tdfa, skip: Option<PrefixSkip>) {
    let nc = tdfa.num_classes();
    let num_states = tdfa.num_states();
    let transitions = tdfa.transitions();
    let accepting = tdfa.accepting();
    let byte_to_class = tdfa.byte_to_class();

    let plans: Vec<Dispatch<RTarget>> = (0..num_states)
        .map(|s| {
            plan::analyze_dispatch(byte_to_class, RTarget::Done, |c| {
                let t = transitions[s * nc + c];
                if t == TDFA_DEAD_STATE {
                    RTarget::Done
                } else {
                    RTarget::State(t)
                }
            })
        })
        .collect();

    // A state carrying a non-multiline `$` accept (all `accepts` are exactly
    // that here, since `has_perbyte_guards` is false) records `acc = pos` in
    // its EOI branch — the analog of the JIT's EOI landing pads.
    let eoi_accept: Vec<bool> = (0..num_states)
        .map(|s| !tdfa.guards(s as u32).accepts.is_empty())
        .collect();
    let reachable = plan::reachable_states(tdfa, skip.map(|s| s.post_state as usize));
    let peel: Vec<Option<Vec<(u8, u8)>>> = (0..num_states)
        .map(|s| {
            if !reachable[s] {
                return None;
            }
            plan::peel_capture_free(tdfa, s, eoi_accept[s], &plans[s])
        })
        .collect();
    let needs_classes =
        (0..num_states).any(|s| reachable[s] && matches!(plans[s], Dispatch::Table));

    // `unused_mut` can fire for degenerate automata (e.g. a start state whose
    // arm never reassigns `state`); harmless in generated code.
    let _ = writeln!(w, "    #[allow(unused_mut)]");
    let _ = writeln!(
        w,
        "    fn __verify(\n        input: &[u8],\n        start: usize,\n        _caps: &mut [usize],\n    ) -> ::core::option::Option<(usize, usize)> {{"
    );
    if needs_classes {
        emit_class_table(w, byte_to_class);
    }
    let _ = writeln!(w, "        let len = input.len();");
    let _ = writeln!(w, "        let mut acc = usize::MAX;");
    // Entry: a warm start resumes past the prefilter-matched prefix (the skip
    // is mark-free by construction); a cold start dispatches on the anchor
    // context (non-multiline `^` can only fire at offset 0).
    match skip {
        Some(sk) => {
            let _ = writeln!(w, "        let mut pos = start + {};", sk.len);
            let _ = writeln!(w, "        let mut state: u32 = {};", sk.post_state);
        }
        None => {
            let _ = writeln!(w, "        let mut pos = start;");
            let (a, u) = (tdfa.start(0), tdfa.start(1));
            if a == u {
                let _ = writeln!(w, "        let mut state: u32 = {a};");
            } else {
                let _ = writeln!(
                    w,
                    "        let mut state: u32 = if start == 0 {{ {a} }} else {{ {u} }};"
                );
            }
        }
    }
    let _ = writeln!(w, "        'scan: loop {{");
    let _ = writeln!(w, "            match state {{");
    for s in 0..num_states {
        if !reachable[s] {
            continue;
        }
        emit_state_arm(w, s, tdfa, &plans[s], peel[s].as_deref(), eoi_accept[s], accepting[s]);
    }
    let _ = writeln!(w, "                _ => ::core::unreachable!(),");
    let _ = writeln!(w, "            }}");
    let _ = writeln!(w, "        }}");
    let _ = writeln!(w, "        if acc == usize::MAX {{");
    let _ = writeln!(w, "            ::core::option::Option::None");
    let _ = writeln!(w, "        }} else {{");
    let _ = writeln!(w, "            ::core::option::Option::Some((start, acc))");
    let _ = writeln!(w, "        }}");
    let _ = writeln!(w, "    }}");
}

/// One `match state` arm: accept record, EOI handling, byte fetch, dispatch —
/// or the peeled self-loop form when `peel` is set.
fn emit_state_arm(
    w: &mut String,
    s: usize,
    tdfa: &Tdfa,
    plan: &Dispatch<RTarget>,
    peel: Option<&[(u8, u8)]>,
    eoi_accept: bool,
    accepting: bool,
) {
    let _ = writeln!(w, "                {s} => {{");
    if let Some(runs) = peel {
        // Peeled self-loop: consume the whole self-run, record the accept once
        // at the exit (covers zero self bytes: `acc` is then the entry `pos`),
        // then dispatch the exit byte through the regular tail. `peel` is
        // gated off for `$`-accept states, so EOI here is a plain stop.
        let pat = runs.iter().map(|&(lo, hi)| fmt_run(lo, hi)).collect::<Vec<_>>().join(" | ");
        let _ = writeln!(w, "                    while pos < len && matches!(input[pos], {pat}) {{");
        let _ = writeln!(w, "                        pos += 1;");
        let _ = writeln!(w, "                    }}");
        if accepting {
            let _ = writeln!(w, "                    acc = pos;");
        }
        let _ = writeln!(w, "                    if pos >= len {{");
        let _ = writeln!(w, "                        break 'scan;");
        let _ = writeln!(w, "                    }}");
        let _ = writeln!(w, "                    let b = input[pos];");
        let _ = writeln!(w, "                    pos += 1;");
        emit_dispatch(w, s, tdfa, plan);
        let _ = writeln!(w, "                }}");
        return;
    }
    if accepting {
        let _ = writeln!(w, "                    acc = pos;");
    }
    if matches!(plan, Dispatch::AllDone) {
        // Every byte dead-ends: no fetch. A `$` accept still records at EOI.
        if eoi_accept && !accepting {
            let _ = writeln!(w, "                    if pos >= len {{");
            let _ = writeln!(w, "                        acc = pos;");
            let _ = writeln!(w, "                    }}");
        }
        let _ = writeln!(w, "                    break 'scan;");
        let _ = writeln!(w, "                }}");
        return;
    }
    if eoi_accept && !accepting {
        let _ = writeln!(w, "                    if pos >= len {{");
        let _ = writeln!(w, "                        acc = pos;");
        let _ = writeln!(w, "                        break 'scan;");
        let _ = writeln!(w, "                    }}");
    } else {
        let _ = writeln!(w, "                    if pos >= len {{");
        let _ = writeln!(w, "                        break 'scan;");
        let _ = writeln!(w, "                    }}");
    }
    let _ = writeln!(w, "                    let b = input[pos];");
    let _ = writeln!(w, "                    pos += 1;");
    emit_dispatch(w, s, tdfa, plan);
    let _ = writeln!(w, "                }}");
}

/// The dispatch tail: assign the next state from the fetched byte `b` (the
/// caller has already advanced `pos`). Ranges match on the raw byte; Table
/// matches on the byte class. `Done` targets `break 'scan`.
fn emit_dispatch(w: &mut String, s: usize, tdfa: &Tdfa, plan: &Dispatch<RTarget>) {
    match plan {
        Dispatch::AllDone => {
            // Only reachable as a peeled state's exit tail; the exit byte
            // dead-ends by definition.
            let _ = writeln!(w, "                    break 'scan;");
        }
        Dispatch::Ranges { runs, default } => {
            if runs.is_empty() {
                // Unconditional: every byte goes to `default`.
                let _ = writeln!(w, "                    state = {};", target_expr(default));
                return;
            }
            let _ = writeln!(w, "                    state = match b {{");
            // Group same-target runs into one arm, first-appearance order.
            let mut groups: Vec<(RTarget, Vec<String>)> = Vec::new();
            for &(lo, hi, t) in runs {
                let pat = fmt_run(lo, hi);
                match groups.iter_mut().find(|(g, _)| *g == t) {
                    Some((_, pats)) => pats.push(pat),
                    None => groups.push((t, vec![pat])),
                }
            }
            for (t, pats) in &groups {
                let _ = writeln!(
                    w,
                    "                        {} => {},",
                    pats.join(" | "),
                    target_expr(t)
                );
            }
            let _ = writeln!(w, "                        _ => {},", target_expr(default));
            let _ = writeln!(w, "                    }};");
        }
        Dispatch::Table => {
            let nc = tdfa.num_classes();
            let transitions = tdfa.transitions();
            // Group byte classes by target, first-appearance order; the widest
            // group becomes the `_` arm (fewest listed classes).
            let mut groups: Vec<(RTarget, Vec<usize>)> = Vec::new();
            for c in 0..nc {
                let t = transitions[s * nc + c];
                let t = if t == TDFA_DEAD_STATE {
                    RTarget::Done
                } else {
                    RTarget::State(t)
                };
                match groups.iter_mut().find(|(g, _)| *g == t) {
                    Some((_, cs)) => cs.push(c),
                    None => groups.push((t, vec![c])),
                }
            }
            let default_idx = groups
                .iter()
                .enumerate()
                .max_by_key(|(i, (_, cs))| (cs.len(), core::cmp::Reverse(*i)))
                .map(|(i, _)| i)
                .expect("state has at least one class");
            if groups.len() == 1 {
                let _ = writeln!(
                    w,
                    "                    state = {};",
                    target_expr(&groups[0].0)
                );
                return;
            }
            let _ = writeln!(w, "                    state = match __CLASSES[b as usize] {{");
            for (i, (t, cs)) in groups.iter().enumerate() {
                if i == default_idx {
                    continue;
                }
                let pats = cs.iter().map(usize::to_string).collect::<Vec<_>>().join(" | ");
                let _ = writeln!(w, "                        {} => {},", pats, target_expr(t));
            }
            let _ = writeln!(
                w,
                "                        _ => {},",
                target_expr(&groups[default_idx].0)
            );
            let _ = writeln!(w, "                    }};");
        }
    }
}

/// A target as a `state = match` arm value: the state id, or `break 'scan`.
fn target_expr(t: &RTarget) -> String {
    match t {
        RTarget::State(id) => id.to_string(),
        RTarget::Done => "break 'scan".to_string(),
    }
}

/// A stub verify fn for the literal-only strategies (`WholeLiteral` /
/// `MultiLiteral`): their prefilter span IS the match, so the driver never
/// calls this.
pub(super) fn emit_stub(w: &mut String) {
    let _ = writeln!(
        w,
        "    fn __verify(\n        _input: &[u8],\n        _start: usize,\n        _caps: &mut [usize],\n    ) -> ::core::option::Option<(usize, usize)> {{"
    );
    let _ = writeln!(w, "        ::core::option::Option::None");
    let _ = writeln!(w, "    }}");
}

// ---------------------------------------------------------------------------
// Capture tier
// ---------------------------------------------------------------------------

/// A capture-tier dispatch target: a plain state, a move edge (the analog of
/// the JIT's deduplicated move stubs — same (moves, target) key, so arms
/// coalesce identically), or `Done`.
#[derive(Copy, Clone, PartialEq, Eq)]
enum CTarget {
    State(u32),
    Stub(u32),
    Done,
}

/// The mark file a capture-tier verify manipulates, lowered to one local
/// array: `m[i]` per mark lane, `tmp` for the cycle-breaking scratch lane,
/// `pos` for the current-position lane, and `usize::MAX` for the clear lane.
/// Every index is an emit-time constant, so LLVM's SROA splits `m` into the
/// same SSA values individual locals would produce.
struct MarkFile {
    num_marks: usize,
}

impl MarkFile {
    fn clear_lane(&self) -> usize {
        self.num_marks
    }
    fn curpos_lane(&self) -> usize {
        self.num_marks + 1
    }
    fn scratch_lane(&self) -> usize {
        self.num_marks + 2
    }

    /// A lane as a *read* expression.
    fn read(&self, lane: usize) -> String {
        if lane < self.num_marks {
            format!("m[{lane}]")
        } else if lane == self.clear_lane() {
            "usize::MAX".to_string()
        } else if lane == self.curpos_lane() {
            "pos".to_string()
        } else if lane == self.scratch_lane() {
            "tmp".to_string()
        } else {
            unreachable!("mark lane {lane} out of range")
        }
    }

    /// A lane as a *write* destination. Only real marks and the scratch lane
    /// are ever written by compiled moves.
    fn write(&self, lane: usize) -> String {
        if lane < self.num_marks {
            format!("m[{lane}]")
        } else if lane == self.scratch_lane() {
            "tmp".to_string()
        } else {
            unreachable!("compiled moves never write lane {lane}")
        }
    }

    /// One `MoveOp` list as assignment statements at `indent`.
    fn move_stmts(&self, w: &mut String, indent: &str, moves: &[MoveOp]) {
        for mv in moves {
            let _ = writeln!(
                w,
                "{indent}{} = {};",
                self.write(mv.dst as usize),
                self.read(mv.src as usize)
            );
        }
    }
}

/// Shared context for the capture-tier emitters: the automaton, the lowered
/// mark file, and the move-edge (stub) tables.
struct CapCtx<'a> {
    tdfa: &'a Tdfa,
    marks: MarkFile,
    /// Stub id per `state * nc + class` edge (`None` = moveless or dead).
    stub: Vec<Option<u32>>,
    /// Representative edge index per stub id.
    stub_reps: Vec<usize>,
}

/// Emit `fn __verify` for the anchored-captures tier (caller has checked the
/// tier gates: compiled moves available, no per-byte guards, no `$` accepts,
/// states/marks within the AoT caps). Handles both fixed-start automata and
/// the unanchored `Scan` automaton (whose `.*?` prefix stamps the match
/// start, read back in the inlined finalize).
pub(super) fn emit_capture(w: &mut String, tdfa: &Tdfa, skip: Option<PrefixSkip>) {
    let nc = tdfa.num_classes();
    let num_states = tdfa.num_states();
    let transitions = tdfa.transitions();
    let trans_moves = tdfa.transition_moves();
    let accepting = tdfa.accepting();
    let fallback = tdfa.accept_fallback();
    let byte_to_class = tdfa.byte_to_class();
    let marks = MarkFile {
        num_marks: tdfa.num_marks(),
    };
    let num_caps = 2 * tdfa.num_capture_groups();

    // Move-edge dedup, exactly like the JIT's stubs: edges sharing the same
    // (moves, target) get one id, assigned in (state, class) scan order —
    // deterministic. Arms with the same id coalesce in the dispatch match.
    let mut stub: Vec<Option<u32>> = vec![None; num_states * nc];
    let mut stub_map: HashMap<(Vec<(u16, u16)>, u32), u32> = HashMap::new();
    // Representative edge index per stub id, for reading its moves/target.
    let mut stub_reps: Vec<usize> = Vec::new();
    for s in 0..num_states {
        for c in 0..nc {
            let idx = s * nc + c;
            let t = transitions[idx];
            if t == TDFA_DEAD_STATE || trans_moves[idx].is_empty() {
                continue;
            }
            let key = (
                trans_moves[idx].iter().map(|m| (m.dst, m.src)).collect::<Vec<_>>(),
                t,
            );
            let id = *stub_map.entry(key).or_insert_with(|| {
                stub_reps.push(idx);
                (stub_reps.len() - 1) as u32
            });
            stub[idx] = Some(id);
        }
    }

    let plans: Vec<Dispatch<CTarget>> = (0..num_states)
        .map(|s| {
            plan::analyze_dispatch(byte_to_class, CTarget::Done, |c| {
                let idx = s * nc + c;
                let t = transitions[idx];
                if t == TDFA_DEAD_STATE {
                    CTarget::Done
                } else if let Some(id) = stub[idx] {
                    CTarget::Stub(id)
                } else {
                    CTarget::State(t)
                }
            })
        })
        .collect();
    let reachable = plan::reachable_states(tdfa, skip.map(|s| s.post_state as usize));
    let peel: Vec<Option<(Vec<(u8, u8)>, Vec<u16>)>> = (0..num_states)
        .map(|s| {
            if !reachable[s] {
                return None;
            }
            plan::peel_capture(tdfa, s, &plans[s])
        })
        .collect();
    let needs_classes =
        (0..num_states).any(|s| reachable[s] && matches!(plans[s], Dispatch::Table));

    // Whether any emitted move touches the cycle-breaking scratch lane, and
    // whether any reachable accept is a fallback (needing the snapshot array).
    let mut uses_tmp = false;
    let mut any_fallback = false;
    {
        let mut check_moves = |mvs: &[MoveOp]| {
            uses_tmp |= mvs
                .iter()
                .any(|mv| mv.dst as usize == marks.scratch_lane() || mv.src as usize == marks.scratch_lane());
        };
        check_moves(tdfa.entry_moves(0));
        check_moves(tdfa.entry_moves(1));
        for s in 0..num_states {
            if !reachable[s] {
                continue;
            }
            for c in 0..nc {
                let idx = s * nc + c;
                if stub[idx].is_some() {
                    check_moves(&trans_moves[idx]);
                }
            }
            any_fallback |= accepting[s] && fallback[s];
        }
    }

    let _ = writeln!(w, "    #[allow(unused_mut, unused_assignments)]");
    // A zero-group pattern can still land in this tier (unanchored Scan);
    // nothing then reads or fills `caps`, so underscore it.
    let caps_name = if num_caps > 0 { "caps" } else { "_caps" };
    let _ = writeln!(
        w,
        "    fn __verify(\n        input: &[u8],\n        start: usize,\n        {caps_name}: &mut [usize],\n    ) -> ::core::option::Option<(usize, usize)> {{"
    );
    if needs_classes {
        emit_class_table(w, byte_to_class);
    }
    let _ = writeln!(w, "        let len = input.len();");
    let _ = writeln!(w, "        let mut pos = start;");
    // The mark file and (for fallback accepts) its snapshot. All indices are
    // constant, so SROA scalarizes both; unused lanes just vanish.
    let _ = writeln!(w, "        let mut m = [usize::MAX; {}];", marks.num_marks);
    if uses_tmp {
        let _ = writeln!(w, "        let mut tmp = usize::MAX;");
    }
    if any_fallback {
        let _ = writeln!(w, "        let mut s = [usize::MAX; {}];", marks.num_marks);
    }
    let _ = writeln!(w, "        let mut acc_end = usize::MAX;");
    let _ = writeln!(w, "        let mut acc_state = u32::MAX;");
    // Entry moves, applied with `pos == start` as the current position
    // (mirrors `jit_prepare_marks`). Shared when both entries agree.
    let (ma, mu) = (tdfa.entry_moves(0), tdfa.entry_moves(1));
    let same_entry = ma.iter().map(|m| (m.dst, m.src)).eq(mu.iter().map(|m| (m.dst, m.src)));
    if same_entry {
        marks.move_stmts(w, "        ", ma);
    } else {
        let _ = writeln!(w, "        if start == 0 {{");
        marks.move_stmts(w, "            ", ma);
        let _ = writeln!(w, "        }} else {{");
        marks.move_stmts(w, "            ", mu);
        let _ = writeln!(w, "        }}");
    }
    match skip {
        Some(sk) => {
            let _ = writeln!(w, "        pos += {};", sk.len);
            let _ = writeln!(w, "        let mut state: u32 = {};", sk.post_state);
        }
        None => {
            let (a, u) = (tdfa.start(0), tdfa.start(1));
            if a == u {
                let _ = writeln!(w, "        let mut state: u32 = {a};");
            } else {
                let _ = writeln!(
                    w,
                    "        let mut state: u32 = if start == 0 {{ {a} }} else {{ {u} }};"
                );
            }
        }
    }
    let ctx = CapCtx {
        tdfa,
        marks,
        stub,
        stub_reps,
    };
    let _ = writeln!(w, "        'scan: loop {{");
    let _ = writeln!(w, "            match state {{");
    for s in 0..num_states {
        if !reachable[s] {
            continue;
        }
        emit_cap_state_arm(w, s, &ctx, &plans[s], peel[s].as_ref(), fallback[s]);
    }
    let _ = writeln!(w, "                _ => ::core::unreachable!(),");
    let _ = writeln!(w, "            }}");
    let _ = writeln!(w, "        }}");
    // Inlined finalize: pick the winning state's `finals` row, reading live
    // marks (or the snapshot for fallback accepts). Defaults mirror
    // `tdfa_backend::finalize`: unset full-match start → 0, end → acc_end.
    let _ = writeln!(w, "        if acc_state == u32::MAX {{");
    let _ = writeln!(w, "            return ::core::option::Option::None;");
    let _ = writeln!(w, "        }}");
    if num_caps > 0 {
        let _ = writeln!(w, "        for c in caps.iter_mut() {{");
        let _ = writeln!(w, "            *c = usize::MAX;");
        let _ = writeln!(w, "        }}");
    }
    let _ = writeln!(w, "        match acc_state {{");
    for s in 0..num_states {
        if !reachable[s] || !accepting[s] {
            continue;
        }
        emit_finalize_arm(w, s, tdfa, &ctx.marks, num_caps, fallback[s]);
    }
    let _ = writeln!(w, "            _ => ::core::unreachable!(),");
    let _ = writeln!(w, "        }}");
    let _ = writeln!(w, "    }}");
}

/// The mark lanes state `s`'s finals read.
fn finals_lanes(tdfa: &Tdfa, s: usize) -> Vec<usize> {
    use crate::automata::tdfa::MarkValue;
    tdfa.finals()[s]
        .iter()
        .map(|cmd| {
            let MarkValue::Copy(src) = cmd.src else {
                unreachable!("finals never use CurrentPos")
            };
            src.0 as usize
        })
        .collect()
}

/// The accept record for capture-tier accepting state `s`: `(acc_end,
/// acc_state)` plus, for a fallback accept, an eager snapshot of the mark
/// lanes its finals read (they may be clobbered before scan end).
fn emit_cap_accept(w: &mut String, indent: &str, s: usize, tdfa: &Tdfa, is_fallback: bool) {
    let _ = writeln!(w, "{indent}acc_end = pos;");
    let _ = writeln!(w, "{indent}acc_state = {s};");
    if is_fallback {
        let mut lanes = finals_lanes(tdfa, s);
        lanes.sort_unstable();
        lanes.dedup();
        for lane in lanes {
            if lane < tdfa.num_marks() {
                let _ = writeln!(w, "{indent}s[{lane}] = m[{lane}];");
            }
        }
    }
}

/// One capture-tier `match state` arm.
fn emit_cap_state_arm(
    w: &mut String,
    s: usize,
    ctx: &CapCtx<'_>,
    plan: &Dispatch<CTarget>,
    peel: Option<&(Vec<(u8, u8)>, Vec<u16>)>,
    is_fallback: bool,
) {
    let tdfa = ctx.tdfa;
    let marks = &ctx.marks;
    let accepting = tdfa.accepting()[s];
    let _ = writeln!(w, "                {s} => {{");
    if let Some((runs, dsts)) = peel {
        // Peeled self-loop. For a stamping loop the marks depend only on the
        // final `pos`, so stamp once at the exit — but only if at least one
        // self byte was consumed (a zero-byte visit must keep the entering
        // edge's marks). The accept (and fallback snapshot) then records with
        // the marks in sync, covering both the exit-byte and EOI paths.
        let pat = runs.iter().map(|&(lo, hi)| fmt_run(lo, hi)).collect::<Vec<_>>().join(" | ");
        if dsts.is_empty() {
            let _ = writeln!(w, "                    while pos < len && matches!(input[pos], {pat}) {{");
            let _ = writeln!(w, "                        pos += 1;");
            let _ = writeln!(w, "                    }}");
        } else {
            let _ = writeln!(w, "                    let p0 = pos;");
            let _ = writeln!(w, "                    while pos < len && matches!(input[pos], {pat}) {{");
            let _ = writeln!(w, "                        pos += 1;");
            let _ = writeln!(w, "                    }}");
            let _ = writeln!(w, "                    if pos != p0 {{");
            for &d in dsts.iter() {
                let _ = writeln!(w, "                        {} = pos;", marks.write(d as usize));
            }
            let _ = writeln!(w, "                    }}");
        }
        if accepting {
            emit_cap_accept(w, "                    ", s, tdfa, is_fallback);
        }
        let _ = writeln!(w, "                    if pos >= len {{");
        let _ = writeln!(w, "                        break 'scan;");
        let _ = writeln!(w, "                    }}");
        let _ = writeln!(w, "                    let b = input[pos];");
        let _ = writeln!(w, "                    pos += 1;");
        emit_cap_dispatch(w, s, ctx, plan);
        let _ = writeln!(w, "                }}");
        return;
    }
    if accepting {
        emit_cap_accept(w, "                    ", s, tdfa, is_fallback);
    }
    if matches!(plan, Dispatch::AllDone) {
        let _ = writeln!(w, "                    break 'scan;");
        let _ = writeln!(w, "                }}");
        return;
    }
    let _ = writeln!(w, "                    if pos >= len {{");
    let _ = writeln!(w, "                        break 'scan;");
    let _ = writeln!(w, "                    }}");
    let _ = writeln!(w, "                    let b = input[pos];");
    let _ = writeln!(w, "                    pos += 1;");
    emit_cap_dispatch(w, s, ctx, plan);
    let _ = writeln!(w, "                }}");
}

/// The capture-tier dispatch tail: a statement `match` whose arms inline a
/// move edge's assignments before the state assignment (the JIT's move stub,
/// inlined at the arm). `pos` is already advanced, so a `curpos` move source
/// reads the position just past the consumed byte — same as the stub order.
fn emit_cap_dispatch(w: &mut String, s: usize, ctx: &CapCtx<'_>, plan: &Dispatch<CTarget>) {
    let tdfa = ctx.tdfa;
    let arm_body = |w: &mut String, t: &CTarget, indent: &str| match t {
        CTarget::Done => {
            let _ = writeln!(w, "{indent}break 'scan;");
        }
        CTarget::State(id) => {
            let _ = writeln!(w, "{indent}state = {id};");
        }
        CTarget::Stub(id) => {
            let idx = ctx.stub_reps[*id as usize];
            ctx.marks.move_stmts(w, indent, &tdfa.transition_moves()[idx]);
            let _ = writeln!(w, "{indent}state = {};", tdfa.transitions()[idx]);
        }
    };
    match plan {
        Dispatch::AllDone => {
            let _ = writeln!(w, "                    break 'scan;");
        }
        Dispatch::Ranges { runs, default } => {
            if runs.is_empty() {
                arm_body(w, default, "                    ");
                return;
            }
            let _ = writeln!(w, "                    match b {{");
            let mut groups: Vec<(CTarget, Vec<String>)> = Vec::new();
            for &(lo, hi, t) in runs {
                let pat = fmt_run(lo, hi);
                match groups.iter_mut().find(|(g, _)| *g == t) {
                    Some((_, pats)) => pats.push(pat),
                    None => groups.push((t, vec![pat])),
                }
            }
            for (t, pats) in &groups {
                let _ = writeln!(w, "                        {} => {{", pats.join(" | "));
                arm_body(w, t, "                            ");
                let _ = writeln!(w, "                        }}");
            }
            let _ = writeln!(w, "                        _ => {{");
            arm_body(w, default, "                            ");
            let _ = writeln!(w, "                        }}");
            let _ = writeln!(w, "                    }}");
        }
        Dispatch::Table => {
            let nc = tdfa.num_classes();
            let transitions = tdfa.transitions();
            let mut groups: Vec<(CTarget, Vec<usize>)> = Vec::new();
            for c in 0..nc {
                let idx = s * nc + c;
                let t = transitions[idx];
                let t = if t == TDFA_DEAD_STATE {
                    CTarget::Done
                } else if let Some(id) = ctx.stub[idx] {
                    CTarget::Stub(id)
                } else {
                    CTarget::State(t)
                };
                match groups.iter_mut().find(|(g, _)| *g == t) {
                    Some((_, cs)) => cs.push(c),
                    None => groups.push((t, vec![c])),
                }
            }
            let default_idx = groups
                .iter()
                .enumerate()
                .max_by_key(|(i, (_, cs))| (cs.len(), core::cmp::Reverse(*i)))
                .map(|(i, _)| i)
                .expect("state has at least one class");
            if groups.len() == 1 {
                arm_body(w, &groups[0].0, "                    ");
                return;
            }
            let _ = writeln!(w, "                    match __CLASSES[b as usize] {{");
            for (i, (t, cs)) in groups.iter().enumerate() {
                if i == default_idx {
                    continue;
                }
                let pats = cs.iter().map(usize::to_string).collect::<Vec<_>>().join(" | ");
                let _ = writeln!(w, "                        {pats} => {{");
                arm_body(w, t, "                            ");
                let _ = writeln!(w, "                        }}");
            }
            let _ = writeln!(w, "                        _ => {{");
            arm_body(w, &groups[default_idx].0, "                            ");
            let _ = writeln!(w, "                        }}");
            let _ = writeln!(w, "                    }}");
        }
    }
}

/// One inlined-finalize arm for accepting state `s`: apply its `finals` row
/// (reading live `m*` or, for a fallback accept, the `s*` snapshot) and
/// produce the match extent.
fn emit_finalize_arm(
    w: &mut String,
    s: usize,
    tdfa: &Tdfa,
    marks: &MarkFile,
    num_caps: usize,
    is_fallback: bool,
) {
    use crate::automata::tdfa::MarkValue;
    // A fallback accept reads its snapshot lanes; others read live marks.
    let lane_expr = |lane: usize| -> String {
        if is_fallback && lane < marks.num_marks {
            format!("s[{lane}]")
        } else {
            marks.read(lane)
        }
    };
    let mut start_expr: Option<String> = None;
    let mut end_expr: Option<String> = None;
    let _ = writeln!(w, "            {s} => {{");
    for cmd in &tdfa.finals()[s] {
        let MarkValue::Copy(src) = cmd.src else {
            unreachable!("finals never use CurrentPos")
        };
        let val = lane_expr(src.0 as usize);
        match cmd.tag as usize {
            0 => start_expr = Some(val),
            1 => end_expr = Some(val),
            tag => {
                let idx = tag - 2;
                // Sentinel tags (ProgressSince) exceed the capture range.
                if idx < num_caps {
                    let _ = writeln!(w, "                caps[{idx}] = {val};");
                }
            }
        }
    }
    match &start_expr {
        Some(e) => {
            let _ = writeln!(w, "                let fs = {e};");
        }
        None => {
            let _ = writeln!(w, "                let fs = usize::MAX;");
        }
    }
    match &end_expr {
        Some(e) => {
            let _ = writeln!(w, "                let fe = {e};");
        }
        None => {
            let _ = writeln!(w, "                let fe = usize::MAX;");
        }
    }
    let _ = writeln!(w, "                ::core::option::Option::Some((");
    let _ = writeln!(w, "                    if fs == usize::MAX {{ 0 }} else {{ fs }},");
    let _ = writeln!(w, "                    if fe == usize::MAX {{ acc_end }} else {{ fe }},");
    let _ = writeln!(w, "                ))");
    let _ = writeln!(w, "            }}");
}


/// `static __CLASSES: [u8; 256] = [...];` — the shared byte-class table, 16
/// bytes per row.
fn emit_class_table(w: &mut String, byte_to_class: &[u8; 256]) {
    let _ = writeln!(w, "        static __CLASSES: [u8; 256] = [");
    for row in byte_to_class.chunks(16) {
        let _ = write!(w, "            ");
        for (i, c) in row.iter().enumerate() {
            if i > 0 {
                let _ = write!(w, " ");
            }
            let _ = write!(w, "{c},");
        }
        let _ = writeln!(w);
    }
    let _ = writeln!(w, "        ];");
}
