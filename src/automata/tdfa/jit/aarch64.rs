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
//! All label references resolve to *offset-relative* immediates (`adr`
//! PC-relative offsets, branch displacements, and 32-bit relative jump-table
//! words), so the finished code is position-independent — no relocation after
//! mapping.

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
const BEST_SNAP: u32 = 11; // capture tier: snapshot destination (from arg 4)
const MOVE_TMP: u32 = 7; // shared with OFF; only used in move/snapshot code
const SNAP_CTR: u32 = 6; // snapshot loop counter (shared with BYTE; free at accept)

const XZR: u32 = 31;

// AArch64 condition codes.
const COND_EQ: u32 = 0;
const COND_LO: u32 = 3; // unsigned lower (<)
const COND_LS: u32 = 9; // unsigned lower or same (<=)

/// The 4th integer argument register (x4) — holds `best_snap` on entry, before
/// the prologue reuses x4 for the class table.
const ARG4: u32 = 4;

/// A pending patch applied in [`finish`] once every label is bound.
enum Fixup {
    /// `adr` at `at` loading `label`'s PC-relative address (±1 MiB).
    Adr { at: u32, label: Label },
    /// 19-bit conditional/`cbz` displacement at `at`.
    Cond19 { at: u32, label: Label },
    /// 26-bit `b` displacement at `at`.
    Branch26 { at: u32, label: Label },
    /// A 32-bit jump-table word at `at`: `label_off - table_off`.
    TableWord {
        at: u32,
        label: Label,
        table_off: u32,
    },
}

pub(crate) struct Aarch64Asm {
    code: Vec<u8>,
    labels: Labels,
    fixups: Vec<Fixup>,
    /// The most recently emitted unconditional `b` — `(offset of the insn,
    /// target)` — set only while it's the last thing emitted (any other emission
    /// clears it). Lets [`bind`](Self::bind) elide a branch to the next
    /// instruction (fall-through).
    pending_b: Option<(u32, Label)>,
}

