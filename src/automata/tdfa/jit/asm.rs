//! Architecture-independent assembler surface the codegen driver speaks to.
//!
//! The driver (in [`super`]) walks the [`Tdfa`](crate::automata::tdfa::Tdfa) and
//! emits a fixed, DFA-shaped sequence of operations; each architecture provides
//! an [`Assembler`] that lowers those operations to machine code. Because the
//! generated control flow is so regular (state blocks, a shared class table,
//! per-state jump tables), the operations are DFA-level rather than
//! instruction-level — every encoder produces the same structure with its own
//! registers and encodings.
//!
//! ## Register roles (fixed, no allocation)
//!
//! The generated function has C signature
//! `extern "C" fn(input: *const u8, len: usize, start: usize) -> usize`,
//! returning the match-end offset or `usize::MAX` for no match. Each encoder
//! pins a small fixed set of registers for the whole function:
//!
//! - `input` — base pointer (arg 0)
//! - `end` — `len` (arg 1); the loop runs while `pos < end`
//! - `pos` — current offset (arg 2 = `start` initially)
//! - `acc` — last accepted end offset, initialized to `usize::MAX`
//! - `classtab` — base of the 256-byte byte→class table
//! - plus a couple of caller-saved scratch registers for the loaded byte/class
//!   and the jump-table address
//!
//! The capture-free tier makes no calls, so the function is a leaf with no
//! stack frame and no callee-saved registers to preserve.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// A code location. Created via [`Assembler::fresh_label`], later bound to an
/// offset with [`Assembler::bind`] (for code blocks) or as the address of an
/// emitted data table. References to a label are resolved in
/// [`Assembler::finish`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) struct Label(pub u32);

/// Tracks the byte offset each [`Label`] resolves to. Shared by all encoders;
/// the arch-specific fixup *application* lives in each [`Assembler`].
#[derive(Debug, Default)]
pub(crate) struct Labels {
    /// `offset[l]` is `Some(byte_offset)` once the label is bound.
    offset: Vec<Option<u32>>,
}

impl Labels {
    pub(crate) fn new() -> Self {
        Self { offset: Vec::new() }
    }

    /// Mint a fresh, unbound label.
    pub(crate) fn fresh(&mut self) -> Label {
        let id = self.offset.len() as u32;
        self.offset.push(None);
        Label(id)
    }

    /// Bind `l` to `offset`. Panics if already bound (a codegen bug).
    pub(crate) fn bind(&mut self, l: Label, offset: usize) {
        let slot = &mut self.offset[l.0 as usize];
        debug_assert!(slot.is_none(), "label {l:?} bound twice");
        *slot = Some(offset as u32);
    }

    /// The bound offset of `l`. Panics if unbound (a codegen bug — every
    /// referenced label must be bound before `finish`).
    pub(crate) fn offset_of(&self, l: Label) -> u32 {
        self.offset[l.0 as usize].expect("referenced label was never bound")
    }
}

/// The DFA-shaped operations the codegen driver emits. One implementation per
/// target architecture. All branch/data references are by [`Label`] and
/// resolved in [`finish`](Assembler::finish); a forward reference is fine.
pub(crate) trait Assembler {
    fn new() -> Self;

    /// Current emission offset (bytes emitted so far). Records the code/data
    /// boundary for `--dump-jit`.
    fn offset(&self) -> usize;

    /// Mint a fresh label.
    fn fresh_label(&mut self) -> Label;
    /// Bind `l` to the current emission offset (start of the next instruction).
    fn bind(&mut self, l: Label);

    /// Function prologue: initialize `acc = usize::MAX`, load `classtab`'s
    /// address, then branch to the start state — `start_anchored` when the
    /// `start` argument is 0, else `start_unanchored`. When `warm` is
    /// `Some((post_block, len))`, instead warm-start a prefilter-matched prefix:
    /// advance `pos` by `len` and branch straight to `post_block` (sound only
    /// when the anchored and unanchored starts coincide, which the caller
    /// guarantees via `compute_prefix_skip`).
    fn prologue(
        &mut self,
        classtab: Label,
        start_anchored: Label,
        start_unanchored: Label,
        warm: Option<(Label, usize)>,
    );

    /// Record an accept at the current position: `acc = pos`. Emitted at the
    /// top of accepting state blocks only.
    fn record_accept(&mut self);

    /// Record an accept at the *previous* position: `acc = pos - 1`. Used by the
    /// peeled self-loop exit path, where `pos` has already been advanced past the
    /// byte that left the state, so the match ends one byte earlier.
    fn record_accept_prev(&mut self);

    /// If `pos >= end`, branch to `done` (end of input — stop scanning).
    fn eoi_check(&mut self, done: Label);

