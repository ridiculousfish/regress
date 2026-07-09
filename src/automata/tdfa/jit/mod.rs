//! TDFA JIT: a hand-rolled native code generator that specializes a built
//! [`Tdfa`](crate::automata::tdfa::Tdfa) into machine code.
//!
//! Instead of the byte-loop interpreter in
//! [`tdfa_backend`](crate::automata::tdfa_backend), which re-reads the
//! transition/class/accept tables for every input byte, the JIT bakes the
//! automaton into control flow: each state becomes a code block, transitions
//! become branches/jump-tables, accepts become inline stores, and the hot
//! values (`pos`, `end`, `input`, `last_accept`) are pinned in registers.
//!
//! Tiers supported: the capture-free fast path (no marks, `start_fixed`, no
//! conditionals/anchor-alts) and the anchored capture path (per-transition
//! `MoveOp` stores + `finalize`, no fallback accepts). Automata outside these
//! (unanchored scan, `$`-conditionals, multiline-`^` alts, fallback accepts)
//! return an error so the caller falls back to the interpreter backend.

// JIT codegen, executable memory, and calling generated code are inherently
// unsafe, so the backend is fundamentally incompatible with `prohibit-unsafe`.
#[cfg(feature = "prohibit-unsafe")]
compile_error!("feature `tdfa-jit` requires unsafe code and is incompatible with `prohibit-unsafe`");

mod asm;
mod mem;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

use crate::automata::nfa_backend::NfaMatch;
use crate::automata::tdfa::{TDFA_DEAD_STATE, Tdfa};
use crate::automata::tdfa_backend::{self, PrefixSkip, Scratch};
use asm::{Assembler, Label};
use mem::ExecBuffer;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// Capture-free C ABI: `(input, len, start) -> match-end offset, or usize::MAX
/// for no match`. The match is `start..end` with no captures.
type CaptureFreeFn = extern "C" fn(*const u8, usize, usize) -> usize;

/// The capture tier's return value, in the ABI's two integer return registers
/// (x86-64 `rax:rdx`, aarch64 `x0:x1`): a ≤16-byte all-integer `repr(C)` aggregate
/// is returned in registers, no hidden `sret` pointer. Splitting `end` off into
/// its own eightbyte is what lets the mark file (and `end`) be full `usize` width,
/// so the capture tier is no longer capped at 4 GiB haystacks.
#[repr(C)]
struct CaptureResult {
    /// Match end offset (only meaningful when `meta != u64::MAX`).
    end: usize,
    /// `u64::MAX` for no match; else the winning state id in the low bits with the
    /// [`SNAPSHOT_FLAG`] snapshot bit.
    meta: u64,
}

/// Capture C ABI: `(input, len, start, marks, best_snap) -> CaptureResult`. The
/// mark file `marks` (u64 lanes) is prepared by the caller (reset + entry
/// commands) and filled in place by the generated code; `best_snap` receives an
/// eager copy of the marks on a fallback accept. See [`CaptureResult`] for the
/// return encoding.
type CaptureFn =
    extern "C" fn(*const u8, usize, usize, *mut usize, *mut usize) -> CaptureResult;

/// Bit of `CaptureResult::meta`: set when the winning marks live in `best_snap`
/// (a fallback accept), clear when live in `marks`. Bit 31 (above every valid
/// state id, which the codegen caps at `u16::MAX`).
const SNAPSHOT_FLAG: u64 = 1 << 31;

/// Which tier a `Tdfa` compiled to (picks the generated function's ABI).
#[derive(Clone, Copy)]
enum Tier {
    CaptureFree,
    Capture,
}

/// Why a [`Tdfa`] could not be JIT-compiled. The caller falls back to the
/// interpreter backend on any of these.
#[derive(Debug)]
pub enum JitError {
    /// The automaton uses a feature this JIT tier doesn't emit yet (captures,
    /// conditionals, anchor alts, oversized mark file, …).
    Unsupported(&'static str),
    /// This target architecture has no encoder yet.
    UnsupportedArch,
    /// Allocating or protecting executable memory failed.
    Memory(region::Error),
}

impl From<region::Error> for JitError {
    fn from(e: region::Error) -> Self {
        JitError::Memory(e)
    }
}

impl core::fmt::Display for JitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            JitError::Unsupported(why) => write!(f, "TDFA JIT unsupported: {why}"),
            JitError::UnsupportedArch => write!(f, "TDFA JIT: no encoder for this architecture"),
            JitError::Memory(e) => write!(f, "TDFA JIT executable memory: {e}"),
        }
    }
}

/// The generated function plus its ABI tier.
enum Compiled {
    CaptureFree(CaptureFreeFn),
    Capture(CaptureFn),
}

/// A [`Tdfa`] compiled to native code. Holds the executable mapping and a
/// pointer into it; the mapping is freed on drop, so this must outlive every
/// [`run`](Self::run) call. Build with [`compile`](Self::compile).
#[derive(Debug)]
pub struct JittedTdfa {
    /// Keeps the executable mapping alive; `compiled` points into it.
    _buf: ExecBuffer,
    compiled: Compiled,
}

impl core::fmt::Debug for Compiled {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Compiled::CaptureFree(_) => f.write_str("CaptureFree"),
            Compiled::Capture(_) => f.write_str("Capture"),
        }
    }
}

impl JittedTdfa {
    /// Compile `tdfa` to native code for the host architecture, or return why
    /// it can't be (so the caller falls back to the interpreter). Supported:
    /// the capture-free tier and the anchored capture tier (no conditionals /
    /// anchor alts / fallback accepts).
    pub(crate) fn compile(tdfa: &Tdfa, skip: Option<PrefixSkip>) -> Result<Self, JitError> {
        let (tier, code, _data_start) = compile_code(tdfa, skip)?;
        let buf = ExecBuffer::new(&code)?;
        // SAFETY: `buf` is kept alive in the returned struct; the generated code
        // implements exactly the ABI selected by `tier` (see `asm` register map).
        let entry = buf.entry_ptr();
        let compiled = match tier {
            // SAFETY: the generated code implements exactly the ABI selected by
            // `tier` (see the `asm` register map); `buf` outlives `compiled`.
            Tier::CaptureFree => {
                Compiled::CaptureFree(unsafe { core::mem::transmute::<*const u8, CaptureFreeFn>(entry) })
            }
            Tier::Capture => {
                Compiled::Capture(unsafe { core::mem::transmute::<*const u8, CaptureFn>(entry) })
            }
        };
        Ok(Self {
            _buf: buf,
            compiled,
        })
    }

