//! Serialize the chosen search [`Strategy`] into a `static __PREFILTER:
//! __rt::PrefilterSpec` initializer. The runtime (`regress::__codegen`)
//! rebuilds the actual searchers from these plain bytes lazily.
//!
//! Built searchers (Teddy, `CaseFoldSearcher`) don't expose their needles, so
//! this re-runs the same IR strategy-detection helpers `try_from_ir` used —
//! same functions on the same optimized IR, so the results are identical by
//! construction.

use super::{EmitError, fmt_byte};
use crate::automata::prefilter::{
    Strategy, alternation_prefix_variants, casefold_clean_run, casefold_prefix_variants,
    literal_alternation, whole_literal,
};
use crate::insn::StartPredicate;
use crate::ir;
use core::fmt::Write;

pub(super) fn emit_prefilter_static(
    w: &mut String,
    strategy: &Strategy,
    re: &ir::Regex,
) -> Result<(), EmitError> {
    match strategy {
        Strategy::Scan { .. } => {
            let _ = writeln!(
                w,
                "    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::Scan;"
            );
        }
        Strategy::Prefix {
            prefilter,
            lit_window,
            ..
        } => {
            let pred = predicate_expr(prefilter)?;
            let _ = writeln!(
                w,
                "    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::Prefix {{"
            );
            let _ = writeln!(w, "        predicate: {pred},");
            match lit_window {
                None => {
                    let _ = writeln!(w, "        lit_window: ::core::option::Option::None,");
                }
                Some(lw) => {
                    let _ = writeln!(
                        w,
                        "        lit_window: ::core::option::Option::Some(__rt::LitWindowSpec {{ byte: {}, lo: {}, hi: {} }}),",
                        fmt_byte(lw.byte),
                        lw.lo,
                        lw.hi
                    );
                }
            }
            let _ = writeln!(w, "    }};");
        }
        Strategy::WholeLiteral { .. } => {
            let bytes = whole_literal(re).expect("strategy implies a whole literal");
            let _ = writeln!(
                w,
                "    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::WholeLiteral {{"
            );
            let _ = writeln!(w, "        bytes: {},", bytes_literal(&bytes));
            let _ = writeln!(w, "        finder: __rt::FinderCache::new(),");
            let _ = writeln!(w, "    }};");
        }
        Strategy::MultiLiteral { .. } => {
            let needles = literal_alternation(re).expect("strategy implies a literal alternation");
            let _ = writeln!(
                w,
                "    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::MultiLiteral {{"
            );
            let _ = writeln!(w, "        needles: {},", needles_literal(&needles));
            let _ = writeln!(w, "        searcher: __rt::MultiCache::new(),");
            let _ = writeln!(w, "    }};");
        }
        Strategy::AltPrefix { .. } => {
            let needles =
                alternation_prefix_variants(re).expect("strategy implies branch prefixes");
            let _ = writeln!(
                w,
                "    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::AltPrefix {{"
            );
            let _ = writeln!(w, "        needles: {},", needles_literal(&needles));
            let _ = writeln!(w, "        searcher: __rt::MultiCache::new(),");
            let _ = writeln!(w, "    }};");
        }
        Strategy::CaseFoldLiteral { teddy, .. } => {
            let run = casefold_clean_run(re).expect("strategy implies a fold-clean run");
            let sets: Vec<Vec<u8>> = run.sets.iter().map(|s| s.to_vec()).collect();
            // Serialize Teddy needles only when the host actually built a
            // Teddy (mirrors the built strategy's engine choice).
            let teddy_needles = if teddy.is_some() {
                casefold_prefix_variants(re)
            } else {
                None
            };
            let _ = writeln!(
                w,
                "    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::CaseFoldLiteral {{"
            );
            let _ = writeln!(w, "        sets: {},", needles_literal(&sets));
            let _ = writeln!(w, "        prefix_lo: {},", run.prefix_lo);
            let _ = writeln!(w, "        prefix_hi: {},", run.prefix_hi);
            match teddy_needles {
                Some(needles) => {
                    let _ = writeln!(
                        w,
                        "        teddy_needles: ::core::option::Option::Some({}),",
                        needles_literal(&needles)
                    );
                }
                None => {
                    let _ = writeln!(w, "        teddy_needles: ::core::option::Option::None,");
                }
            }
            let _ = writeln!(w, "        cache: __rt::CaseFoldCache::new(),");
            let _ = writeln!(w, "    }};");
        }
        Strategy::ReverseInner {
            reverse, literal, ..
        } => {
            let _ = writeln!(
                w,
                "    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::ReverseInner {{"
            );
            let _ = writeln!(w, "        literal: {},", bytes_literal(literal.needle()));
            let _ = writeln!(w, "        finder: __rt::FinderCache::new(),");
            emit_reverse_dfa(w, reverse);
            let _ = writeln!(w, "    }};");
        }
    }
    Ok(())
}

