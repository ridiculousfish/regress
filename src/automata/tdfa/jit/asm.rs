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

    /// Mint a fresh label.
    fn fresh_label(&mut self) -> Label;
    /// Bind `l` to the current emission offset (start of the next instruction).
    fn bind(&mut self, l: Label);

    /// Function prologue: initialize `acc = usize::MAX`, load `classtab`'s
    /// address, then branch to the start state — `start_anchored` when the
    /// `start` argument is 0, else `start_unanchored`.
    fn prologue(&mut self, classtab: Label, start_anchored: Label, start_unanchored: Label);

    /// Record an accept at the current position: `acc = pos`. Emitted at the
    /// top of accepting state blocks only.
    fn record_accept(&mut self);

    /// If `pos >= end`, branch to `done` (end of input — stop scanning).
    fn eoi_check(&mut self, done: Label);

    /// Load `byte = input[pos]`, advance `pos`, then `class = classtab[byte]`.
    fn fetch_and_classify(&mut self);

    /// Indirect-branch to the target block via `jump_table[class]`. A dead
    /// transition's slot points at `done`.
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
}
