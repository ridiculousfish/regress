//! x86-64 (System V AMD64 ABI) encoder for the TDFA JIT.
//!
//! Unified fixed register map (all caller-saved; the function is a leaf, no
//! frame). Both tiers share it, so the per-byte methods (`eoi_check`,
//! `fetch_and_classify`, `dispatch`) and the data emitters serve both:
//!
//! | role       | reg | notes                                        |
//! |------------|-----|----------------------------------------------|
//! | `input`    | rdi | arg 0, base pointer                          |
//! | `end`      | rsi | arg 1, `len`                                 |
//! | `pos`      | rdx | arg 2 = `start`, then incremented            |
//! | `marks`    | rcx | arg 3 (capture tier only; unused otherwise)  |
//! | `classtab` | r8  | base of the byte→class table                 |
//! | `acc_end`  | r9  | last accept end (capture-free: the result)   |
//! | `acc_state`| r10 | winning state id (capture tier only)         |
//! | scratch    | rax | byte/class, move value, jump-table offset    |
//! | jt/target  | r11 | jump-table address / indirect-branch target  |
//!
//! Capture-free returns `acc_end` (usize, `usize::MAX` for no match) in rax.
//! The capture tier returns `(acc_state << 32) | acc_end`, or `u64::MAX` for no
//! match, in rax. All references resolve to position-independent values (RIP
//! disp32, jcc/jmp rel32, 32-bit table-relative words) — no relocation after
//! mapping.

use super::asm::{Assembler, Label, Labels};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// A pending patch applied in [`finish`].
enum Fixup {
    /// 32-bit field at `field` holding `label - (field + 4)` (the field is the
    /// last 4 bytes of a RIP-relative `lea` / `jcc` / `jmp`).
    Rel32 { field: u32, label: Label },
    /// A 32-bit jump-table word at `field`: `label - table_off`.
    TableWord {
        field: u32,
        label: Label,
        table_off: u32,
    },
}

pub(crate) struct X86_64Asm {
    code: Vec<u8>,
    labels: Labels,
    fixups: Vec<Fixup>,
    /// The most recently emitted unconditional `jmp` — `(offset of the E9 byte,
    /// target)` — set only while it's the last thing emitted (any other emission
    /// clears it). Lets [`bind`](Self::bind) elide a branch to the next
    /// instruction (fall-through).
    pending_jmp: Option<(usize, Label)>,
}

impl X86_64Asm {
    fn emit(&mut self, bytes: &[u8]) {
        self.pending_jmp = None;
        self.code.extend_from_slice(bytes);
    }

    fn emit_u32(&mut self, v: u32) {
        self.pending_jmp = None;
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    /// `jmp target`, remembered so a following `bind(target)` can drop it as a
    /// jump to the next instruction. Every unconditional branch routes through
    /// here.
    fn jmp(&mut self, target: Label) {
        self.emit(&[0xE9]);
        self.emit_rel32(target);
        self.pending_jmp = Some((self.code.len() - 5, target));
    }

    /// Append a 4-byte placeholder and record a `Rel32` fixup over it (the
    /// placeholder is the last 4 bytes of the current instruction).
    fn emit_rel32(&mut self, label: Label) {
        let field = self.code.len() as u32;
        self.emit(&[0, 0, 0, 0]);
        self.fixups.push(Fixup::Rel32 { field, label });
    }
}

impl Assembler for X86_64Asm {
    fn new() -> Self {
        Self {
            code: Vec::new(),
            labels: Labels::new(),
            fixups: Vec::new(),
            pending_jmp: None,
        }
    }

    fn offset(&self) -> usize {
        self.code.len()
    }

    fn fresh_label(&mut self) -> Label {
        self.labels.fresh()
    }

    fn bind(&mut self, l: Label) {
        // If the last thing emitted was `jmp l`, it's a jump to the next
        // instruction — drop it and let control fall through.
        if let Some((at, target)) = self.pending_jmp.take() {
            if target == l && at + 5 == self.code.len() {
                self.code.truncate(at);
                self.fixups.pop(); // the jmp's Rel32, pushed last
            }
        }
        let off = self.code.len();
        self.labels.bind(l, off);
    }

    fn prologue(
        &mut self,
        classtab: Label,
        start_anchored: Label,
        start_unanchored: Label,
        warm: Option<(Label, usize)>,
    ) {
        // mov r9, -1            (acc = usize::MAX)  49 C7 C1 FF FF FF FF
        self.emit(&[0x49, 0xC7, 0xC1, 0xFF, 0xFF, 0xFF, 0xFF]);
        self.load_classtab(classtab);
        self.warm_or_start(warm, start_anchored, start_unanchored);
    }