    /// Load `byte = input[pos]` and advance `pos`. (No class lookup — the
    /// byte-range dispatch tests the raw byte; only the jump-table path needs
    /// [`classify`](Self::classify).)
    ///
    /// Provided as `load_byte(); advance(1)`; the peeled self-loop path (which
    /// must test the byte *before* advancing, so `pos` points at the exit byte
    /// on the way out) and the SIMD skip path call the two halves separately.
    fn fetch_byte(&mut self) {
        self.load_byte();
        self.advance(1);
    }

    /// Load `byte = input[pos]` without touching `pos`.
    fn load_byte(&mut self);

    /// `pos += n`.
    fn advance(&mut self, n: u32);

    /// `class = classtab[byte]` (overwrites the byte register). Only emitted
    /// before a jump-table [`dispatch`](Self::dispatch).
    fn classify(&mut self);

    /// Unconditional branch to `target`.
    fn branch(&mut self, target: Label);

    /// Compare-chain dispatch on the loaded byte (sparse states): for each
    /// `(lo, hi, target)` run, branch to `target` when `lo <= byte <= hi`;
    /// otherwise fall through to `default`. Skips the class table entirely.
    fn dispatch_byte_ranges(&mut self, runs: &[(u8, u8, Label)], default: Label);

    /// Indirect-branch to the target block via `jump_table[class]` (dense
    /// states). A dead transition's slot points at `done`. Requires a preceding
    /// [`classify`](Self::classify).
    fn dispatch(&mut self, jump_table: Label);

    /// The `done` block body: return `acc` (in the ABI return register) and
    /// `ret`. The driver binds the `done` label immediately before calling this.
    fn ret_done(&mut self);

    /// Emit the shared 256-byte byte→class table at `l`.
    fn class_table(&mut self, l: Label, table: &[u8; 256]);

    /// Emit a per-state jump table at `l`: one entry per byte class, each
    /// pointing at the target block's label (dead → `done`).
    fn jump_table(&mut self, l: Label, entries: &[Label]);

    /// Resolve all label references and return the finished machine code.
    fn finish(self) -> Vec<u8>;

    // ----- capture tier -----
    //
    // Same control flow as the capture-free tier, but the mark file (arg 3, a
    // `*mut u32`) is threaded through and per-transition `MoveOp`s are applied
    // as inlined stores. The accept bookkeeping tracks `(end, winning state)`
    // instead of just `end`, and `cap_done` packs both into the `u64` return
    // (`(state << 32) | end`, or `u64::MAX` for no match). `eoi_check`,
    // `fetch_and_classify`, `dispatch`, `jump_table`, and `class_table` are
    // shared with the capture-free tier (each encoder keeps a register map that
    // agrees across both tiers).

    /// Capture-tier prologue: stash the `best_snap` pointer (arg 4), initialize
    /// the accept-end sentinel, load `classtab`, then branch to the start state
    /// (anchored when `start == 0`). `warm` warm-starts a prefilter-matched
    /// prefix exactly as in [`prologue`](Self::prologue) — the skipped prefix
    /// writes no marks (caller-guaranteed), so the prepared mark file stays
    /// valid.
    fn cap_prologue(
        &mut self,
        classtab: Label,
        start_anchored: Label,
        start_unanchored: Label,
        warm: Option<(Label, usize)>,
    );

    /// Record an accept at the current position in state `state_id`:
    /// `acc_end = pos`, `acc_state = state_id`. When `is_fallback`, the winning
    /// marks may be clobbered before scan end, so the snapshot flag (high bit)
    /// is folded into `acc_state` (the caller then reads `best_snap`); the
    /// driver emits a following [`cap_snapshot`](Self::cap_snapshot).
    fn cap_record_accept(&mut self, state_id: u32, is_fallback: bool);

    /// Copy `width` u32 lanes from the live mark file into `best_snap` (a
    /// fallback accept's eager snapshot). Emitted at the top of fallback
    /// accepting blocks, right after [`cap_record_accept`](Self::cap_record_accept).
    fn cap_snapshot(&mut self, width: u32);

    /// A move stub: stamp the current position into mark lane `curpos_idx`, then
    /// apply each `(dst, src)` move as `marks[dst] = marks[src]` (u32 lanes),
    /// then branch to `target`. A jump-table slot for an edge with a non-empty
    /// move sequence points here instead of straight at the target block.
    fn cap_move_stub(&mut self, curpos_idx: u32, moves: &[(u16, u16)], target: Label);

    /// Capture-tier epilogue: if no accept fired, return `u64::MAX`; else return
    /// `(acc_state << 32) | acc_end`.
    fn cap_done(&mut self);
}