impl Aarch64Asm {
    fn emit_u32(&mut self, insn: u32) {
        self.pending_b = None;
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

    /// `ADR Xd, label` — PC-relative address in a single instruction (±1 MiB,
    /// always satisfied within our code budget; patched in `finish`).
    fn adr_addr(&mut self, rd: u32, label: Label) {
        let at = self.here();
        self.emit_u32(0x1000_0000 | rd); // ADR (imm patched later)
        self.fixups.push(Fixup::Adr { at, label });
    }

    /// `CBZ Xt, label`.
    fn cbz(&mut self, rt: u32, label: Label) {
        let at = self.here();
        self.emit_u32(0xB400_0000 | rt);
        self.fixups.push(Fixup::Cond19 { at, label });
    }

    /// `B label`, remembered so a following `bind(label)` can drop it as a
    /// branch to the next instruction. The only unconditional-branch site.
    fn b(&mut self, label: Label) {
        let at = self.here();
        self.emit_u32(0x1400_0000);
        self.fixups.push(Fixup::Branch26 { at, label });
        self.pending_b = Some((at, label));
    }

    /// Branch to the start state: `cbz pos, anchored; b unanchored` (start == 0 ?
    /// anchored : unanchored). When the two coincide the `start` test is dead, so
    /// emit a single unconditional `b`. Shared by both prologues.
    fn start_dispatch(&mut self, start_anchored: Label, start_unanchored: Label) {
        if start_anchored == start_unanchored {
            self.b(start_anchored);
        } else {
            self.cbz(POS, start_anchored);
            self.b(start_unanchored);
        }
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

    /// `MOVZ Xd, #imm16` (LSL #0).
    fn movz_x(&mut self, rd: u32, imm16: u32) {
        debug_assert!(imm16 < 0x1_0000);
        self.emit_u32(0xD280_0000 | (imm16 << 5) | rd);
    }

    /// `MOVK Xd, #imm16, LSL #16` (keeps the other bits).
    fn movk_x_hi16(&mut self, rd: u32, imm16: u32) {
        debug_assert!(imm16 < 0x1_0000);
        self.emit_u32(0xF2A0_0000 | (imm16 << 5) | rd);
    }

    /// `CMN Xn, #1` (ADDS XZR, Xn, #1): sets Z iff `Xn == u64::MAX`.
    fn cmn_x1(&mut self, rn: u32) {
        self.emit_u32(0xB100_0000 | (1 << 10) | (rn << 5) | XZR);
    }

    /// `LDR Xt, [Xn, Xm, LSL #3]` (64-bit register offset).
    fn ldr_x_idx(&mut self, rt: u32, rn: u32, rm: u32) {
        self.emit_u32(0xF860_7800 | (rm << 16) | (rn << 5) | rt);
    }

    /// `STR Xt, [Xn, Xm, LSL #3]` (64-bit register offset).
    fn str_x_idx(&mut self, rt: u32, rn: u32, rm: u32) {
        self.emit_u32(0xF820_7800 | (rm << 16) | (rn << 5) | rt);
    }

    /// `STR Xt, [Xn, #imm12*8]` (64-bit, unsigned scaled offset).
    fn str_x(&mut self, rt: u32, rn: u32, imm12: u32) {
        debug_assert!(imm12 < 0x1000);
        self.emit_u32(0xF900_0000 | (imm12 << 10) | (rn << 5) | rt);
    }

    /// `LDR Xt, [Xn, #imm12*8]` (64-bit, unsigned scaled offset).
    fn ldr_x(&mut self, rt: u32, rn: u32, imm12: u32) {
        debug_assert!(imm12 < 0x1000);
        self.emit_u32(0xF940_0000 | (imm12 << 10) | (rn << 5) | rt);
    }

    /// `B.<cond> label`.
    fn b_cond(&mut self, cond: u32, label: Label) {
        let at = self.here();
        self.emit_u32(0x5400_0000 | cond);
        self.fixups.push(Fixup::Cond19 { at, label });
    }

    /// `B.EQ label`.
    fn b_eq(&mut self, label: Label) {
        self.b_cond(COND_EQ, label);
    }

    /// `CMP Wn, #imm12` (SUBS WZR, Wn, #imm).
    fn cmp_imm_w(&mut self, rn: u32, imm12: u32) {
        debug_assert!(imm12 < 0x1000);
        self.emit_u32(0x7100_0000 | (imm12 << 10) | (rn << 5) | XZR);
    }

    /// `SUB Wd, Wn, #imm12`.
    fn sub_imm_w(&mut self, rd: u32, rn: u32, imm12: u32) {
        debug_assert!(imm12 < 0x1000);
        self.emit_u32(0x5100_0000 | (imm12 << 10) | (rn << 5) | rd);
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
            pending_b: None,
        }
    }

    fn offset(&self) -> usize {
        self.code.len()
    }

    fn fresh_label(&mut self) -> Label {
        self.labels.fresh()
    }

    fn bind(&mut self, l: Label) {
        // If the last thing emitted was `b l`, it's a branch to the next
        // instruction — drop it and let control fall through.
        if let Some((at, target)) = self.pending_b.take() {
            if target == l && at as usize + 4 == self.code.len() {
                self.code.truncate(at as usize);
                self.fixups.pop(); // the b's Branch26, pushed last
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
        self.movn_zero(ACC); // acc = usize::MAX
        self.adr_addr(CLASSTAB, classtab); // classtab = &table
        if let Some((post, len)) = warm {
            self.add_imm(POS, POS, len as u32); // pos += len (skip the prefix)
            self.b(post); // resume in the post-prefix state
        } else {
            self.start_dispatch(start_anchored, start_unanchored);
        }
    }

    fn record_accept(&mut self) {
        self.mov_reg(ACC, POS); // acc = pos
    }

    fn record_accept_prev(&mut self) {
        // SUB x3, x2, #1  (acc = pos - 1)
        self.emit_u32(0xD100_0000 | (1 << 10) | (POS << 5) | ACC);
    }

    fn eoi_check(&mut self, done: Label) {
        self.cmp(POS, END);
        self.b_hs(done); // pos >= end -> done
    }

    fn load_byte(&mut self) {
        self.ldrb(BYTE, INPUT, POS); // byte = input[pos]
    }

    fn advance(&mut self, n: u32) {
        if n != 0 {
            self.add_imm(POS, POS, n); // pos += n
        }
    }

    fn classify(&mut self) {
        self.ldrb(BYTE, CLASSTAB, BYTE); // class = classtab[byte]
    }

    fn branch(&mut self, target: Label) {
        self.b(target);
    }

    fn dispatch_byte_ranges(&mut self, runs: &[(u8, u8, Label)], default: Label) {
        for &(lo, hi, target) in runs {
            if lo == hi {
                self.cmp_imm_w(BYTE, lo as u32); // cmp byte, lo
                self.b_cond(COND_EQ, target); // b.eq target
            } else if lo == 0 {
                // `[0..=hi]` needs no normalization: `byte <= hi` is enough.
                self.cmp_imm_w(BYTE, hi as u32); // cmp byte, hi
                self.b_cond(COND_LS, target); // b.ls target (unsigned <=)
            } else {
                self.sub_imm_w(MOVE_TMP, BYTE, lo as u32); // tmp = byte - lo
                self.cmp_imm_w(MOVE_TMP, (hi - lo) as u32); // cmp tmp, hi-lo
                self.b_cond(COND_LS, target); // b.ls target (unsigned <=)
            }
        }
        self.b(default);
    }

    fn dispatch(&mut self, jump_table: Label) {
        self.adr_addr(JT, jump_table); // jt = &table
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

    fn cap_prologue(
        &mut self,
        classtab: Label,
        start_anchored: Label,
        start_unanchored: Label,
        warm: Option<(Label, usize)>,
    ) {
        self.mov_reg(BEST_SNAP, ARG4); // best_snap = arg4 (x4), before x4 is reused
        self.movn_zero(ACC_END); // acc_end = u64::MAX sentinel (no accept)
        self.adr_addr(CLASSTAB, classtab); // overwrites x4 with the class table
        if let Some((post, len)) = warm {
            self.add_imm(POS, POS, len as u32); // pos += len (skip the prefix)
            self.b(post); // resume in the post-prefix state
        } else {
            self.start_dispatch(start_anchored, start_unanchored);
        }
    }

    fn cap_record_accept(&mut self, state_id: u32, is_fallback: bool) {
        self.mov_reg(ACC_END, POS); // acc_end = pos
        self.movz_x(ACC_STATE, state_id); // acc_state = state_id
        if is_fallback {
            // Set bit 31 of acc_state (the snapshot flag): MOVK Xd,#0x8000,LSL#16.
            self.movk_x_hi16(ACC_STATE, 0x8000);
        }
    }

    fn cap_snapshot(&mut self, width: u32) {
        // for i in 0..width { best_snap[i] = marks[i] }  (u64 lanes)
        self.movz_x(SNAP_CTR, 0); // i = 0
        let loop_top = self.code.len() as u32;
        self.ldr_x_idx(MOVE_TMP, MARKS, SNAP_CTR); // tmp = marks[i]
        self.str_x_idx(MOVE_TMP, BEST_SNAP, SNAP_CTR); // best_snap[i] = tmp
        self.add_imm(SNAP_CTR, SNAP_CTR, 1); // i += 1
        self.cmp_imm_w(SNAP_CTR, width); // cmp i, width
        // b.lo loop_top  (backward branch; compute the displacement directly)
        let at = self.here();
        let imm19 = (((loop_top as i64 - at as i64) >> 2) as u32) & 0x7_FFFF;
        self.emit_u32(0x5400_0000 | (imm19 << 5) | COND_LO);
    }

    fn cap_move_stub(&mut self, curpos_idx: u32, moves: &[(u16, u16)], target: Label) {
        // u64 lanes: STR/LDR (imm12) auto-scales the offset ×8, and lane counts
        // (≤ 4095) fit the imm12 field, so no disp split is needed here.
        for &(dst, src) in moves {
            if src as u32 == curpos_idx {
                // CurrentPos write: marks[dst] = pos, directly (no curpos lane).
                self.str_x(POS, MARKS, dst as u32);
            } else {
                self.ldr_x(MOVE_TMP, MARKS, src as u32);
                self.str_x(MOVE_TMP, MARKS, dst as u32);
            }
        }
        self.b(target);
    }

    fn cap_done(&mut self) {
        // Return CaptureResult { end: x0, meta: x1 }. acc_end (x9) is the full
        // 64-bit match end; acc_state (x10) is the meta word (state + snapshot bit
        // 31). No accept => acc_end still the sentinel => meta = u64::MAX.
        self.cmn_x1(ACC_END);
        let no_match = self.labels.fresh();
        self.b_eq(no_match);
        self.mov_reg(INPUT, ACC_END); // x0 = end
        self.mov_reg(END, ACC_STATE); // x1 = meta
        self.ret();
        self.bind(no_match);
        self.movn_zero(END); // x1 = u64::MAX (x0 is don't-care)
        self.ret();
    }

    fn finish(mut self) -> Vec<u8> {
        for fx in &self.fixups {
            match *fx {
                Fixup::Adr { at, label } => {
                    // ADR encodes the signed byte offset to the label (±1 MiB),
                    // split into immlo[1:0] (bits 30:29) and immhi[20:2] (bits 23:5).
                    let delta = self.labels.offset_of(label) as i64 - at as i64;
                    debug_assert!((-(1 << 20)..(1 << 20)).contains(&delta), "adr out of range");
                    let immlo = (delta & 0x3) as u32;
                    let immhi = ((delta >> 2) & 0x7_FFFF) as u32;
                    let mut adr = read_u32(&self.code, at);
                    adr &= !((0x3 << 29) | (0x7_FFFF << 5));
                    adr |= (immlo << 29) | (immhi << 5);
                    write_u32(&mut self.code, at, adr);
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

#[cfg(test)]
mod tests {
    use super::super::asm::Assembler;
    use super::*;

    #[test]
    fn zero_based_byte_range_dispatch_skips_subtract() {
        let mut asm = Aarch64Asm::new();
        let target = asm.fresh_label();
        let default = asm.fresh_label();
        asm.dispatch_byte_ranges(&[(0, 10, target)], default);
        asm.bind(target);
        asm.bind(default);
        let code = asm.finish();

        assert_eq!(code.len(), 12);
    }
}