    fn record_accept(&mut self) {
        // mov r9, rdx          (acc = pos)          49 89 D1
        self.emit(&[0x49, 0x89, 0xD1]);
    }

    fn eoi_check(&mut self, done: Label) {
        // cmp rdx, rsi                              48 39 F2
        self.emit(&[0x48, 0x39, 0xF2]);
        // jae done             (pos >= end)         0F 83 <rel32>
        self.emit(&[0x0F, 0x83]);
        self.emit_rel32(done);
    }

    fn fetch_byte(&mut self) {
        // movzx eax, byte [rdi + rdx]               0F B6 04 17
        self.emit(&[0x0F, 0xB6, 0x04, 0x17]);
        // inc rdx                                   48 FF C2
        self.emit(&[0x48, 0xFF, 0xC2]);
    }

    fn classify(&mut self) {
        // movzx eax, byte [r8 + rax]                41 0F B6 04 00
        self.emit(&[0x41, 0x0F, 0xB6, 0x04, 0x00]);
    }

    fn branch(&mut self, target: Label) {
        self.jmp(target); // E9 <rel32>
    }

    fn dispatch_byte_ranges(&mut self, runs: &[(u8, u8, Label)], default: Label) {
        let mut i = 0;
        while i < runs.len() {
            let (lo, hi, target) = runs[i];
            if i + 2 == runs.len() {
                let (lo2, hi2, target2) = runs[i + 1];
                if lo == hi
                    && lo2 == hi2
                    && target == target2
                    && lo.is_ascii_alphabetic()
                    && lo2.is_ascii_alphabetic()
                    && lo.to_ascii_lowercase() == lo2.to_ascii_lowercase()
                {
                    // Terminal ASCII case pair: fold the loaded byte in-place
                    // and test once. This is smaller and removes one taken/not-
                    // taken branch in literal icase verifiers like /Sherlock/i.
                    self.emit(&[0x0C, 0x20]); // or al, 0x20
                    self.emit(&[0x3C, lo.to_ascii_lowercase()]); // cmp al, lower
                    self.emit(&[0x0F, 0x84]); // je target
                    self.emit_rel32(target);
                    i += 2;
                    continue;
                }
            }
            if lo == hi {
                // cmp al, lo ; je target            3C <lo> ; 0F 84 <rel32>
                self.emit(&[0x3C, lo]);
                self.emit(&[0x0F, 0x84]);
                self.emit_rel32(target);
            } else if lo == 0 {
                // `[0..=hi]` needs no normalization: test the raw byte. eax is
                // left intact so later runs in the chain still see it.
                emit_cmp_eax_imm(self, hi as u32);
                self.emit(&[0x0F, 0x86]); // jbe target
                self.emit_rel32(target);
            } else {
                // r11d = byte - lo, then `r11d <= hi-lo` (unsigned) -> target.
                // The `lea` folds the register copy and the subtract into one
                // op (shorter dependency chain than mov+sub+cmp) and leaves eax
                // intact for later runs.
                let disp = -(lo as i32);
                if (-128..=127).contains(&disp) {
                    // lea r11d, [rax + disp8]        44 8D 58 <disp8>
                    self.emit(&[0x44, 0x8D, 0x58, disp as i8 as u8]);
                } else {
                    // lea r11d, [rax + disp32]       44 8D 98 <disp32>
                    self.emit(&[0x44, 0x8D, 0x98]);
                    self.emit_u32(disp as u32);
                }
                let span = (hi - lo) as u32;
                if span <= 127 {
                    // cmp r11d, imm8                 41 83 FB <imm8>
                    self.emit(&[0x41, 0x83, 0xFB, span as u8]);
                } else {
                    // cmp r11d, imm32                41 81 FB <imm32>
                    self.emit(&[0x41, 0x81, 0xFB]);
                    self.emit_u32(span);
                }
                self.emit(&[0x0F, 0x86]); // jbe target
                self.emit_rel32(target);
            }
            i += 1;
        }
        self.jmp(default); // E9 <rel32>
    }

    fn dispatch(&mut self, jump_table: Label) {
        // lea r11, [rip + jump_table]               4C 8D 1D <disp32>
        self.emit(&[0x4C, 0x8D, 0x1D]);
        self.emit_rel32(jump_table);
        // movsxd rax, dword [r11 + rax*4]           49 63 04 83
        self.emit(&[0x49, 0x63, 0x04, 0x83]);
        // add r11, rax                              49 01 C3
        self.emit(&[0x49, 0x01, 0xC3]);
        // jmp r11                                   41 FF E3
        self.emit(&[0x41, 0xFF, 0xE3]);
    }

