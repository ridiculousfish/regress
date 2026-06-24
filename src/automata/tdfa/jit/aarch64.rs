//! AArch64 (ARM64) encoder for the TDFA JIT capture-free tier.
//!
//! Fixed register map (all caller-saved; the function is a leaf with no frame):
//!
//! | role      | reg | notes                                   |
//! |-----------|-----|-----------------------------------------|
//! | `input`   | x0  | arg 0, base pointer (preserved)         |
//! | `end`     | x1  | arg 1, `len`                            |
//! | `pos`     | x2  | arg 2 = `start`, then incremented       |
//! | `acc`     | x3  | last accept end, init `usize::MAX`      |
//! | `classtab`| x4  | base of the byte→class table            |
//! | byte/class| x5  | scratch (w5: byte, then class)          |
//! | jt base   | x6  | scratch (jump-table address / target)   |
//! | offset    | x7  | scratch (signed jump-table entry)       |
//!
//! All label references resolve to *offset-relative* immediates (adrp/add page
//! deltas, branch displacements, and 32-bit relative jump-table words), so the
//! finished code is position-independent — no relocation after mapping.

use super::asm::{Assembler, Label, Labels};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

// Register numbers.
const INPUT: u32 = 0;
const END: u32 = 1;
const POS: u32 = 2;
const ACC: u32 = 3;
const CLASSTAB: u32 = 4;
const BYTE: u32 = 5; // also holds class after the second load
const JT: u32 = 6;
const OFF: u32 = 7;

// Capture tier: the mark-file pointer (arg 3) reuses x3 (the capture-free tier's
// `acc`), and the accept bookkeeping moves to x9/x10. The shared per-byte
// methods (`fetch_and_classify`, `dispatch`, `eoi_check`) never touch x3/x9/x10,
// so they serve both tiers unchanged.
const MARKS: u32 = 3;
const ACC_END: u32 = 9;
const ACC_STATE: u32 = 10;
const MOVE_TMP: u32 = 7; // shared with OFF; only used in move stubs

const XZR: u32 = 31;

/// A pending patch applied in [`finish`] once every label is bound.
enum Fixup {
    /// `adrp`/`add` pair at `at` / `at+4` loading `label`'s address.
    AdrpAdd { at: u32, label: Label },
    /// 19-bit conditional/`cbz` displacement at `at`.
    Cond19 { at: u32, label: Label },
    /// 26-bit `b` displacement at `at`.
    Branch26 { at: u32, label: Label },
    /// A 32-bit jump-table word at `at`: `label_off - table_off`.
    TableWord { at: u32, label: Label, table_off: u32 },
}

pub(crate) struct Aarch64Asm {
    code: Vec<u8>,
    labels: Labels,
    fixups: Vec<Fixup>,
}

impl Aarch64Asm {
    fn emit_u32(&mut self, insn: u32) {
        self.code.extend_from_slice(&insn.to_le_bytes());
    }

    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    /// `MOVN Xd, #0` → `Xd = !0 = usize::MAX`.
    fn movn_zero(&mut self, rd: u32) {
        self.emit_u32(0x9280_0000 | rd);
    }

    /// `ORR Xd, XZR, Xm` (i.e. `MOV Xd, Xm`).
    fn mov_reg(&mut self, rd: u32, rm: u32) {
        self.emit_u32(0xAA00_0000 | (rm << 16) | (XZR << 5) | rd);
    }

    /// `ADRP Xd, label` + `ADD Xd, Xd, #:lo12:label` (patched in `finish`).
    fn adrp_add(&mut self, rd: u32, label: Label) {
        let at = self.here();
        self.emit_u32(0x9000_0000 | rd); // ADRP (imm patched later)
        self.emit_u32(0x9100_0000 | (rd << 5) | rd); // ADD Xd, Xd, #0
        self.fixups.push(Fixup::AdrpAdd { at, label });
    }

    /// `CBZ Xt, label`.
    fn cbz(&mut self, rt: u32, label: Label) {
        let at = self.here();
        self.emit_u32(0xB400_0000 | rt);
        self.fixups.push(Fixup::Cond19 { at, label });
    }

    /// `B label`.
    fn b(&mut self, label: Label) {
        let at = self.here();
        self.emit_u32(0x1400_0000);
        self.fixups.push(Fixup::Branch26 { at, label });
    }

    /// `B.HS label` (unsigned ≥, condition code `0b0010`).
    fn b_hs(&mut self, label: Label) {
        let at = self.here();
        self.emit_u32(0x5400_0000 | 0x2);
        self.fixups.push(Fixup::Cond19 { at, label });
    }

    /// `CMP Xn, Xm` (SUBS XZR, Xn, Xm).
    fn cmp(&mut self, rn: u32, rm: u32) {
        self.emit_u32(0xEB00_0000 | (rm << 16) | (rn << 5) | XZR);
    }