/// The `dfa:` field of a `ReverseInner` spec: the reverse DFA's tables.
fn emit_reverse_dfa(w: &mut String, dfa: &crate::automata::dfa::Dfa) {
    let _ = writeln!(w, "        dfa: __rt::ReverseDfaSpec {{");
    let _ = writeln!(w, "            start: {},", dfa.start());
    let _ = writeln!(w, "            num_classes: {},", dfa.num_classes());
    let _ = writeln!(w, "            byte_to_class: &[");
    for row in dfa.byte_to_class().chunks(16) {
        let _ = write!(w, "                ");
        for (i, c) in row.iter().enumerate() {
            if i > 0 {
                let _ = write!(w, " ");
            }
            let _ = write!(w, "{c},");
        }
        let _ = writeln!(w);
    }
    let _ = writeln!(w, "            ],");
    let _ = write!(w, "            transitions: &[");
    for (i, t) in dfa.transitions().iter().enumerate() {
        if i % 16 == 0 {
            let _ = write!(w, "\n                ");
        } else {
            let _ = write!(w, " ");
        }
        let _ = write!(w, "{t},");
    }
    let _ = writeln!(w, "\n            ],");
    let _ = write!(w, "            accepting: &[");
    for (i, a) in dfa.accepting().iter().enumerate() {
        if i > 0 {
            let _ = write!(w, ", ");
        }
        let _ = write!(w, "{a}");
    }
    let _ = writeln!(w, "],");
    let _ = writeln!(w, "        }},");
}

/// A `&[&[u8]]` literal of needles.
fn needles_literal(needles: &[Vec<u8>]) -> String {
    let mut s = String::from("&[");
    for (i, n) in needles.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&bytes_literal(n));
    }
    s.push(']');
    s
}

fn predicate_expr(p: &StartPredicate) -> Result<String, EmitError> {
    Ok(match p {
        StartPredicate::ByteSet1([a]) => {
            format!("__rt::PredicateSpec::ByteSet1([{}])", fmt_byte(*a))
        }
        StartPredicate::ByteSet2([a, b]) => format!(
            "__rt::PredicateSpec::ByteSet2([{}, {}])",
            fmt_byte(*a),
            fmt_byte(*b)
        ),
        StartPredicate::ByteSet3([a, b, c]) => format!(
            "__rt::PredicateSpec::ByteSet3([{}, {}, {}])",
            fmt_byte(*a),
            fmt_byte(*b),
            fmt_byte(*c)
        ),
        StartPredicate::ByteSeq(finder) => format!(
            "__rt::PredicateSpec::ByteSeq {{ bytes: {}, finder: __rt::FinderCache::new() }}",
            bytes_literal(finder.needle())
        ),
        StartPredicate::ByteBracket(bm) => {
            let bits = bm.raw();
            let words = bits
                .iter()
                .map(|w| format!("0x{w:04x}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("__rt::PredicateSpec::ByteBracket([{words}])")
        }
        // `Prefix` is only built with a searchable predicate.
        StartPredicate::Arbitrary | StartPredicate::StartAnchored => {
            return Err(EmitError::Unsupported(
                "internal: unsearchable prefix predicate in the Prefix strategy",
            ));
        }
    })
}

/// Format a byte string literal (`b"..."`) with printable ASCII kept verbatim.
fn bytes_literal(bytes: &[u8]) -> String {
    let mut s = String::from("b\"");
    for &b in bytes {
        match b {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            0x20..=0x7e => s.push(b as char),
            _ => {
                let _ = write!(s, "\\x{b:02x}");
            }
        }
    }
    s.push('"');
    s
}