    /// Run the anchored automaton from byte offset `start`, returning the match
    /// (range + captures) or `None`. `tdfa` is the automaton this was compiled
    /// from (used by the capture path for entry/finalize); `scratch` is the
    /// reusable mark buffer.
    pub(crate) fn run(
        &self,
        tdfa: &Tdfa,
        input: &[u8],
        start: usize,
        scratch: &mut Scratch<usize>,
    ) -> Option<NfaMatch> {
        debug_assert!(start <= input.len());
        match self.compiled {
            Compiled::CaptureFree(f) => {
                let end = f(input.as_ptr(), input.len(), start);
                (end != usize::MAX).then(|| NfaMatch {
                    range: start..end,
                    captures: Vec::new(),
                })
            }
            Compiled::Capture(f) => {
                // The capture tier writes a u64 mark file and returns a full-width
                // `end`, so it handles any haystack size (no width fallback).
                tdfa_backend::jit_prepare_marks(tdfa, scratch, start);
                let r = f(
                    input.as_ptr(),
                    input.len(),
                    start,
                    scratch.src_buf_mut_ptr(),
                    scratch.best_snap_mut_ptr(),
                );
                if r.meta == u64::MAX {
                    return None;
                }
                let read_live = r.meta & SNAPSHOT_FLAG == 0;
                let state = (r.meta & 0x7FFF_FFFF) as u32;
                Some(tdfa_backend::jit_finalize(tdfa, state, scratch, r.end, read_live))
            }
        }
    }
}

/// Pick the tier a `Tdfa` compiles to. The capture tier (mark application +
/// finalize) is used whenever the match shape isn't a fixed `start..end`: user
/// captures, *or* an unanchored automaton whose `.*?` prefix stamps
/// `FULL_MATCH_START` mid-scan (`!start_fixed`). The capture-free tier is
/// reserved for anchored, capture-free automata.
fn select_tier(tdfa: &Tdfa) -> Tier {
    if tdfa.has_captures() || !tdfa.start_fixed() {
        Tier::Capture
    } else {
        Tier::CaptureFree
    }
}

/// Lower `tdfa` to machine code, picking the tier and dispatching to the
/// host-arch encoder. Errors with [`JitError::UnsupportedArch`] on
/// architectures without an encoder.
fn compile_code(tdfa: &Tdfa, skip: Option<PrefixSkip>) -> Result<(Tier, Vec<u8>, usize), JitError> {
    let tier = select_tier(tdfa);
    #[cfg(target_arch = "aarch64")]
    let (code, data_start) = lower::<aarch64::Aarch64Asm>(tdfa, tier, skip)?;
    #[cfg(target_arch = "x86_64")]
    let (code, data_start) = lower::<x86_64::X86_64Asm>(tdfa, tier, skip)?;
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let (code, data_start) = {
        let _ = (tdfa, tier, skip);
        return Err(JitError::UnsupportedArch);
    };
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    Ok((tier, code, data_start))
}

/// Parse a branch instruction's PC-relative target address from its operand
/// string — the last `0x..` / `#0x..` token. Returns `None` for non-branches and
/// indirect branches (`br x6`, no static target). Detection is by mnemonic,
/// which is reliable for the JIT's small, known instruction set (aarch64 `b` /
/// `b.<cc>` / `cbz`; x86 `j*`).
#[cfg(feature = "tdfa-jit-dump")]
fn branch_target(mnemonic: &str, op_str: &str) -> Option<u64> {
    let is_branch = mnemonic == "b"
        || mnemonic.starts_with("b.")
        || mnemonic == "cbz"
        || mnemonic.starts_with('j');
    if !is_branch {
        return None;
    }
    op_str
        .rsplit([' ', ','])
        .filter_map(|tok| {
            let h = tok.trim().trim_start_matches('#').strip_prefix("0x")?;
            u64::from_str_radix(h, 16).ok()
        })
        .next()
}

/// Disassemble the machine code the JIT would generate for `tdfa` (a dev aid;
/// feature `tdfa-jit-dump`). Code is shown one instruction per line with
/// `L_xxxx:` labels at every branch target (and branch operands rewritten to use
/// them); the trailing class table + jump tables are shown as a hex dump rather
/// than decoded as bogus instructions. Errors exactly as
/// [`JittedTdfa::compile`] does (e.g. unsupported pattern / arch).
#[cfg(feature = "tdfa-jit-dump")]
pub fn disassemble(tdfa: &Tdfa) -> Result<String, JitError> {
    use core::fmt::Write as _;
    use std::collections::BTreeSet;

    let (tier, code, data_start) = compile_code(tdfa, None)?;
    let cs = host_capstone()?;
    let insns = cs
        .disasm_all(&code[..data_start], 0)
        .map_err(|_| JitError::Unsupported("capstone disassembly failed"))?;

    // Pass 1: collect branch targets so we can place labels at them.
    let mut targets: BTreeSet<u64> = BTreeSet::new();
    for insn in insns.iter() {
        if let Some(t) = branch_target(insn.mnemonic().unwrap_or(""), insn.op_str().unwrap_or("")) {
            targets.insert(t);
        }
    }
    let label = |a: u64| if a == 0 { "entry".to_string() } else { format!("L_{a:04x}") };

    let tier_name = match tier {
        Tier::CaptureFree => "capture-free",
        Tier::Capture => "capture",
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "; tier={tier_name} bytes={} (code={data_start}, data={})",
        code.len(),
        code.len() - data_start,
    );

    // Pass 2: print, with labels at targets and branch operands rewritten.
    for insn in insns.iter() {
        let addr = insn.address();
        if addr == 0 || targets.contains(&addr) {
            let _ = writeln!(out, "{}:", label(addr));
        }
        let mnem = insn.mnemonic().unwrap_or("");
        let op_str = insn.op_str().unwrap_or("");
        let ops = match branch_target(mnem, op_str) {
            // Rewrite the literal `#0x..`/`0x..` target to the label name.
            Some(t) => op_str
                .replace(&format!("#0x{t:x}"), &label(t))
                .replace(&format!("0x{t:x}"), &label(t)),
            None => op_str.to_string(),
        };
        let hex: Vec<String> = insn.bytes().iter().map(|b| format!("{b:02x}")).collect();
        let _ = writeln!(out, "  {addr:#06x}: {:<23} {mnem} {ops}", hex.join(" "));
    }

    // The class table + jump tables are data; dump them as bytes.
    if data_start < code.len() {
        let _ = writeln!(
            out,
            "; data: class table + jump tables ({} bytes)",
            code.len() - data_start,
        );
        for (i, chunk) in code[data_start..].chunks(16).enumerate() {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let _ = writeln!(out, "  {:#06x}: {}", data_start + i * 16, hex.join(" "));
        }
    }
    Ok(out)
}

#[cfg(all(feature = "tdfa-jit-dump", target_arch = "aarch64"))]
fn host_capstone() -> Result<capstone::Capstone, JitError> {
    use capstone::prelude::*;
    capstone::Capstone::new()
        .arm64()
        .mode(arch::arm64::ArchMode::Arm)
        .build()
        .map_err(|_| JitError::UnsupportedArch)
}

#[cfg(all(feature = "tdfa-jit-dump", target_arch = "x86_64"))]
fn host_capstone() -> Result<capstone::Capstone, JitError> {
    use capstone::prelude::*;
    capstone::Capstone::new()
        .x86()
        .mode(arch::x86::ArchMode::Mode64)
        .build()
        .map_err(|_| JitError::UnsupportedArch)
}

#[cfg(all(
    feature = "tdfa-jit-dump",
    not(any(target_arch = "aarch64", target_arch = "x86_64"))
))]
fn host_capstone() -> Result<capstone::Capstone, JitError> {
    Err(JitError::UnsupportedArch)
}

