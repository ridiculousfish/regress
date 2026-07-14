//! TDFA → Rust source: the ahead-of-time sibling of the native-code JIT.
//!
//! [`compile_to_rust`] runs the full pipeline (parse → IR optimize →
//! [`TdfaProgram::try_from_ir`]) and lowers the strategy's verify automaton to
//! a block *expression* of Rust source, following the same shapes as
//! `tdfa/jit` (states → blocks, transitions → range compare-chains or
//! class-table dispatch, peeled self-loops) via the shared analysis in
//! [`plan`]. The `regress-macro` crate's `regex!` proc-macro parses the
//! returned string into its output token stream; the expression evaluates to a
//! [`CompiledMatcher`](crate::__codegen::CompiledMatcher) driven by the
//! runtime support in `regress::__codegen`.
//!
//! Output is deterministic (no hash-map iteration anywhere): the golden
//! snapshot tests compare it byte-for-byte, and the same files are `include!`d
//! and executed against the backtracker oracle.
//!
//! Patterns outside the supported tier return [`EmitError::Unsupported`]
//! rather than falling back — the macro surfaces that as a compile error.

use crate::automata::prefilter::{BuildError, Strategy, TdfaProgram};
use crate::automata::tdfa::Tdfa;
use crate::automata::tdfa_backend::PrefixSkip;
use core::fmt::Write;

/// Caps on the generated code's size. Set at the TDFA's own build budget
/// (`TDFA_STATE_BUDGET` = 4096): a budget-limit automaton emits ~1 MB of
/// source that release rustc chews through in a few seconds, so anything the
/// TDFA can build, the emitter accepts.
const CODEGEN_MAX_STATES: usize = 4096;

/// Mark-file cap: marks become local variables in the capture tier (they're
/// just SSA values to LLVM; the bound only keeps the emitted text sane).
const CODEGEN_MAX_MARKS: usize = 4096;

mod prefilter;
mod verify;

#[cfg(test)]
mod tests;

/// Why a pattern couldn't be AoT-compiled. `Display` gives the user-facing
/// diagnostic the proc-macro embeds in its `compile_error!`.
#[derive(Debug)]
pub enum EmitError {
    /// The pattern didn't parse.
    Parse(crate::Error),
    /// The NFA/TDFA build failed (unsupported construct or state budget).
    Build(BuildError),
    /// Built fine, but outside the tier the code emitter supports.
    Unsupported(&'static str),
}

impl core::fmt::Display for EmitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EmitError::Parse(e) => write!(f, "{e}"),
            EmitError::Build(BuildError::Nfa(e)) => match e {
                crate::automata::nfa::Error::UnsupportedInstruction(what) => {
                    // e.g. "Backreferences not supported by NFAs" — rephrase
                    // away the internal jargon for the macro diagnostic.
                    let what = what.trim_end_matches(" by NFAs");
                    write!(
                        f,
                        "{what} by the ahead-of-time compiler; use regress::Regex for this pattern"
                    )
                }
                crate::automata::nfa::Error::NotUTF8 => write!(f, "pattern is not valid UTF-8"),
                crate::automata::nfa::Error::BudgetExceeded => {
                    write!(f, "pattern is too large to compile ahead of time (NFA state budget)")
                }
            },
            EmitError::Build(BuildError::Tdfa(e)) => match e {
                crate::automata::tdfa::Error::BudgetExceeded => {
                    write!(f, "pattern is too large to compile ahead of time (TDFA state budget)")
                }
                crate::automata::tdfa::Error::PredicatedEpsNotSupported => {
                    write!(
                        f,
                        "pattern uses anchors/boundaries in a position the ahead-of-time \
                         compiler does not support; use regress::Regex for this pattern"
                    )
                }
            },
            EmitError::Unsupported(what) => {
                write!(f, "{what}; use regress::Regex for this pattern")
            }
        }
    }
}

/// Compile `pattern` to Rust source: a block expression evaluating to a
/// `regress::__codegen::CompiledMatcher`. `flags.unicode` is forced on — the
/// automata backends (and their oracle validation) only run in unicode mode.
pub fn compile_to_rust(pattern: &str, flags: crate::Flags) -> Result<String, EmitError> {
    let mut flags = flags;
    flags.unicode = true;
    let mut re = crate::parse::try_parse(pattern.chars().map(u32::from), flags)
        .map_err(EmitError::Parse)?;
    crate::optimizer::optimize(&mut re);
    let program = TdfaProgram::try_from_ir(&re).map_err(EmitError::Build)?;
    emit_expansion(&program, &re, pattern)
}