    fn ret_done(&mut self) {
        // mov rax, r9                               49 8B C1
        self.emit(&[0x49, 0x8B, 0xC1]);
        // ret                                       C3
        self.emit(&[0xC3]);
    }

    fn class_table(&mut self, l: Label, table: &[u8; 256]) {
        self.bind(l);
        self.emit(table);
    }

    fn jump_table(&mut self, l: Label, entries: &[Label]) {
        // 4-byte align (x86 tolerates unaligned, but aligned is free here).
        while !self.code.len().is_multiple_of(4) {
            self.code.push(0);
        }
        self.bind(l);
        let table_off = self.code.len() as u32;
        for &target in entries {
            let field = self.code.len() as u32;
            self.emit(&[0, 0, 0, 0]);
            self.fixups.push(Fixup::TableWord {
                field,
                label: target,
                table_off,
            });
        }
    }

    fn cap_prologue(
        &mut self,
        classtab: Label,
        start_anchored: Label,
        start_unanchored: Label,
        warm: Option<(Label, usize)>,
    ) {
        // best_snap (arg 4 = r8) lives in callee-saved rbx for the whole run.
        self.emit(&[0x53]); // push rbx
        self.emit(&[0x4C, 0x89, 0xC3]); // mov rbx, r8   (best_snap, before r8 reused)
        // mov r9, -1           (acc_end = u64::MAX sentinel)  49 C7 C1 FF FF FF FF
        self.emit(&[0x49, 0xC7, 0xC1, 0xFF, 0xFF, 0xFF, 0xFF]);
        self.load_classtab(classtab); // overwrites r8 with the class table
        self.warm_or_start(warm, start_anchored, start_unanchored);
    }

    fn cap_record_accept(&mut self, state_id: u32, is_fallback: bool) {
        // mov r9, rdx          (acc_end = pos)       49 89 D1
        self.emit(&[0x49, 0x89, 0xD1]);
        // mov r10d, state_id                         41 BA <imm32>
        self.emit(&[0x41, 0xBA]);
        self.emit_u32(state_id);
        if is_fallback {
            // or r10d, 0x8000_0000  (snapshot flag -> bit 63 of the return)
            self.emit(&[0x41, 0x81, 0xCA]);
            self.emit_u32(0x8000_0000);
        }
    }

    fn cap_snapshot(&mut self, width: u32) {
        // for i in 0..width { best_snap[i] = marks[i] }  (u64 lanes)
        self.emit(&[0x31, 0xC0]); // xor eax, eax   (i = 0)
        let loop_top = self.code.len() as u32;
        self.emit(&[0x4C, 0x8B, 0x1C, 0xC1]); // mov r11, [rcx + rax*8]
        self.emit(&[0x4C, 0x89, 0x1C, 0xC3]); // mov [rbx + rax*8], r11
        self.emit(&[0xFF, 0xC0]); // inc eax
        self.emit(&[0x3D]); // cmp eax, imm32
        self.emit_u32(width);
        // jb loop_top   (0F 82 <rel32>, backward)
        self.emit(&[0x0F, 0x82]);
        let field = self.code.len() as u32;
        self.emit(&[0, 0, 0, 0]);
        let rel = (loop_top as i64 - (field as i64 + 4)) as i32 as u32;
        write_u32(&mut self.code, field, rel);
    }

    fn cap_move_stub(&mut self, curpos_idx: u32, moves: &[(u16, u16)], target: Label) {
        // u64 lanes (×8). A disp8 form (offset < 128, i.e. lanes 0..15) is a
        // 4-byte instruction — smaller than the disp32 form (7 bytes) and than the
        // former u32 disp32 mov (6 bytes) — which offsets the REX.W width cost and
        // keeps the hot loop dense. `op` = 89 store / 8B load; `reg_bits` is the
        // ModRM reg field already positioned (bits 5..3).
        fn mark_mem(asm: &mut X86_64Asm, op: u8, reg_bits: u8, lane: u16) {
            let off = lane as u32 * 8;
            if off < 128 {
                // REX.W op modrm(mod=01, reg, rm=rcx=001) disp8
                asm.emit(&[0x48, op, 0x40 | reg_bits | 0x01, off as u8]);
            } else {
                // REX.W op modrm(mod=10, reg, rm=rcx=001) disp32
                asm.emit(&[0x48, op, 0x80 | reg_bits | 0x01]);
                asm.emit_u32(off);
            }
        }
        for &(dst, src) in moves {
            if src as u32 == curpos_idx {
                mark_mem(self, 0x89, 0b010_000, dst); // marks[dst] = pos (rdx)
            } else {
                mark_mem(self, 0x8B, 0b000_000, src); // rax = marks[src]
                mark_mem(self, 0x89, 0b000_000, dst); // marks[dst] = rax
            }
        }
        self.jmp(target); // E9 <rel32>
    }