    /// `LDRB Wt, [Xn, Xm]` (UXTX, LSL #0).
    fn ldrb(&mut self, rt: u32, rn: u32, rm: u32) {
        self.emit_u32(0x3860_6800 | (rm << 16) | (rn << 5) | rt);
    }

    /// `ADD Xd, Xn, #imm12`.
    fn add_imm(&mut self, rd: u32, rn: u32, imm12: u32) {
        debug_assert!(imm12 < 0x1000);
        self.emit_u32(0x9100_0000 | (imm12 << 10) | (rn << 5) | rd);
    }

    /// `LDRSW Xt, [Xn, Xm, LSL #2]`.
    fn ldrsw_lsl2(&mut self, rt: u32, rn: u32, rm: u32) {
        self.emit_u32(0xB8A0_7800 | (rm << 16) | (rn << 5) | rt);
    }

    /// `ADD Xd, Xn, Xm` (shifted register, LSL #0).
    fn add_reg(&mut self, rd: u32, rn: u32, rm: u32) {
        self.emit_u32(0x8B00_0000 | (rm << 16) | (rn << 5) | rd);
    }

    /// `BR Xn`.
    fn br(&mut self, rn: u32) {
        self.emit_u32(0xD61F_0000 | (rn << 5));
    }

    /// `RET` (x30).
    fn ret(&mut self) {
        self.emit_u32(0xD65F_03C0);
    }

    /// `MOVN Wd, #0` → `Wd = 0xFFFF_FFFF` (zero-extended into Xd).
    fn movn_w_zero(&mut self, rd: u32) {
        self.emit_u32(0x1280_0000 | rd);
    }

    /// `MOVZ Xd, #imm16` (LSL #0).
    fn movz_x(&mut self, rd: u32, imm16: u32) {
        debug_assert!(imm16 < 0x1_0000);
        self.emit_u32(0xD280_0000 | (imm16 << 5) | rd);
    }

    /// `STR Wt, [Xn, #imm12*4]` (32-bit, unsigned scaled offset).
    fn str_w(&mut self, rt: u32, rn: u32, imm12: u32) {
        debug_assert!(imm12 < 0x1000);
        self.emit_u32(0xB900_0000 | (imm12 << 10) | (rn << 5) | rt);
    }

    /// `LDR Wt, [Xn, #imm12*4]` (32-bit, unsigned scaled offset).
    fn ldr_w(&mut self, rt: u32, rn: u32, imm12: u32) {
        debug_assert!(imm12 < 0x1000);
        self.emit_u32(0xB940_0000 | (imm12 << 10) | (rn << 5) | rt);
    }

    /// `CMN Wn, #1` (ADDS WZR, Wn, #1): sets Z iff `Wn == 0xFFFF_FFFF`.
    fn cmn_w1(&mut self, rn: u32) {
        self.emit_u32(0x3100_0000 | (1 << 10) | (rn << 5) | XZR);
    }

    /// `B.EQ label`.
    fn b_eq(&mut self, label: Label) {
        let at = self.here();
        self.emit_u32(0x5400_0000); // cond = EQ = 0
        self.fixups.push(Fixup::Cond19 { at, label });
    }

    /// `ORR Xd, Xn, Xm, LSL #32`.
    fn orr_lsl32(&mut self, rd: u32, rn: u32, rm: u32) {
        self.emit_u32(0xAA00_0000 | (rm << 16) | (32 << 10) | (rn << 5) | rd);
    }

    /// Pad with zero bytes until the emission offset is 4-byte aligned. Code is
    /// already 4-aligned; this guards data tables emitted after odd-length runs
    /// (there are none today, but keeps jump tables naturally aligned).
    fn align4(&mut self) {
        while !self.code.len().is_multiple_of(4) {
            self.code.push(0);
        }
    }
}

