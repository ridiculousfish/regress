//! x86-64 (System V AMD64 ABI) encoder for the TDFA JIT capture-free tier.
//!
//! Fixed register map (all caller-saved; the function is a leaf, no frame):
//!
//! | role       | reg | notes                                  |
//! |------------|-----|----------------------------------------|
//! | `input`    | rdi | arg 0, base pointer                    |
//! | `end`      | rsi | arg 1, `len`                           |
//! | `pos`      | rdx | arg 2 = `start`, then incremented      |
//! | `acc`      | r8  | last accept end, init `usize::MAX`     |
//! | `classtab` | r9  | base of the byte→class table           |
//! | byte/class | rcx | scratch (cl/ecx: byte, then class)     |
//! | jt base    | r10 | scratch (jump-table address / target)  |
//! | offset     | r11 | scratch (sign-extended table entry)    |
//! | return     | rax | set to `acc` at `done`                 |
//!
//! Like the aarch64 encoder, every reference resolves to a PC-relative (RIP
//! disp32, jcc/jmp rel32) or table-relative (32-bit jump-table word) value, so
//! the finished code is position-independent — no relocation after mapping.

use super::asm::{Assembler, Label, Labels};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// A pending patch applied in [`finish`].
enum Fixup {
    /// 32-bit field at `field` holding `label - (field + 4)` (RIP-relative
    /// `lea`/`jcc`/`jmp` displacement — the field is always the last 4 bytes of
    /// the instruction).
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

    /// Append a 4-byte placeholder and record a `Rel32` fixup for `label` over
    /// it (the placeholder is the last 4 bytes of the current instruction).
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
        // mov r8, -1            (acc = usize::MAX)  49 C7 C0 FF FF FF FF
        self.emit(&[0x49, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF]);
        // lea r9, [rip + classtab]                  4C 8D 0D <disp32>
        self.emit(&[0x4C, 0x8D, 0x0D]);
        self.emit_rel32(classtab);
        // test rdx, rdx                             48 85 D2
        self.emit(&[0x48, 0x85, 0xD2]);
        // jz start_anchored                         0F 84 <rel32>
        self.emit(&[0x0F, 0x84]);
        self.emit_rel32(start_anchored);
        // jmp start_unanchored                      E9 <rel32>
        self.emit(&[0xE9]);
        self.emit_rel32(start_unanchored);
    }

    fn record_accept(&mut self) {
        // mov r8, rdx          (acc = pos)          49 89 D0
        self.emit(&[0x49, 0x89, 0xD0]);
    }

    fn eoi_check(&mut self, done: Label) {
        // cmp rdx, rsi                              48 39 F2
        self.emit(&[0x48, 0x39, 0xF2]);
        // jae done             (pos >= end)         0F 83 <rel32>
        self.emit(&[0x0F, 0x83]);
        self.emit_rel32(done);
    }

    fn fetch_and_classify(&mut self) {
        // movzx ecx, byte [rdi + rdx]               0F B6 0C 17
        self.emit(&[0x0F, 0xB6, 0x0C, 0x17]);
        // inc rdx                                   48 FF C2
        self.emit(&[0x48, 0xFF, 0xC2]);
        // movzx ecx, byte [r9 + rcx]                41 0F B6 0C 09
        self.emit(&[0x41, 0x0F, 0xB6, 0x0C, 0x09]);
    }

    fn dispatch(&mut self, jump_table: Label) {
        // lea r10, [rip + jump_table]               4C 8D 15 <disp32>
        self.emit(&[0x4C, 0x8D, 0x15]);
        self.emit_rel32(jump_table);
        // movsxd r11, dword [r10 + rcx*4]           4D 63 1C 8A
        self.emit(&[0x4D, 0x63, 0x1C, 0x8A]);
        // add r10, r11                              4D 01 DA
        self.emit(&[0x4D, 0x01, 0xDA]);
        // jmp r10                                   41 FF E2
        self.emit(&[0x41, 0xFF, 0xE2]);
    }

    fn ret_done(&mut self) {
        // mov rax, r8                               4C 89 C0
        self.emit(&[0x4C, 0x89, 0xC0]);
        // ret                                       C3
        self.emit(&[0xC3]);
    }

    fn class_table(&mut self, l: Label, table: &[u8; 256]) {
        self.bind(l);
        self.emit(table);
    }

    fn jump_table(&mut self, l: Label, entries: &[Label]) {
        // 4-byte align so the dword loads are aligned (x86 tolerates unaligned,
        // but aligned is free here).
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

fn write_u32(code: &mut [u8], at: u32, val: u32) {
    let at = at as usize;
    code[at..at + 4].copy_from_slice(&val.to_le_bytes());
}