    fn cap_done(&mut self) {
        // Return CaptureResult { end: rax, meta: rdx }. acc_end (r9) is the full
        // 64-bit match end; acc_state (r10) is the meta word (state + snapshot bit
        // 31). No accept => acc_end still holds the -1 sentinel => meta = u64::MAX.
        // cmp r9, -1          (acc_end == sentinel?)  49 83 F9 FF
        self.emit(&[0x49, 0x83, 0xF9, 0xFF]);
        // je no_match                                0F 84 <rel32>
        let no_match = self.labels.fresh();
        self.emit(&[0x0F, 0x84]);
        self.emit_rel32(no_match);
        self.emit(&[0x4C, 0x89, 0xC8]); // mov rax, r9    (end)
        self.emit(&[0x4C, 0x89, 0xD2]); // mov rdx, r10   (meta)
        self.emit(&[0x5B]); // pop rbx
        self.emit(&[0xC3]); // ret
        // no_match: mov rdx, -1 ; pop rbx ; ret   (rax is don't-care)
        self.bind(no_match);
        self.emit(&[0x48, 0xC7, 0xC2, 0xFF, 0xFF, 0xFF, 0xFF]); // mov rdx, -1
        self.emit(&[0x5B]); // pop rbx
        self.emit(&[0xC3]);
    }

    fn finish(mut self) -> Vec<u8> {
        for fx in &self.fixups {
            match *fx {
                Fixup::Rel32 { field, label } => {
                    let target = self.labels.offset_of(label) as i64;
                    let rel = (target - (field as i64 + 4)) as i32 as u32;
                    write_u32(&mut self.code, field, rel);
                }
                Fixup::TableWord {
                    field,
                    label,
                    table_off,
                } => {
                    let target = self.labels.offset_of(label) as i64;
                    let rel = (target - table_off as i64) as i32 as u32;
                    write_u32(&mut self.code, field, rel);
                }
            }
        }
        self.code
    }
}

impl X86_64Asm {
    /// `lea r8, [rip + classtab]` — shared by both prologues.
    fn load_classtab(&mut self, classtab: Label) {
        self.emit(&[0x4C, 0x8D, 0x05]); // lea r8, [rip + disp32]
        self.emit_rel32(classtab);
    }

    /// `test rdx, rdx; jz anchored; jmp unanchored` — shared by both prologues.
    /// When the two starts coincide the `start` test is dead, so emit a single
    /// unconditional `jmp`.
    fn start_dispatch(&mut self, start_anchored: Label, start_unanchored: Label) {
        if start_anchored == start_unanchored {
            self.jmp(start_anchored);
            return;
        }
        self.emit(&[0x48, 0x85, 0xD2]); // test rdx, rdx
        self.emit(&[0x0F, 0x84]); // jz start_anchored
        self.emit_rel32(start_anchored);
        self.jmp(start_unanchored);
    }

    /// Tail of both prologues: either warm-start past the prefilter-matched
    /// prefix (`add rdx, len; jmp post_block`) or the normal start dispatch.
    fn warm_or_start(
        &mut self,
        warm: Option<(Label, usize)>,
        start_anchored: Label,
        start_unanchored: Label,
    ) {
        match warm {
            Some((post, len)) => {
                if len < 128 {
                    self.emit(&[0x48, 0x83, 0xC2, len as u8]); // add rdx, imm8
                } else {
                    self.emit(&[0x48, 0x81, 0xC2]); // add rdx, imm32
                    self.emit_u32(len as u32);
                }
                self.jmp(post); // jmp post_block
            }
            None => self.start_dispatch(start_anchored, start_unanchored),
        }
    }
}

/// `cmp eax, imm`, using the 3-byte imm8 form when `imm` fits an unsigned byte
/// compare (≤ 127, so the sign-extension is harmless) and the 5-byte `eax`-form
/// imm32 otherwise. eax is not modified.
fn emit_cmp_eax_imm(asm: &mut X86_64Asm, imm: u32) {
    if imm <= 127 {
        asm.emit(&[0x83, 0xF8, imm as u8]); // cmp eax, imm8
    } else {
        asm.emit(&[0x3D]); // cmp eax, imm32
        asm.emit_u32(imm);
    }
}

fn write_u32(code: &mut [u8], at: u32, val: u32) {
    let at = at as usize;
    code[at..at + 4].copy_from_slice(&val.to_le_bytes());
}