impl Assembler for Aarch64Asm {
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
        self.movn_zero(ACC); // acc = usize::MAX
        self.adrp_add(CLASSTAB, classtab); // classtab = &table
        // start == 0 ? anchored : unanchored
        self.cbz(POS, start_anchored);
        self.b(start_unanchored);
    }

    fn record_accept(&mut self) {
        self.mov_reg(ACC, POS); // acc = pos
    }

    fn eoi_check(&mut self, done: Label) {
        self.cmp(POS, END);
        self.b_hs(done); // pos >= end -> done
    }

    fn fetch_and_classify(&mut self) {
        self.ldrb(BYTE, INPUT, POS); // byte = input[pos]
        self.add_imm(POS, POS, 1); // pos += 1
        self.ldrb(BYTE, CLASSTAB, BYTE); // class = classtab[byte]
    }

    fn dispatch(&mut self, jump_table: Label) {
        self.adrp_add(JT, jump_table); // jt = &table
        self.ldrsw_lsl2(OFF, JT, BYTE); // off = jt[class]
        self.add_reg(JT, JT, OFF); // target = jt + off
        self.br(JT);
    }

    fn ret_done(&mut self) {
        self.mov_reg(INPUT, ACC); // x0 = acc (return reg)
        self.ret();
    }

    fn class_table(&mut self, l: Label, table: &[u8; 256]) {
        self.align4();
        self.bind(l);
        self.code.extend_from_slice(table);
    }

    fn jump_table(&mut self, l: Label, entries: &[Label]) {
        self.align4();
        self.bind(l);
        let table_off = self.code.len() as u32;
        for &target in entries {
            let at = self.here();
            self.emit_u32(0); // patched in finish
            self.fixups.push(Fixup::TableWord {
                at,
                label: target,
                table_off,
            });
        }
    }

    fn cap_prologue(&mut self, classtab: Label, start_anchored: Label, start_unanchored: Label) {
        self.movn_w_zero(ACC_END); // acc_end = 0xFFFF_FFFF (no accept)
        self.adrp_add(CLASSTAB, classtab);
        self.cbz(POS, start_anchored);
        self.b(start_unanchored);
    }

    fn cap_record_accept(&mut self, state_id: u32) {
        self.mov_reg(ACC_END, POS); // acc_end = pos
        self.movz_x(ACC_STATE, state_id); // acc_state = state_id
    }

    fn cap_move_stub(&mut self, curpos_idx: u32, moves: &[(u16, u16)], target: Label) {
        // Stamp current position into the curpos lane, then apply the moves.
        self.str_w(POS, MARKS, curpos_idx);
        for &(dst, src) in moves {
            self.ldr_w(MOVE_TMP, MARKS, src as u32);
            self.str_w(MOVE_TMP, MARKS, dst as u32);
        }
        self.b(target);
    }

    fn cap_done(&mut self) {
        // if acc_end == 0xFFFF_FFFF (no accept) -> return u64::MAX
        self.cmn_w1(ACC_END);
        let no_match = self.labels.fresh();
        self.b_eq(no_match);
        // x0 = (acc_state << 32) | acc_end
        self.orr_lsl32(INPUT, ACC_END, ACC_STATE);
        self.ret();
        self.bind(no_match);
        self.emit_u32(0x9280_0000); // MOVN x0, #0  -> x0 = u64::MAX
        self.ret();
    }

    fn finish(mut self) -> Vec<u8> {
        for fx in &self.fixups {
            match *fx {
                Fixup::AdrpAdd { at, label } => {
                    let target = self.labels.offset_of(label) as i64;
                    let adrp_at = at as i64;
                    // Page delta is base-independent because the mapping is
                    // page-aligned: ((base+target)&!0xFFF) - ((base+adrp)&!0xFFF)
                    // collapses to the offset-only difference.
                    let page = ((target & !0xFFF) - (adrp_at & !0xFFF)) >> 12;
                    let immlo = (page & 0x3) as u32;
                    let immhi = ((page >> 2) & 0x7_FFFF) as u32;
                    let mut adrp = read_u32(&self.code, at);
                    adrp &= !((0x3 << 29) | (0x7_FFFF << 5));
                    adrp |= (immlo << 29) | (immhi << 5);
                    write_u32(&mut self.code, at, adrp);

                    let lo12 = (target & 0xFFF) as u32;
                    let mut add = read_u32(&self.code, at + 4);
                    add &= !(0xFFF << 10);
                    add |= lo12 << 10;
                    write_u32(&mut self.code, at + 4, add);
                }
                Fixup::Cond19 { at, label } => {
                    let target = self.labels.offset_of(label) as i64;
                    let imm19 = ((target - at as i64) >> 2) as u32 & 0x7_FFFF;
                    let mut insn = read_u32(&self.code, at);
                    insn &= !(0x7_FFFF << 5);
                    insn |= imm19 << 5;
                    write_u32(&mut self.code, at, insn);
                }
                Fixup::Branch26 { at, label } => {
                    let target = self.labels.offset_of(label) as i64;
                    let imm26 = ((target - at as i64) >> 2) as u32 & 0x3FF_FFFF;
                    let mut insn = read_u32(&self.code, at);
                    insn &= !0x3FF_FFFF;
                    insn |= imm26;
                    write_u32(&mut self.code, at, insn);
                }
                Fixup::TableWord {
                    at,
                    label,
                    table_off,
                } => {
                    let target = self.labels.offset_of(label) as i64;
                    let rel = (target - table_off as i64) as i32 as u32;
                    write_u32(&mut self.code, at, rel);
                }
            }
        }
        self.code
    }
}

fn read_u32(code: &[u8], at: u32) -> u32 {
    let at = at as usize;
    u32::from_le_bytes([code[at], code[at + 1], code[at + 2], code[at + 3]])
}

fn write_u32(code: &mut [u8], at: u32, val: u32) {
    let at = at as usize;
    code[at..at + 4].copy_from_slice(&val.to_le_bytes());
}