/// Dispatch to the tier's codegen driver.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
/// Resolve a [`PrefixSkip`] into the prologue's warm-entry `(post_block, len)`.
/// Declines (keeps the cold start) when `len` won't fit the prologue's `add`
/// immediate (12 bits) — real prefilter prefixes are far smaller, so this never
/// fires in practice; it's a guard against an oversize literal.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn warm_entry(skip: Option<PrefixSkip>, block: &[Label]) -> Option<(Label, usize)> {
    skip.filter(|s| s.len < 0x1000).map(|s| (block[s.post_state as usize], s.len))
}

fn lower<A: Assembler>(
    tdfa: &Tdfa,
    tier: Tier,
    skip: Option<PrefixSkip>,
) -> Result<(Vec<u8>, usize), JitError> {
    match tier {
        Tier::CaptureFree => emit_capture_free::<A>(tdfa, skip),
        Tier::Capture => emit_capture::<A>(tdfa, skip),
    }
}

/// Max number of byte-range compares before a state prefers the jump table.
/// Below this, a compare-chain on the raw byte (no class table, no jump-table
/// memory access) is cheaper; above it, the table's constant cost wins. Tunable.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
const RANGE_DISPATCH_THRESHOLD: usize = 8;

/// Max self-runs for which a peeled state gets the SIMD skip prelude. Bounds the
/// inline per-vector membership test (each run is a few vector ops); above this,
/// the scalar peel alone is used. Real wide self-loops (`.` `[^"]` `\w` `[a-z]`)
/// are well under this.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
const SIMD_MAX_SELF_RUNS: usize = 4;

/// How a state dispatches on the next input byte.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
enum Dispatch {
    /// Every byte dead-ends — branch straight to `done`, skipping the fetch.
    AllDone,
    /// Sparse: a compare-chain on raw byte ranges, falling through to `default`.
    Ranges {
        runs: Vec<(u8, u8, Label)>,
        default: Label,
    },
    /// Dense: load the byte class and indirect-branch through the jump table.
    Table,
}

/// Decide how a state dispatches. `target_of(class)` resolves a byte class to
/// the label it branches to (a state block, a capture move stub, or `done`).
/// Coalesces the per-byte targets into runs, picks the most-covered target as
/// the fall-through `default` (so e.g. `[^x]` tests only `x`), and chooses the
/// compare-chain when there are few enough runs, else the jump table.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn analyze_dispatch(
    byte_to_class: &[u8; 256],
    done: Label,
    target_of: impl Fn(usize) -> Label,
) -> Dispatch {
    let byte_target: Vec<Label> =
        (0..256).map(|b| target_of(byte_to_class[b] as usize)).collect();
    // Coalesce contiguous equal labels into runs.
    let mut runs: Vec<(u8, u8, Label)> = Vec::new();
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
    let mut coverage: Vec<(Label, usize)> = Vec::new();
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
    let nondefault: Vec<(u8, u8, Label)> =
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

/// Emit one state's per-byte dispatch through `A` given its [`Dispatch`] plan.
/// `eoi` is where the state jumps when input is exhausted: normally the shared
/// `done`, but a state carrying a non-multiline `$` accept points at its own
/// landing pad (which records the accept before falling into `done`). The EOI
/// check guards the `fetch_byte` fall-through, so the fetching plans always emit
/// it; an `AllDone` state branches straight to `done` and needs the check only
/// when it has a `$` accept to record (`eoi != done`).
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn emit_dispatch<A: Assembler>(asm: &mut A, plan: &Dispatch, jt: Label, done: Label, eoi: Label) {
    match plan {
        Dispatch::AllDone => {
            if eoi != done {
                asm.eoi_check(eoi);
            }
            asm.branch(done);
        }
        Dispatch::Ranges { runs, default } => {
            asm.eoi_check(eoi);
            asm.fetch_byte();
            asm.dispatch_byte_ranges(runs, *default);
        }
        Dispatch::Table => {
            asm.eoi_check(eoi);
            asm.fetch_byte();
            asm.classify();
            asm.dispatch(jt);
        }
    }
}

/// Coalesced byte ranges `[lo, hi]` on which state `s` self-loops
/// (`transitions[s][class(byte)] == s`). Empty when `s` has no self-transition.
/// These are the bytes the peeled hot loop tests inline before falling through
/// to the state's regular exit dispatch.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn self_loop_runs(
    byte_to_class: &[u8; 256],
    transitions: &[crate::automata::tdfa::TdfaStateId],
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

/// Emit only the dispatch *tail* of a state's plan (no `eoi_check`, no
/// `fetch_byte` — the caller has already loaded the byte and advanced `pos`).
/// Used on the peeled self-loop exit path, which reaches here only for bytes
/// that leave the state.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn emit_dispatch_tail<A: Assembler>(asm: &mut A, plan: &Dispatch, jt: Label, done: Label) {
    match plan {
        Dispatch::AllDone => asm.branch(done),
        Dispatch::Ranges { runs, default } => asm.dispatch_byte_ranges(runs, *default),
        Dispatch::Table => {
            asm.classify();
            asm.dispatch(jt);
        }
    }
}

/// Emit a peeled self-loop state. `loop_top` (== `block[s]`) is already bound by
/// the caller. Shape (test-before-advance so `pos` points at the exit byte on
/// the way out, keeping the accept exact):
///
/// ```text
/// loop_top:
///   eoi_check -> eoi_pad          ; (eoi_pad == done when not accepting)
///   load_byte ; advance(1)
///   <byte in self-set?> -> loop_top    ; raw-byte range tests, direct branch
///   ; --- exit fall-through: byte at pos-1 left the state ---
///   (if accepting) acc = pos - 1
///   <exit dispatch tail>          ; Ranges/Table over the state's plan
/// eoi_pad:                        ; (accepting only)
///   acc = pos                     ; == end
///   jmp done
/// ```
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn emit_peeled_self_loop<A: Assembler>(
    asm: &mut A,
    runs: &[(u8, u8)],
    loop_top: Label,
    accepting: bool,
    plan: &Dispatch,
    jt: Label,
    done: Label,
) {
    // Optional SIMD prelude: fast-skip the self-run 16 bytes at a time, then hand
    // off to the scalar loop for the last `< 16` bytes and the exit byte. Gated
    // to a few runs so the inline membership stays small; the scalar peel is the
    // fallback (and the whole thing is a no-op on arches without a SIMD path, in
    // which case `scalar_tail == loop_top`). The scalar self-test then loops to
    // `scalar_tail` rather than `loop_top`, so it doesn't re-run the SIMD prelude.
    let simd = runs.len() <= SIMD_MAX_SELF_RUNS;
    let scalar_tail = if simd { asm.fresh_label() } else { loop_top };
    if simd {
        asm.simd_self_skip(runs, scalar_tail);
        asm.bind(scalar_tail);
    }
    let eoi_pad = if accepting { asm.fresh_label() } else { done };
    asm.eoi_check(eoi_pad);
    asm.load_byte();
    asm.advance(1);
    // Self bytes branch back to the scalar loop; others fall through.
    // `dispatch_byte_ranges` appends a branch to its default, so bind `ft` right
    // after to elide it.
    let ft = asm.fresh_label();
    // Widest run first: the most common self byte then matches on the first
    // compare (e.g. `.`'s `[0x0e,0x7f]` covers letters/digits/space), instead of
    // falling through the narrow ranges first.
    let mut self_runs: Vec<(u8, u8, Label)> =
        runs.iter().map(|&(lo, hi)| (lo, hi, scalar_tail)).collect();
    self_runs.sort_by_key(|&(lo, hi, _)| core::cmp::Reverse(hi as u16 - lo as u16));
    asm.dispatch_byte_ranges(&self_runs, ft);
    asm.bind(ft);
    if accepting {
        asm.record_accept_prev(); // acc = pos - 1 (match ends before the exit byte)
    }
    emit_dispatch_tail(asm, plan, jt, done);
    if accepting {
        asm.bind(eoi_pad);
        asm.record_accept(); // acc = pos (== end)
        asm.branch(done);
    }
}

