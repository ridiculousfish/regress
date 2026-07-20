//! Table-tier emitter: the automaton as `static` data + the shared
//! interpreter loop, instead of states unrolled into code.
//!
//! Emitting data sidesteps the unrolled tier's compile-time wall, but *how*
//! the data is spelled matters just as much: every token of an array literal
//! crosses the proc-macro bridge (~90 s/MB measured), while a byte-string
//! literal is a single token. So the large numeric tables are encoded as
//! little-endian byte blobs (`AlignedBytes` + const reinterpret in the
//! runtime) and only the small structured tables (move/final arenas,
//! accelerator records — all interned/deduped) are literal arrays. The
//! emitted `__verify` drives `__codegen::table_verify`, which runs the exact
//! executor loop the interpreter uses (`TdfaTables` keeps them one
//! implementation), so match behavior is the interpreter's by construction.
//!
//! Tier restrictions (checked by the caller): compiled moves present, no
//! per-byte guards, no EOI (`$`) accepts.

use crate::automata::tdfa::{
    FinalCommand, MarkValue, MoveOp, PosStampLoopFlat, ScanFast, ScanSkipFlat, Tdfa,
};
use crate::automata::tdfa_backend::PrefixSkip;
use core::fmt::Write;

fn write_scan_fast(w: &mut String, f: &ScanFast) {
    match f {
        ScanFast::Bitmap => {
            let _ = write!(w, "__rt::ScanFast::Bitmap");
        }
        ScanFast::Memchr { count, bytes } => {
            let _ = write!(
                w,
                "__rt::ScanFast::Memchr{{count:{count},bytes:[{},{},{}]}}",
                bytes[0], bytes[1], bytes[2]
            );
        }
        ScanFast::AsciiBarrier { count, bytes } => {
            let _ = write!(
                w,
                "__rt::ScanFast::AsciiBarrier{{count:{count},bytes:[{},{},{}]}}",
                bytes[0], bytes[1], bytes[2]
            );
        }
        ScanFast::AsciiRanges { count, pairs, bm0, bm1 } => {
            let _ = write!(w, "__rt::ScanFast::AsciiRanges{{count:{count},pairs:[");
            for (i, p) in pairs.iter().enumerate() {
                let _ = write!(w, "{}{p}", if i > 0 { "," } else { "" });
            }
            let _ = write!(w, "],bm0:{bm0},bm1:{bm1}}}");
        }
        ScanFast::AsciiRangesStop { count, pairs, bm0, bm1 } => {
            let _ = write!(w, "__rt::ScanFast::AsciiRangesStop{{count:{count},pairs:[");
            for (i, p) in pairs.iter().enumerate() {
                let _ = write!(w, "{}{p}", if i > 0 { "," } else { "" });
            }
            let _ = write!(w, "],bm0:{bm0},bm1:{bm1}}}");
        }
        ScanFast::BitmapAscii { bm0, bm1 } => {
            let _ = write!(w, "__rt::ScanFast::BitmapAscii{{bm0:{bm0},bm1:{bm1}}}");
        }
    }
}

/// `static NAME: [TY; n] = [..];` — for the *small* structured tables only
/// (each element is tokens through the proc-macro bridge).
fn emit_array<T: core::fmt::Display>(
    w: &mut String,
    name: &str,
    ty: &str,
    items: impl ExactSizeIterator<Item = T>,
) {
    let _ = write!(w, "    static {name}: [{ty}; {}] = [", items.len());
    for (i, item) in items.enumerate() {
        if i % 16 == 0 {
            let _ = write!(w, "\n        ");
        }
        let _ = write!(w, "{item},");
    }
    let _ = writeln!(w, "\n    ];");
}

/// Escaped byte-string body, wrapped with string-continuation escapes
/// (backslash-newline skips the following indentation) so lines stay sane.
fn write_escaped(w: &mut String, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        if i % 64 == 0 {
            let _ = write!(w, "\\\n        ");
        }
        let _ = write!(w, "\\x{b:02x}");
    }
}

/// `static NAME: __rt::AlignedBytes<N> = __rt::AlignedBytes(*b"...");` — the
/// whole table as one byte-string token.
fn emit_blob(w: &mut String, name: &str, bytes: &[u8]) {
    let _ = write!(
        w,
        "    static {name}: __rt::AlignedBytes<{}> = __rt::AlignedBytes(*b\"",
        bytes.len()
    );
    write_escaped(w, bytes);
    let _ = writeln!(w, "\");");
}

