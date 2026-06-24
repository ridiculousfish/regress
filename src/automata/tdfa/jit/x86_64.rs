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
}

impl X86_64Asm {
    fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    fn emit_u32(&mut self, v: u32) {
        self.code.extend_from_slice(&v.to_le_bytes());
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
        }
    }

    fn fresh_label(&mut self) -> Label {
        self.labels.fresh()
    }

    fn bind(&mut self, l: Label) {
        let off = self.code.len();
        self.labels.bind(l, off);
    }

    fn prologue(&mut self, classtab: Label, start_anchored: Label, start_unanchored: Label) {
        // mov r9, -1            (acc = usize::MAX)  49 C7 C1 FF FF FF FF
        self.emit(&[0x49, 0xC7, 0xC1, 0xFF, 0xFF, 0xFF, 0xFF]);
        self.load_classtab(classtab);
        self.start_dispatch(start_anchored, start_unanchored);
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

    fn fetch_and_classify(&mut self) {
        // movzx eax, byte [rdi + rdx]               0F B6 04 17
        self.emit(&[0x0F, 0xB6, 0x04, 0x17]);
        // inc rdx                                   48 FF C2
        self.emit(&[0x48, 0xFF, 0xC2]);
        // movzx eax, byte [r8 + rax]                41 0F B6 04 00
        self.emit(&[0x41, 0x0F, 0xB6, 0x04, 0x00]);
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

    fn cap_prologue(&mut self, classtab: Label, start_anchored: Label, start_unanchored: Label) {
        // mov r9d, -1          (acc_end = 0xFFFF_FFFF)  41 B9 FF FF FF FF
        self.emit(&[0x41, 0xB9, 0xFF, 0xFF, 0xFF, 0xFF]);
        self.load_classtab(classtab);
        self.start_dispatch(start_anchored, start_unanchored);
    }

    fn cap_record_accept(&mut self, state_id: u32) {
        // mov r9, rdx          (acc_end = pos)       49 89 D1
        self.emit(&[0x49, 0x89, 0xD1]);
        // mov r10d, state_id                         41 BA <imm32>
        self.emit(&[0x41, 0xBA]);
        self.emit_u32(state_id);
    }

    fn cap_move_stub(&mut self, curpos_idx: u32, moves: &[(u16, u16)], target: Label) {
        // mov [rcx + curpos_idx*4], edx              89 91 <disp32>
        self.emit(&[0x89, 0x91]);
        self.emit_u32(curpos_idx * 4);
        for &(dst, src) in moves {
            // mov eax, [rcx + src*4]                 8B 81 <disp32>
            self.emit(&[0x8B, 0x81]);
            self.emit_u32(src as u32 * 4);
            // mov [rcx + dst*4], eax                 89 81 <disp32>
            self.emit(&[0x89, 0x81]);
            self.emit_u32(dst as u32 * 4);
        }
        // jmp target                                 E9 <rel32>
        self.emit(&[0xE9]);
        self.emit_rel32(target);
    }

    fn cap_done(&mut self) {
        // cmp r9d, -1          (acc_end == 0xFFFF_FFFF?)  41 83 F9 FF
        self.emit(&[0x41, 0x83, 0xF9, 0xFF]);
        // je no_match                                0F 84 <rel32>
        let no_match = self.labels.fresh();
        self.emit(&[0x0F, 0x84]);
        self.emit_rel32(no_match);
        // rax = (r10 << 32) | r9
        self.emit(&[0x49, 0x8B, 0xC2]); // mov rax, r10
        self.emit(&[0x48, 0xC1, 0xE0, 0x20]); // shl rax, 32
        self.emit(&[0x49, 0x0B, 0xC1]); // or rax, r9
        self.emit(&[0xC3]); // ret
        // no_match: mov rax, -1 ; ret
        self.bind(no_match);
        self.emit(&[0x48, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF]);
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
    fn start_dispatch(&mut self, start_anchored: Label, start_unanchored: Label) {
        self.emit(&[0x48, 0x85, 0xD2]); // test rdx, rdx
        self.emit(&[0x0F, 0x84]); // jz start_anchored
        self.emit_rel32(start_anchored);
        self.emit(&[0xE9]); // jmp start_unanchored
        self.emit_rel32(start_unanchored);
    }
}

fn write_u32(code: &mut [u8], at: u32, val: u32) {
    let at = at as usize;
    code[at..at + 4].copy_from_slice(&val.to_le_bytes());
}