/// States reachable from the automaton's entry points, following live (non-dead)
/// transitions. Unreachable state blocks are pointed at by nothing, so the
/// driver skips emitting them. Seeds from both starts, plus `extra_seed` — the
/// warm-start `post_state` the prologue can branch straight into, which the cold
/// starts might not reach.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn reachable_states(tdfa: &Tdfa, extra_seed: Option<usize>) -> Vec<bool> {
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

/// The arch-independent codegen driver: walk `tdfa` and emit the capture-free
/// state machine through `A`. Each state becomes a code block (accept-on-entry,
/// EOI check, then per-state dispatch — a compare-chain on the raw byte for
/// sparse states, a jump table for dense ones). Dead transitions route to
/// `done`. Jump tables are emitted only for the states that use them.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
#[allow(clippy::needless_range_loop)] // state id indexes several parallel arrays
fn emit_capture_free<A: Assembler>(
    tdfa: &Tdfa,
    skip: Option<PrefixSkip>,
) -> Result<(Vec<u8>, usize), JitError> {
    // Capture-free tier only: no marks read back per byte, fixed start, and no
    // per-byte guards (multiline `^`/`$`, `\b`/`\B`). Non-multiline `$` accepts
    // are allowed and handled via the EOI landing pads below.
    if tdfa.has_captures() {
        return Err(JitError::Unsupported("captures"));
    }
    if !tdfa.start_fixed() {
        return Err(JitError::Unsupported("unanchored / start not fixed"));
    }
    // Per-byte guards (multiline `^`/`$`, `\b`/`\B`) still need inline
    // boundary-signature evaluation the codegen doesn't yet emit.
    // TODO(stage 4): compute the signature inline and emit the guard branches.
    // Non-multiline `$` accepts (`has_eoi_accepts` without `has_perbyte_guards`)
    // *are* handled below: they fire only at EOI, where their predicate is
    // trivially true, so each such state gets an EOI landing pad.
    if tdfa.has_perbyte_guards() {
        return Err(JitError::Unsupported("per-byte zero-width guards"));
    }

    let nc = tdfa.num_classes();
    let num_states = tdfa.num_states();
    let transitions = tdfa.transitions();
    let accepting = tdfa.accepting();
    let byte_to_class = tdfa.byte_to_class();
    let start_anchored = tdfa.start(0) as usize;
    let start_unanchored = tdfa.start(1) as usize;

    let mut asm = A::new();
    let classtab = asm.fresh_label();
    let done = asm.fresh_label();
    let block: Vec<Label> = (0..num_states).map(|_| asm.fresh_label()).collect();
    let jt: Vec<Label> = (0..num_states).map(|_| asm.fresh_label()).collect();

    // A state carrying a non-multiline `$` accept (all `accepts` are exactly that
    // here, since `has_perbyte_guards` is false) needs an EOI landing pad: on
    // reaching end-of-input in this state, record the accept, then fall into
    // `done`. States without a `$` accept jump straight to `done` at EOI.
    let eoi_stub: Vec<Option<Label>> = (0..num_states)
        .map(|s| (!tdfa.guards(s as u32).accepts.is_empty()).then(|| asm.fresh_label()))
        .collect();
    let eoi_target = |s: usize| eoi_stub[s].unwrap_or(done);

    // Decide each state's dispatch up front (also tells us which states need a
    // jump table).
    let plans: Vec<Dispatch> = (0..num_states)
        .map(|s| {
            analyze_dispatch(byte_to_class, done, |c| {
                let t = transitions[s * nc + c];
                if t == TDFA_DEAD_STATE {
                    done
                } else {
                    block[t as usize]
                }
            })
        })
        .collect();

    // Prologue + per-state code blocks. A prefix skip warm-starts past the
    // prefilter-matched prefix: resume in `post_state` at `pos + len` instead of
    // dispatching on the start state. `len` must fit the prologue's `add`
    // immediate (12 bits); real prefixes are tiny, so an oversize one just keeps
    // the cold start.
    // Only emit blocks reachable from the entry points (incl. the warm-start
    // post_state); unreachable state blocks are referenced by nothing.
    let reachable = reachable_states(tdfa, skip.map(|s| s.post_state as usize));
    // Decide whether state `s`'s self-loop is worth peeling, returning its self
    // byte-runs if so. Worth it when: the self-set is a handful of runs (so the
    // inline test is cheap), the state has no `$`-accept EOI landing pad (whose
    // accept the peel doesn't emit), and either the state uses the indirect
    // `Table` dispatch (peel removes the class-table load + indirect branch on
    // the self path) or it's an accepting `Ranges` state (peel hoists the
    // per-byte `record_accept` out of the loop).
    let should_peel = |s: usize| -> Option<Vec<(u8, u8)>> {
        if eoi_stub[s].is_some() {
            return None;
        }
        let runs = self_loop_runs(byte_to_class, transitions, nc, s);
        if runs.is_empty() || runs.len() > RANGE_DISPATCH_THRESHOLD {
            return None;
        }
        let worth = matches!(plans[s], Dispatch::Table)
            || (accepting[s] && matches!(plans[s], Dispatch::Ranges { .. }));
        worth.then_some(runs)
    };

    let warm = warm_entry(skip, &block);
    asm.prologue(classtab, block[start_anchored], block[start_unanchored], warm);
    for s in 0..num_states {
        if !reachable[s] {
            continue;
        }
        asm.bind(block[s]);
        // Peel a dominant self-loop when it pays: the hot self path becomes a
        // raw-byte range test + a direct backward branch, skipping the class
        // table and the indirect jump-table branch that a `Table` state would
        // otherwise pay on every self byte. See `should_peel`.
        if let Some(runs) = should_peel(s) {
            emit_peeled_self_loop(
                &mut asm,
                &runs,
                block[s],
                accepting[s],
                &plans[s],
                jt[s],
                done,
            );
            continue;
        }
        if accepting[s] {
            asm.record_accept();
        }
        emit_dispatch(&mut asm, &plans[s], jt[s], done, eoi_target(s));
    }
    // EOI landing pads: a `$`-accept state records the accept (`acc = pos`, which
    // at EOI is the match end) and falls into `done`. Only reachable states'
    // dispatch references these, so unreachable pads are dead — skip them.
    for s in 0..num_states {
        if let Some(stub) = eoi_stub[s].filter(|_| reachable[s]) {
            asm.bind(stub);
            asm.record_accept();
            asm.branch(done);
        }
    }
    asm.bind(done);
    asm.ret_done();

    // Data: shared class table, then a jump table for each dense state only.
    let data_start = asm.offset();
    asm.class_table(classtab, byte_to_class);
    let mut entries: Vec<Label> = Vec::with_capacity(nc);
    for s in 0..num_states {
        if !reachable[s] || !matches!(plans[s], Dispatch::Table) {
            continue;
        }
        entries.clear();
        let row = &transitions[s * nc..s * nc + nc];
        for &t in row {
            entries.push(if t == TDFA_DEAD_STATE {
                done
            } else {
                block[t as usize]
            });
        }
        asm.jump_table(jt[s], &entries);
    }
    // Arch-specific data pools (e.g. the SIMD skip's broadcast constants).
    asm.end_data();

    Ok((asm.finish(), data_start))
}