fn le16(vals: impl Iterator<Item = u16>) -> Vec<u8> {
    vals.flat_map(u16::to_le_bytes).collect()
}
fn le32(vals: impl Iterator<Item = u32>) -> Vec<u8> {
    vals.flat_map(u32::to_le_bytes).collect()
}
fn le64(vals: impl Iterator<Item = u64>) -> Vec<u8> {
    vals.flat_map(u64::to_le_bytes).collect()
}
/// `MoveOp{dst,src}` (repr(C), two u16s) as its raw LE byte layout — the
/// exact bytes `__rt::le_moveops` reinterprets back.
fn le_moveops(vals: impl Iterator<Item = MoveOp>) -> Vec<u8> {
    vals.flat_map(|m| [m.dst.to_le_bytes(), m.src.to_le_bytes()].concat()).collect()
}
/// Flat `(tag, mark)` u32 pairs — see `StaticTdfa::finals_raw`. Panics if a
/// final ever uses `CurrentPos` (the interpreter's own invariant; violating
/// it would mean the emitter and interpreter have drifted).
fn le_finals<'a>(vals: impl Iterator<Item = &'a FinalCommand>) -> Vec<u8> {
    vals.flat_map(|fc| {
        let MarkValue::Copy(mark) = fc.src else {
            unreachable!("finals never use CurrentPos (see tdfa_backend::finalize)")
        };
        [fc.tag.to_le_bytes(), mark.0.to_le_bytes()].concat()
    })
    .collect()
}