/// Lower a built program: pick the strategy's verify automaton and prefix skip
/// exactly as the JIT's `enable_jit` does, gate on the supported tier, and
/// assemble the expansion. `re` is the optimized IR the program was built
/// from; the prefilter serializer re-runs the strategy-detection helpers on it
/// to recover needle/set data the built searchers don't expose.
fn emit_expansion(
    program: &TdfaProgram,
    re: &crate::ir::Regex,
    pattern: &str,
) -> Result<String, EmitError> {
    // The literal-only strategies have no verify automaton: the prefilter span
    // IS the match, and the emitted verify fn is an unused stub.
    let literal_only = matches!(
        program.strategy(),
        Strategy::WholeLiteral { .. } | Strategy::MultiLiteral { .. }
    );
    if literal_only {
        let mut out = String::new();
        let w = &mut out;
        let _ = writeln!(w, "{{");
        let _ = writeln!(w, "    // regress AoT-compiled matcher for pattern {pattern:?}.");
        let _ = writeln!(w, "    use ::regress::__codegen as __rt;");
        prefilter::emit_prefilter_static(w, program.strategy(), re)?;
        emit_group_names_static(w, program.group_names());
        verify::emit_stub(w);
        let _ = writeln!(
            w,
            "    __rt::CompiledMatcher::from_parts(&__PREFILTER, __verify, 0usize, __GROUP_NAMES)"
        );
        let _ = write!(w, "}}");
        return Ok(out);
    }

    // (verify automaton, warm-start skip) per strategy — mirrors `enable_jit`.
    let (tdfa, skip): (&Tdfa, Option<PrefixSkip>) = match program.strategy() {
        Strategy::Prefix { anchored, skip, .. } => (anchored, *skip),
        Strategy::Scan { unanchored } => (unanchored, None),
        Strategy::CaseFoldLiteral { forward, .. } => (forward, None),
        Strategy::AltPrefix { forward, .. } => (forward, None),
        Strategy::ReverseInner { forward, .. } => (forward, None),
        Strategy::WholeLiteral { .. } | Strategy::MultiLiteral { .. } => unreachable!(),
    };

    // Tier selection, mirroring the JIT's `select_tier` + per-tier gates:
    // capture-free when there are no captures, a fixed start, and no per-byte
    // guards; otherwise the capture tier (which also serves the unanchored
    // `Scan` automaton — its `.*?` prefix stamps the match start).
    let capture_free =
        !tdfa.has_captures() && tdfa.start_fixed() && !tdfa.has_perbyte_guards();
    if tdfa.has_perbyte_guards() {
        return Err(EmitError::Unsupported(
            "multiline anchors and word boundaries are not yet supported by the ahead-of-time compiler",
        ));
    }
    if tdfa.num_states() > CODEGEN_MAX_STATES {
        return Err(EmitError::Unsupported(
            "pattern is too large to compile ahead of time (generated-code size cap)",
        ));
    }
    if !capture_free {
        // Capture-tier gates (JIT parity): the "read live marks at scan end"
        // scheme needs every `$` accept ruled out, and the marks lowered to
        // locals need compiled moves and a bounded mark file.
        if tdfa.has_eoi_accepts() {
            return Err(EmitError::Unsupported(
                "the `$` anchor combined with capture groups is not yet supported by the ahead-of-time compiler",
            ));
        }
        if !tdfa.has_moves() {
            return Err(EmitError::Unsupported(
                "pattern is too large to compile ahead of time (mark file exceeds compiled-move range)",
            ));
        }
        if tdfa.num_marks() > CODEGEN_MAX_MARKS {
            return Err(EmitError::Unsupported(
                "pattern is too large to compile ahead of time (too many capture positions)",
            ));
        }
    }

    let mut out = String::new();
    let w = &mut out;
    let _ = writeln!(w, "{{");
    let _ = writeln!(w, "    // regress AoT-compiled matcher for pattern {pattern:?}.");
    let _ = writeln!(w, "    use ::regress::__codegen as __rt;");
    prefilter::emit_prefilter_static(w, program.strategy(), re)?;
    emit_group_names_static(w, program.group_names());
    if capture_free {
        verify::emit_capture_free(w, tdfa, skip);
    } else {
        verify::emit_capture(w, tdfa, skip);
    }
    let _ = writeln!(
        w,
        "    __rt::CompiledMatcher::from_parts(&__PREFILTER, __verify, {}usize, __GROUP_NAMES)",
        program.num_capture_groups()
    );
    let _ = write!(w, "}}");
    Ok(out)
}

/// `static __GROUP_NAMES: &[&str] = &[...];` — the automaton's group-name
/// table verbatim (empty when there are no named groups, else one entry per
/// group with `""` for unnamed ones — same convention as `Match`).
fn emit_group_names_static(w: &mut String, names: &[Box<str>]) {
    let _ = write!(w, "    static __GROUP_NAMES: &[&str] = &[");
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            let _ = write!(w, ", ");
        }
        let _ = write!(w, "{name:?}");
    }
    let _ = writeln!(w, "];");
}

/// Format one byte as a Rust byte literal: printable ASCII as `b'x'`, the rest
/// as hex. Keeps the emitted compare-chains and peel loops readable.
fn fmt_byte(b: u8) -> String {
    match b {
        b'\'' => "b'\\''".to_string(),
        b'\\' => "b'\\\\'".to_string(),
        0x20..=0x7e => format!("b'{}'", b as char),
        _ => format!("0x{b:02x}"),
    }
}

/// Format an inclusive byte range as a match pattern (`b'a'..=b'z'` or a
/// single byte literal).
fn fmt_run(lo: u8, hi: u8) -> String {
    if lo == hi {
        fmt_byte(lo)
    } else {
        format!("{}..={}", fmt_byte(lo), fmt_byte(hi))
    }
}