/// Largest mark-file width the JIT addresses, bounded so aarch64's scaled `ldr`
/// immediate (`imm12 * 4`) reaches every lane and the snapshot loop's `cmp`
/// immediate fits `imm12`. The interpreter handles bigger mark files; the JIT
/// just declines them.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
const JIT_MAX_MARK_LANES: usize = 4095;

/// Codegen driver for the **anchored capture tier**: like
/// [`emit_capture_free`], but threads the u32 mark file through (arg 3), applies
/// each transition's `MoveOp` sequence as an inlined move stub, and tracks the
/// winning `(end, state)` for the caller to `finalize`. Supported only when the
/// "read live registers at scan end" scheme is valid — i.e. no fallback accepts,
/// no `$`-conditionals or anchor alts, a fixed start, and a small-enough mark
/// file (see gating).
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
#[allow(clippy::needless_range_loop)] // state id indexes several parallel arrays
fn emit_capture<A: Assembler>(
    tdfa: &Tdfa,
    skip: Option<PrefixSkip>,
) -> Result<(Vec<u8>, usize), JitError> {
    use std::collections::{HashMap, HashSet};
    // Works for both anchored (fixed start) and unanchored (start stamped by the
    // `.*?` prefix's handoff transition, read back by `finalize`) automata; the
    // leftmost-start rule reduces to last-accept-wins without conditionals.
    // Per-byte guards (multiline `^`/`$`, `\b`/`\B`) and any `$` accept need
    // boundary-signature evaluation / an EOI accept the codegen doesn't yet emit.
    // TODO(stage 4): compute the signature inline and emit the guard branches.
    if tdfa.has_perbyte_guards() || tdfa.has_eoi_accepts() {
        return Err(JitError::Unsupported("zero-width guards"));
    }
    // We apply marks via the precompiled `MoveOp` sequences; the interpreter's
    // scalar fallback (huge mark files) isn't lowered.
    if !tdfa.has_moves() {
        return Err(JitError::Unsupported("no compiled moves (mark file too large)"));
    }
    let num_marks = tdfa.num_marks();
    if num_marks + 3 > JIT_MAX_MARK_LANES {
        return Err(JitError::Unsupported("mark file too large for JIT offsets"));
    }
    let num_states = tdfa.num_states();
    if num_states > u16::MAX as usize {
        return Err(JitError::Unsupported("too many states for movz state id"));
    }
    let accepting = tdfa.accepting();
    // Fallback accepts (an accepting state that can read on and clobber the
    // winner's registers) are handled with an eager snapshot into `best_snap`;
    // see `cap_snapshot` and the `SNAPSHOT_FLAG` return bit.
    let fallback = tdfa.accept_fallback();

    let nc = tdfa.num_classes();
    let transitions = tdfa.transitions();
    let trans_moves = tdfa.transition_moves();
    let byte_to_class = tdfa.byte_to_class();
    let curpos_idx = (num_marks + 1) as u32;
    let mark_width = (num_marks + 3) as u32;
    let start_anchored = tdfa.start(0) as usize;
    let start_unanchored = tdfa.start(1) as usize;

    let mut asm = A::new();
    let classtab = asm.fresh_label();
    let done = asm.fresh_label();
    let block: Vec<Label> = (0..num_states).map(|_| asm.fresh_label()).collect();
    let jt: Vec<Label> = (0..num_states).map(|_| asm.fresh_label()).collect();
    // A move stub per edge with a non-empty move sequence — but deduplicated:
    // edges sharing the same (moves, target) point at one shared stub. Many do,
    // especially the unanchored `.*?` handoffs that all stamp the start mark and
    // jump to the same state. `stub_reps` keeps one representative edge per
    // unique stub, in deterministic insertion order, for emission below.
    let mut stub: Vec<Option<Label>> = vec![None; num_states * nc];
    let mut stub_map: HashMap<(Vec<(u16, u16)>, u32), Label> = HashMap::new();
    let mut stub_reps: Vec<(Label, usize)> = Vec::new();
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
            let lbl = if let Some(&lbl) = stub_map.get(&key) {
                lbl
            } else {
                let lbl = asm.fresh_label();
                stub_reps.push((lbl, idx));
                stub_map.insert(key, lbl);
                lbl
            };
            stub[idx] = Some(lbl);
        }
    }

    // Dispatch plan per state (a move-edge resolves to its stub label).
    let plans: Vec<Dispatch> = (0..num_states)
        .map(|s| {
            analyze_dispatch(byte_to_class, done, |c| {
                let idx = s * nc + c;
                let t = transitions[idx];
                if t == TDFA_DEAD_STATE {
                    done
                } else if let Some(lbl) = stub[idx] {
                    lbl
                } else {
                    block[t as usize]
                }
            })
        })
        .collect();

    // Only emit blocks reachable from the entry points (incl. the warm-start
    // post_state). A stub is emitted only when a reachable state points at it;
    // its target is then reachable too, so `block[target]` below is always bound.
    let reachable = reachable_states(tdfa, skip.map(|s| s.post_state as usize));
    let mut referenced_stubs: HashSet<u32> = HashSet::new();
    for s in 0..num_states {
        if !reachable[s] {
            continue;
        }
        for c in 0..nc {
            if let Some(lbl) = stub[s * nc + c] {
                referenced_stubs.insert(lbl.0);
            }
        }
    }

    let warm = warm_entry(skip, &block);
    asm.cap_prologue(classtab, block[start_anchored], block[start_unanchored], warm);
    for s in 0..num_states {
        if !reachable[s] {
            continue;
        }
        asm.bind(block[s]);
        if accepting[s] {
            // Record the accept; for a fallback accept also snapshot the live
            // marks (they may be clobbered before scan end).
            asm.cap_record_accept(s as u32, fallback[s]);
            if fallback[s] {
                asm.cap_snapshot(mark_width);
            }
        }
        // The capture tier still declines `has_eoi_accepts`, so every state's
        // EOI target is the shared `done`.
        emit_dispatch(&mut asm, &plans[s], jt[s], done, done);
    }
    asm.bind(done);
    asm.cap_done();

    // Move stubs (one per unique (moves, target)): stamp current pos, apply the
    // edge's moves, jump to the target. Only those a reachable state references.
    let mut mvs: Vec<(u16, u16)> = Vec::new();
    for &(lbl, idx) in &stub_reps {
        if !referenced_stubs.contains(&lbl.0) {
            continue;
        }
        asm.bind(lbl);
        mvs.clear();
        mvs.extend(trans_moves[idx].iter().map(|m| (m.dst, m.src)));
        asm.cap_move_stub(curpos_idx, &mvs, block[transitions[idx] as usize]);
    }

    // Data: shared class table, then one jump table per state. A slot routes to
    // `done` (dead), a move stub (transition with marks), or straight to the
    // target block (transition with no marks).
    let data_start = asm.offset();
    asm.class_table(classtab, byte_to_class);
    let mut entries: Vec<Label> = Vec::with_capacity(nc);
    for s in 0..num_states {
        if !reachable[s] || !matches!(plans[s], Dispatch::Table) {
            continue;
        }
        entries.clear();
        for c in 0..nc {
            let idx = s * nc + c;
            let t = transitions[idx];
            entries.push(if t == TDFA_DEAD_STATE {
                done
            } else if let Some(lbl) = stub[idx] {
                lbl
            } else {
                block[t as usize]
            });
        }
        asm.jump_table(jt[s], &entries);
    }

    Ok((asm.finish(), data_start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::mem::ExecBuffer;
    use crate::automata::nfa::Nfa;
    use crate::automata::tdfa_backend::{self, Scratch};

    fn anchored_tdfa(pattern: &str) -> Tdfa {
        let flags = crate::api::Flags::default();
        let re = crate::backends::try_parse(pattern.chars().map(u32::from), flags)
            .expect("parse failed");
        let nfa = Nfa::try_from(&re).expect("nfa build failed");
        let mut tdfa = Tdfa::try_from(&nfa).expect("tdfa build failed");
        tdfa.optimize();
        tdfa
    }

    /// `disassemble` (feature `tdfa-jit-dump`) produces readable output for both
    /// tiers — a smoke test that the capstone path is wired and the generated
    /// code decodes (ends in `ret`).
    #[cfg(feature = "tdfa-jit-dump")]
    #[test]
    fn jit_disassemble_smoke() {
        for pat in ["foo[0-9]+", "foo(\\d+)"] {
            let asm = disassemble(&anchored_tdfa(pat)).unwrap_or_else(|e| panic!("{pat}: {e}"));
            assert!(asm.contains("ret"), "{pat} disasm missing ret:\n{asm}");
            assert!(asm.lines().count() > 5, "{pat} disasm too short:\n{asm}");
        }
    }

    fn unanchored_tdfa(pattern: &str) -> Tdfa {
        let flags = crate::api::Flags::default();
        let re = crate::backends::try_parse(pattern.chars().map(u32::from), flags)
            .expect("parse failed");
        let nfa = Nfa::try_from_unanchored(&re).expect("unanchored nfa build failed");
        let mut tdfa = Tdfa::try_from(&nfa).expect("tdfa build failed");
        tdfa.optimize();
        tdfa
    }

    /// JIT output (range **and** captures) must match the interpreter (the
    /// oracle) for every (input, start) pair.
    fn assert_matches_interpreter(pattern: &str, inputs: &[&str]) {
        let tdfa = anchored_tdfa(pattern);
        let jit = JittedTdfa::compile(&tdfa, None)
            .unwrap_or_else(|e| panic!("compile {pattern:?}: {e}"));
        let mut scratch =
            Scratch::new(tdfa_backend::mark_file_width(&tdfa), tdfa.num_tags());
        for input in inputs {
            let bytes = input.as_bytes();
            for start in 0..=bytes.len() {
                let want = tdfa_backend::execute(&tdfa, bytes, start);
                let got = jit.run(&tdfa, bytes, start, &mut scratch);
                assert_eq!(
                    want.as_ref().map(|m| (m.range.clone(), m.captures.clone())),
                    got.as_ref().map(|m| (m.range.clone(), m.captures.clone())),
                    "pattern {pattern:?} input {input:?} start {start}",
                );
            }
        }
    }

    #[test]
    fn jit_literal() {
        assert_matches_interpreter("abc", &["abc", "xabc", "ab", "abcabc", "", "abx"]);
    }

    #[test]
    fn jit_quantifiers() {
        assert_matches_interpreter("a+", &["", "a", "aaa", "baaa", "aaab"]);
        assert_matches_interpreter("a*b", &["b", "ab", "aaab", "aaa", "xb"]);
        assert_matches_interpreter("[0-9]+", &["123", "", "12a34", "a1"]);
    }

    #[test]
    fn jit_alternation_and_groups() {
        assert_matches_interpreter("a|bc", &["a", "bc", "b", "abc", ""]);
        assert_matches_interpreter("(?:ab)+", &["ab", "abab", "aba", "a", ""]);
        assert_matches_interpreter("(?:foo|bar)+", &["foobar", "foo", "barbar", "baz"]);
    }

    /// End-to-end through the public backend: the `TdfaJitExecutor` must yield
    /// the same matches as the interpreter `TdfaExecutor` over a real
    /// `find_iter`, and actually run native code for a prefix-literal pattern.
    #[test]
    fn jit_backend_matches_interpreter() {
        use crate::automata::prefilter::{TdfaJitProgram, TdfaProgram};
        use crate::backends::{self, TdfaExecutor, TdfaJitExecutor};

        // `expect_jit`: `Some(true)` patterns must run native code — a literal
        // prefix + tail (anchored verify) or an unanchored `Scan` (`a+b`).
        // `Some(false)` stays interpreted: a bare literal is the memmem-only
        // whole-literal path (no automaton). `None` is selection-dependent —
        // only correctness is checked.
        let cases: &[(&str, &str, Option<bool>)] = &[
            ("abc", "xx abc yy abcabc zz", Some(false)),
            ("foo[0-9]+", "foo12 foobar foo3 foo", Some(true)),
            ("a+b", "aaab ab cab b aaa", Some(true)),
            ("(?:cat|dog)s?", "cats dog doghouse ca dogs", None),
            ("xyz[a-z]*", "xyzabc xy xyz xyzzz", Some(true)),
        ];
        for &(pat, hay, expect_jit) in cases {
            let flags = crate::api::Flags::default();
            let mut re = backends::try_parse(pat.chars().map(u32::from), flags).expect("parse");
            // `try_from_ir` expects optimized IR (it's what lowers a literal run
            // into a `ByteSequence` the prefilter can use).
            backends::optimize(&mut re);
            let interp = TdfaProgram::try_from_ir(&re).expect("interp program");
            let jit = TdfaJitProgram::try_from_ir(&re).expect("jit program");

            let want: Vec<_> = backends::find::<TdfaExecutor>(&interp, hay, 0)
                .map(|m| (m.range.clone(), m.captures.clone()))
                .collect();
            let got: Vec<_> = backends::find::<TdfaJitExecutor>(&jit, hay, 0)
                .map(|m| (m.range.clone(), m.captures.clone()))
                .collect();
            assert_eq!(want, got, "pattern {pat:?} over {hay:?}");
            if let Some(want_jit) = expect_jit {
                assert_eq!(jit.jit_active(), want_jit, "jit activation for {pat:?}");
            }
        }
    }

    #[test]
    fn jit_byte_classes_and_negation() {
        assert_matches_interpreter("a.c", &["abc", "axc", "ac", "aXcc"]);
        assert_matches_interpreter("[^x]+", &["abc", "x", "abxc", "", "xxx"]);
        assert_matches_interpreter("[a-c]+", &["abc", "abcd", "d", "cba"]);
    }

    /// Peeled self-loop states (the wide, scan-bound loops) must match the
    /// interpreter — including the paths the peel is subtle about: `.`'s newline
    /// exit, UTF-8 multi-byte code points (a lead byte leaves the ASCII self-set,
    /// so the peel falls back to the scalar UTF-8 chain, then resumes), the
    /// single-byte `"` exit, and accepting vs. non-accepting loops (`acc = pos-1`
    /// on exit vs. `acc = pos` at EOI). Every start offset is checked.
    #[test]
    fn jit_self_loop_peel_vs_interpreter() {
        let patterns = [
            ".*", ".+", "a.*b", // `.` = [^\n\r…]; accepting (`.*`) and not (`a.*b`)
            "\"[^\"]*\"", "[^\"]+", // single-byte `"` exit; Table self-loop
            "\\w+", "[a-zA-Z]+ing", "[a-z]*", // accepting Ranges/`\w` loops (accept hoist)
        ];
        let inputs = [
            "",
            "hello world",
            "a\nb\nc",              // `.` newline exits
            "line1\r\nline2",       // CR and LF
            "\"quoted\" tail",      // `"` exit
            "\"\"",
            "über café: 日本語!",   // multi-byte UTF-8 (lead-byte exits → scalar chain)
            "emoji 😀 x",           // 4-byte code point
            "abcding endings",
            "MixedCaseIng",
            "no_match_here",
            "\n",
            "x",
        ];
        for pat in patterns {
            assert_matches_interpreter(pat, &inputs);
        }
    }

    /// The SIMD (16-byte) self-loop skip only engages when ≥16 self bytes remain,
    /// so it needs long inputs to exercise at all. Drive it with runs well past a
    /// vector width, with the exit (newline / quote / space / UTF-8 lead byte)
    /// landing at many offsets — checking every start offset shifts the 16-byte
    /// alignment relative to the exit, so the "first out-of-set lane" (`bsf`) is
    /// hit at every lane and in the scalar tail. Oracle: the interpreter.
    #[test]
    fn jit_simd_self_skip_long_inputs() {
        let long: Vec<String> = vec![
            "a".repeat(64),                                    // long pure run to EOI
            "abcdefghijklmnopqrstuvwxyz".repeat(3),            // 78 lowercase
            format!("{}\n{}", "x".repeat(30), "y".repeat(30)), // newline mid-run
            format!("{}\r\n{}", "p".repeat(17), "r".repeat(17)), // CRLF at a boundary
            format!("{}\"{}", "z".repeat(20), "w".repeat(20)), // quote mid-run
            format!("\"{}\"", "q".repeat(40)),                 // long quoted (for "[^"]*")
            "café ".repeat(12),                                // repeated 2-byte UTF-8
            format!("{} 日本語 tail", "word".repeat(8)),       // 3-byte UTF-8 after a run
            format!("{}😀{}", "e".repeat(18), "f".repeat(18)), // 4-byte code point mid-run
            format!("{} {}", "aaaa".repeat(9), "bbbb".repeat(9)), // space mid-run
        ];
        let patterns = [
            ".*", ".+", "a.*b", "\"[^\"]*\"", "[^\"]+", "\\w+", "[a-zA-Z]+ing", "[a-z]*",
        ];
        for pat in patterns {
            let tdfa = anchored_tdfa(pat);
            let jit = JittedTdfa::compile(&tdfa, None)
                .unwrap_or_else(|e| panic!("compile {pat:?}: {e}"));
            let mut scratch = Scratch::new(tdfa_backend::mark_file_width(&tdfa), tdfa.num_tags());
            for input in &long {
                let bytes = input.as_bytes();
                for start in 0..=bytes.len() {
                    let want = tdfa_backend::execute(&tdfa, bytes, start);
                    let got = jit.run(&tdfa, bytes, start, &mut scratch);
                    assert_eq!(
                        want.as_ref().map(|m| (m.range.clone(), m.captures.clone())),
                        got.as_ref().map(|m| (m.range.clone(), m.captures.clone())),
                        "pattern {pat:?} input {input:?} start {start}",
                    );
                }
            }
        }
    }

    /// Non-multiline `$` accepts: the capture-free tier records the accept at an
    /// EOI landing pad. Ranges/Table states retarget their `eoi_check`; the
    /// dead-end `$` state (`AllDone`, e.g. after `foo`) gains an `eoi_check` it
    /// otherwise skips. Must match the interpreter at every start, including
    /// inputs that stop short of EOI (`"foox"`, `"xfoo"`).
    #[test]
    fn jit_end_anchor_vs_interpreter() {
        // `foo$` must land on the capture-free tier now (was declined before).
        let tdfa = anchored_tdfa("foo$");
        let jit = JittedTdfa::compile(&tdfa, None).expect("foo$ should compile");
        assert!(
            matches!(jit.compiled, Compiled::CaptureFree(_)),
            "foo$ should use the capture-free tier",
        );

        assert_matches_interpreter("foo$", &["foo", "foox", "xfoo", "fo", "", "foofoo"]);
        assert_matches_interpreter("^foo$", &["foo", "foox", "xfoo", ""]);
        assert_matches_interpreter("[a-z]+$", &["abc", "abc1", "1abc", "", "a"]);
        assert_matches_interpreter("foo$|bar$", &["foo", "bar", "foobar", "barx", ""]);
        assert_matches_interpreter("ab*c$", &["ac", "abbc", "abbcx", "abb", ""]);
    }

    /// Unanchored tier: the JIT must match the interpreter's single-pass scan
    /// (`.*?`-prefixed automaton) — leftmost match, with the start read from the
    /// stamped `FULL_MATCH_START` mark — for capture-free and capturing patterns.
    #[test]
    fn jit_unanchored_vs_interpreter() {
        let patterns = [
            "\\w+", "[0-9]+", "(\\w+)", "(\\d+)-(\\d+)", "foo(\\d+)", "a+b",
            "(ab)+", "(\\w)(\\w)",
        ];
        let inputs = [
            "", "xx ab12 cd", "  123-456 ", "fooo foo7", "aaab", "ababab x",
            "no digits", "a1b2c3", "   ", "12-34-56",
        ];
        for pat in patterns {
            let tdfa = unanchored_tdfa(pat);
            let Ok(jit) = JittedTdfa::compile(&tdfa, None) else {
                continue; // outside the supported tier (e.g. conditionals)
            };
            let mut scratch =
            Scratch::new(tdfa_backend::mark_file_width(&tdfa), tdfa.num_tags());
            for input in inputs {
                let bytes = input.as_bytes();
                for start in 0..=bytes.len() {
                    let want = tdfa_backend::execute(&tdfa, bytes, start);
                    let got = jit.run(&tdfa, bytes, start, &mut scratch);
                    assert_eq!(
                        want.as_ref().map(|m| (m.range.clone(), m.captures.clone())),
                        got.as_ref().map(|m| (m.range.clone(), m.captures.clone())),
                        "pattern {pat:?} input {input:?} start {start}",
                    );
                }
            }
        }
    }

    /// Throughput smoke check (run with `--ignored --nocapture`): compare the
    /// JIT against the interpreter fast loop on a large capture-free anchored
    /// scan. Not an assertion — prints MB/s for both so the speedup is visible.
    #[test]
    #[ignore = "performance, run manually with --ignored --nocapture"]
    fn jit_throughput_vs_interpreter() {
        use std::time::Instant;
        // Capture-free self-loop scans (the peel targets: `.*`/`"[^"]*"` are
        // Table self-loops that shed a class-table load + indirect branch per
        // byte; `\w+`/`[a-z]+` are Ranges loops that shed the per-byte accept),
        // plus a capture scan (group marks stamped per byte).
        for pattern in [".*", "\"[^\"]*\"", "\\w+", "[a-z]+", "([a-z]+)"] {
            let tdfa = anchored_tdfa(pattern);
            let jit = JittedTdfa::compile(&tdfa, None).expect("compile");
            let mut scratch =
            Scratch::new(tdfa_backend::mark_file_width(&tdfa), tdfa.num_tags());
            // A 4 MiB self-looping haystack; `"[^"]*"` needs a leading quote so
            // the loop is entered (and no closing quote so it runs to the end).
            let input = if pattern.starts_with('"') {
                let mut v = vec![b'a'; 4 * 1024 * 1024];
                v[0] = b'"';
                v
            } else {
                vec![b'a'; 4 * 1024 * 1024]
            };
            let iters = 200;

            let mut sink = 0usize;
            let t = Instant::now();
            for _ in 0..iters {
                sink += tdfa_backend::execute(&tdfa, &input, 0).map_or(0, |m| m.range.end);
            }
            let interp = t.elapsed();

            let t = Instant::now();
            for _ in 0..iters {
                sink += jit
                    .run(&tdfa, &input, 0, &mut scratch)
                    .map_or(0, |m| m.range.end);
            }
            let jitted = t.elapsed();

            let mb = (input.len() * iters) as f64 / (1024.0 * 1024.0);
            eprintln!(
                "{pattern:<10} interp: {:>5.0} MB/s   jit: {:>5.0} MB/s   speedup: {:.2}x   (sink={sink})",
                mb / interp.as_secs_f64(),
                mb / jitted.as_secs_f64(),
                interp.as_secs_f64() / jitted.as_secs_f64(),
            );
        }
    }

    /// Randomized differential test: for a spread of patterns, compare the JIT
    /// against the interpreter on many random inputs at every start offset.
    #[test]
    fn jit_fuzz_vs_interpreter() {
        use rand::rngs::SmallRng;
        use rand::{Rng, SeedableRng};

        let patterns = [
            "a+b+", "(?:ab|cd)+", "[0-9]+", "[a-c]*z", "x.y", "[^ab]+",
            "foo", "a?b?c?", "(?:a|b|c)+d",
            // Non-multiline `$` accepts (EOI landing pads).
            "a+b$", "[a-c]*z$", "foo$|bar$", "x.y$", "ab*c$",
        ];
        let alphabet = b"abcdz019x";
        let mut rng = SmallRng::seed_from_u64(0xC0DA_F00D);

        for pat in patterns {
            let tdfa = anchored_tdfa(pat);
            let Ok(jit) = JittedTdfa::compile(&tdfa, None) else {
                continue; // legitimately unsupported tier; backend falls back
            };
            let mut scratch =
            Scratch::new(tdfa_backend::mark_file_width(&tdfa), tdfa.num_tags());
            for _ in 0..200 {
                let len = rng.gen_range(0..12);
                let bytes: Vec<u8> =
                    (0..len).map(|_| alphabet[rng.gen_range(0..alphabet.len())]).collect();
                for start in 0..=bytes.len() {
                    let want = tdfa_backend::execute(&tdfa, &bytes, start);
                    let got = jit.run(&tdfa, &bytes, start, &mut scratch);
                    assert_eq!(
                        want.as_ref().map(|m| (m.range.clone(), m.captures.clone())),
                        got.as_ref().map(|m| (m.range.clone(), m.captures.clone())),
                        "pattern {pat:?} input {bytes:?} start {start}",
                    );
                }
            }
        }
    }

    /// Capture tier: ranges **and** captured group spans must match the
    /// interpreter, including unmatched groups (the `Some(0..0)` sentinel) and
    /// quantified groups (last iteration wins).
    #[test]
    fn jit_captures_vs_interpreter() {
        // Capture patterns, including fallback accepts (quantified groups like
        // `(ab)+` / `(\w+\s*)+`, which accept then read on) handled by the eager
        // snapshot.
        let patterns = [
            "foo(\\d+)",
            "(\\d+)-(\\d+)",
            "(a+)(b+)",
            "x(\\w)(\\w)",
            "(\\d+)(z)?",
            "([a-c])([0-9])",
            "(ab)+",
            "(\\w+\\s*)+",
            "(\\d+,)+",
        ];
        for pat in patterns {
            let tdfa = anchored_tdfa(pat);
            // The capture tier must actually compile for these.
            let jit = JittedTdfa::compile(&tdfa, None)
                .unwrap_or_else(|e| panic!("compile {pat:?}: {e}"));
            assert!(
                matches!(jit.compiled, Compiled::Capture(_)),
                "{pat:?} should use the capture tier",
            );
            let mut scratch =
            Scratch::new(tdfa_backend::mark_file_width(&tdfa), tdfa.num_tags());
            let inputs = [
                "foo123", "12-345", "aaabb", "xpq", "ababab", "9", "b7", "", "zzz", "abab",
                "a b  c ", "1,2,3,", "ab a",
            ];
            for input in inputs {
                let bytes = input.as_bytes();
                for start in 0..=bytes.len() {
                    let want = tdfa_backend::execute(&tdfa, bytes, start);
                    let got = jit.run(&tdfa, bytes, start, &mut scratch);
                    assert_eq!(
                        want.as_ref().map(|m| (m.range.clone(), m.captures.clone())),
                        got.as_ref().map(|m| (m.range.clone(), m.captures.clone())),
                        "pattern {pat:?} input {input:?} start {start}",
                    );
                }
            }
        }
    }

    /// Validate the executable-memory path end to end on this host: hand-encode
    /// a tiny function and call it. This is the platform tracer bullet — if RX
    /// allocation / protection / icache flush / calling convention work here,
    /// the codegen can build on top.
    #[test]
    fn exec_buffer_calls_handwritten_fn() {
        // A function `(usize, usize, usize) -> usize` returning its 3rd arg.
        #[cfg(target_arch = "aarch64")]
        let code: &[u8] = &[
            0xE0, 0x03, 0x02, 0xAA, // mov x0, x2
            0xC0, 0x03, 0x5F, 0xD6, // ret
        ];
        #[cfg(target_arch = "x86_64")]
        let code: &[u8] = &[
            0x48, 0x89, 0xD0, // mov rax, rdx
            0xC3, // ret
        ];
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        let code: &[u8] = &[];
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            return; // no encoder/ABI knowledge for this arch
        }

        #[allow(unreachable_code)]
        {
            let buf = ExecBuffer::new(code).expect("alloc RX");
            // SAFETY: `buf` outlives the call; the bytes implement this exact
            // C ABI signature on the target arch.
            let f: extern "C" fn(usize, usize, usize) -> usize =
                unsafe { core::mem::transmute(buf.entry_ptr()) };
            assert_eq!(f(10, 20, 30), 30);
            assert_eq!(f(1, 2, 3), 3);
        }
    }
}