pub(super) fn emit_table(w: &mut String, tdfa: &Tdfa, skip: Option<PrefixSkip>) {
    let n = tdfa.num_states();
    let (mv_cells, mv_arena) = tdfa.transition_moves().as_raw();
    let (fin_cells, fin_arena) = tdfa.finals_raw();
    let (psl_index, psl_table) = tdfa.psl_tables();
    let (ss_index, ss_table) = tdfa.scan_skip_tables();

    // u8 tables directly as byte strings (`&[u8; 256]` / `&[u8]` coerce).
    let _ = write!(w, "    static __T_B2C: &[u8; 256] = b\"");
    write_escaped(w, tdfa.byte_to_class());
    let _ = writeln!(w, "\";");
    let _ = write!(w, "    static __T_FLAGS: &[u8] = b\"");
    write_escaped(w, tdfa.trans_flags());
    let _ = writeln!(w, "\";");

    // Wide numeric tables as LE blobs, reinterpreted const-ly at the use site.
    // Covers everything that scales with automaton size *and* is plain data
    // (no enum payload needing type-safe reconstruction) — including MoveOp
    // (repr(C), two u16s, no invalid bit patterns) via `le_moveops`. The
    // move arena in particular must stay on this path: it's the one table
    // read every byte with moves, and it's also the one most likely to be
    // large (mark-heavy captures), so it's exactly where literal-array
    // token cost would reintroduce the unrolled tier's compile-time wall.
    emit_blob(w, "__T_TRANS_B", &le32(tdfa.transitions().iter().copied()));
    emit_blob(w, "__T_EXEC_B", &le32(tdfa.exec_transitions().iter().copied()));
    emit_blob(w, "__T_MV_CELLS_B", &le32(mv_cells.iter().copied()));
    emit_blob(w, "__T_MV_ARENA_B", &le_moveops(mv_arena.iter().copied()));
    emit_blob(w, "__T_FIN_CELLS_B", &le32(fin_cells.iter().copied()));
    emit_blob(w, "__T_FIN_RAW_B", &le_finals(fin_arena.iter()));
    emit_blob(w, "__T_PSL_IDX_B", &le32(psl_index.iter().copied()));
    emit_blob(w, "__T_SS_IDX_B", &le32(ss_index.iter().copied()));
    emit_blob(w, "__T_STAMP_B", &le16(tdfa.stamp_arena().iter().copied()));
    emit_blob(w, "__T_BMS_B", &le64(tdfa.psl_ascii_bms().iter().copied()));

    // Small structured tables as literal arrays.
    emit_array(w, "__T_ACC", "bool", tdfa.accepting().iter().copied());
    emit_array(w, "__T_FB", "bool", tdfa.accept_fallback().iter().copied());

    struct Mv(MoveOp);
    impl core::fmt::Display for Mv {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "__rt::MoveOp{{dst:{},src:{}}}", self.0.dst, self.0.src)
        }
    }
    struct Bm([u64; 4]);
    impl core::fmt::Display for Bm {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "[{},{},{},{}]", self.0[0], self.0[1], self.0[2], self.0[3])
        }
    }
    struct Ss<'a>(&'a ScanSkipFlat);
    impl core::fmt::Display for Ss<'_> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            let mut fast = String::new();
            write_scan_fast(&mut fast, &self.0.fast);
            write!(
                f,
                "__rt::ScanSkipFlat{{byte_bitmap:{},fast:{fast},stamp:({},{})}}",
                Bm(self.0.byte_bitmap),
                self.0.stamp.0,
                self.0.stamp.1
            )
        }
    }
    struct Psl<'a>(&'a PosStampLoopFlat);
    impl core::fmt::Display for Psl<'_> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            let mut fast = String::new();
            write_scan_fast(&mut fast, &self.0.fast);
            write!(
                f,
                "__rt::PosStampLoopFlat{{byte_bitmap:{},fast:{fast},stamp:({},{}),needs_snapshot:{}}}",
                Bm(self.0.byte_bitmap),
                self.0.stamp.0,
                self.0.stamp.1,
                self.0.needs_snapshot
            )
        }
    }

    emit_array(w, "__T_ENT_A", "__rt::MoveOp", tdfa.entry_moves(0).iter().copied().map(Mv));
    emit_array(w, "__T_ENT_U", "__rt::MoveOp", tdfa.entry_moves(1).iter().copied().map(Mv));
    emit_array(w, "__T_PSL", "__rt::PosStampLoopFlat", psl_table.iter().map(Psl));
    emit_array(w, "__T_SS", "__rt::ScanSkipFlat", ss_table.iter().map(Ss));

    let skip_expr = match skip {
        None => "None".to_string(),
        Some(s) => format!(
            "Some(__rt::PrefixSkip{{post_state:{},len:{}}})",
            s.post_state, s.len
        ),
    };
    let _ = writeln!(
        w,
        "    static __TDFA: __rt::StaticTdfa = __rt::StaticTdfa {{
        num_classes: {nc},
        num_marks: {nm},
        num_states: {n},
        num_capture_groups: {ng},
        has_captures: {hc},
        start_fixed: {sf},
        start_anchored: {sa},
        start_unanchored: {su},
        byte_to_class: __T_B2C,
        transitions: __rt::le_u32s(&__T_TRANS_B),
        trans_flags: __T_FLAGS,
        exec_transitions: __rt::le_u32s(&__T_EXEC_B),
        accepting: &__T_ACC,
        accept_fallback: &__T_FB,
        mv_cells: __rt::le_u32s(&__T_MV_CELLS_B),
        mv_arena: __rt::le_moveops(&__T_MV_ARENA_B),
        entry_moves_anchored: &__T_ENT_A,
        entry_moves_unanchored: &__T_ENT_U,
        finals_cells: __rt::le_u32s(&__T_FIN_CELLS_B),
        finals_raw: __rt::le_u32s(&__T_FIN_RAW_B),
        finals_cache: {{
            static __FIN_CACHE: ::std::sync::OnceLock<::std::vec::Vec<__rt::FinalCommand>> =
                ::std::sync::OnceLock::new();
            &__FIN_CACHE
        }},
        psl_index: __rt::le_u32s(&__T_PSL_IDX_B),
        psl_table: &__T_PSL,
        scan_skip_index: __rt::le_u32s(&__T_SS_IDX_B),
        scan_skip_table: &__T_SS,
        stamp_arena: __rt::le_u16s(&__T_STAMP_B),
        psl_ascii_bms: __rt::le_u64s(&__T_BMS_B),
        prefix_skip: {skip_expr},
    }};",
        nc = tdfa.num_classes(),
        nm = tdfa.num_marks(),
        ng = tdfa.num_capture_groups(),
        hc = tdfa.has_captures(),
        sf = tdfa.start_fixed(),
        sa = tdfa.start(0),
        su = tdfa.start(1),
    );
    let _ = writeln!(
        w,
        "    fn __verify(
        input: &[u8],
        start: usize,
        caps: &mut [usize],
    ) -> ::core::option::Option<(usize, usize)> {{
        __rt::table_verify(&__TDFA, input, start, caps)
    }}"
    );
}
