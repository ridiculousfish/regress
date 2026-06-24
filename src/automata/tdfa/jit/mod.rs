//! TDFA JIT: a hand-rolled native code generator that specializes a built
//! [`Tdfa`](crate::automata::tdfa::Tdfa) into machine code.
//!
//! Instead of the byte-loop interpreter in
//! [`tdfa_backend`](crate::automata::tdfa_backend), which re-reads the
//! transition/class/accept tables for every input byte, the JIT bakes the
//! automaton into control flow: each state becomes a code block, transitions
//! become branches/jump-tables, accepts become inline stores, and the hot
//! values (`pos`, `end`, `input`, `last_accept`) are pinned in registers.
//!
//! Status: Phase 1 — capture-free tier only (the `exec_transitions` fast path:
//! no marks, `start_fixed`, no conditionals/anchor-alts). Unsupported automata
//! return an error so the caller falls back to the interpreter backend.

mod asm;
mod mem;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

use crate::automata::nfa_backend::NfaMatch;
use crate::automata::tdfa::{TDFA_DEAD_STATE, Tdfa};
use asm::{Assembler, Label};
use mem::ExecBuffer;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// C ABI of the generated function:
/// `(input, len, start) -> match-end offset, or usize::MAX for no match`.
type EntryFn = extern "C" fn(*const u8, usize, usize) -> usize;

/// Why a [`Tdfa`] could not be JIT-compiled. The caller falls back to the
/// interpreter backend on any of these.
#[derive(Debug)]
pub enum JitError {
    /// The automaton uses a feature this JIT tier doesn't emit yet (captures,
    /// conditionals, anchor alts, oversized mark file, …).
    Unsupported(&'static str),
    /// This target architecture has no encoder yet.
    UnsupportedArch,
    /// Allocating or protecting executable memory failed.
    Memory(region::Error),
}

impl From<region::Error> for JitError {
    fn from(e: region::Error) -> Self {
        JitError::Memory(e)
    }
}

impl core::fmt::Display for JitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            JitError::Unsupported(why) => write!(f, "TDFA JIT unsupported: {why}"),
            JitError::UnsupportedArch => write!(f, "TDFA JIT: no encoder for this architecture"),
            JitError::Memory(e) => write!(f, "TDFA JIT executable memory: {e}"),
        }
    }
}

/// A [`Tdfa`] compiled to native code. Holds the executable mapping and a
/// pointer into it; the mapping is freed on drop, so this must outlive every
/// [`run`](Self::run) call. Build with [`compile`](Self::compile).
#[derive(Debug)]
pub struct JittedTdfa {
    /// Keeps the executable mapping alive; `entry` points into it.
    _buf: ExecBuffer,
    entry: EntryFn,
}

impl JittedTdfa {
    /// Compile `tdfa` to native code for the host architecture, or return why
    /// it can't be (so the caller falls back to the interpreter). Only the
    /// capture-free tier is supported in this phase.
    pub fn compile(tdfa: &Tdfa) -> Result<Self, JitError> {
        let code = compile_capture_free(tdfa)?;
        let buf = ExecBuffer::new(&code)?;
        // SAFETY: `buf` is kept alive in the returned struct; the generated
        // code implements exactly this C ABI signature (see `asm` register map).
        let entry: EntryFn = unsafe { core::mem::transmute(buf.entry_ptr()) };
        Ok(Self { _buf: buf, entry })
    }

    /// Run the anchored automaton from byte offset `start`. Returns the
    /// (capture-free) match `start..end`, or `None`.
    pub fn run(&self, input: &[u8], start: usize) -> Option<NfaMatch> {
        debug_assert!(start <= input.len());
        let end = (self.entry)(input.as_ptr(), input.len(), start);
        if end == usize::MAX {
            None
        } else {
            Some(NfaMatch {
                range: start..end,
                captures: Vec::new(),
            })
        }
    }
}

/// Pick the host-arch encoder and lower the capture-free tier. Errors with
/// [`JitError::UnsupportedArch`] on architectures without an encoder.
fn compile_capture_free(tdfa: &Tdfa) -> Result<Vec<u8>, JitError> {
    #[cfg(target_arch = "aarch64")]
    {
        emit::<aarch64::Aarch64Asm>(tdfa)
    }
    #[cfg(target_arch = "x86_64")]
    {
        emit::<x86_64::X86_64Asm>(tdfa)
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        let _ = tdfa;
        Err(JitError::UnsupportedArch)
    }
}

/// The arch-independent codegen driver: walk `tdfa` and emit the capture-free
/// state machine through `A`. Each state becomes a code block (accept-on-entry,
/// EOI check, byte fetch, then jump-table dispatch); a shared class table and
/// per-state jump tables follow the code. Dead transitions route to `done`.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn emit<A: Assembler>(tdfa: &Tdfa) -> Result<Vec<u8>, JitError> {
    // Capture-free tier only: the conditions under which `exec_transitions`
    // exists (no marks read back per byte, fixed start, no `$`-conditionals or
    // multiline-`^` alts).
    if tdfa.has_captures() {
        return Err(JitError::Unsupported("captures"));
    }
    if !tdfa.start_fixed() {
        return Err(JitError::Unsupported("unanchored / start not fixed"));
    }
    if tdfa.has_conditionals() {
        return Err(JitError::Unsupported("$-conditionals"));
    }
    if tdfa.has_anchor_alts() {
        return Err(JitError::Unsupported("multiline-^ anchor alts"));
    }

    let nc = tdfa.num_classes();
    let num_states = tdfa.num_states();
    let transitions = tdfa.transitions();
    let accepting = tdfa.accepting();
    let byte_to_class = tdfa.byte_to_class();
    let start_anchored = tdfa.start(0) as usize;
    let start_unanchored = tdfa.start(1) as usize;

    let mut asm = A::new();
    let classtab = asm.fresh_label();
    let done = asm.fresh_label();
    let block: Vec<Label> = (0..num_states).map(|_| asm.fresh_label()).collect();
    let jt: Vec<Label> = (0..num_states).map(|_| asm.fresh_label()).collect();

    // Prologue + per-state code blocks.
    asm.prologue(classtab, block[start_anchored], block[start_unanchored]);
    for s in 0..num_states {
        asm.bind(block[s]);
        if accepting[s] {
            asm.record_accept();
        }
        asm.eoi_check(done);
        asm.fetch_and_classify();
        asm.dispatch(jt[s]);
    }
    asm.bind(done);
    asm.ret_done();

    // Data: shared class table, then one jump table per state.
    asm.class_table(classtab, byte_to_class);
    let mut entries: Vec<Label> = Vec::with_capacity(nc);
    for s in 0..num_states {
        entries.clear();
        let row = &transitions[s * nc..s * nc + nc];
        for &t in row {
            entries.push(if t == TDFA_DEAD_STATE {
                done
            } else {
                block[t as usize]
            });
        }
        asm.jump_table(jt[s], &entries);
    }

    Ok(asm.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::mem::ExecBuffer;
    use crate::automata::nfa::Nfa;
    use crate::automata::tdfa_backend;

    fn anchored_tdfa(pattern: &str) -> Tdfa {
        let flags = crate::api::Flags::default();
        let re = crate::backends::try_parse(pattern.chars().map(u32::from), flags)
            .expect("parse failed");
        let nfa = Nfa::try_from(&re).expect("nfa build failed");
        let mut tdfa = Tdfa::try_from(&nfa).expect("tdfa build failed");
        tdfa.optimize();
        tdfa
    }

    /// JIT output must match the interpreter (the oracle) for every (input,
    /// start) pair, across patterns that exercise the capture-free tier.
    fn assert_matches_interpreter(pattern: &str, inputs: &[&str]) {
        let tdfa = anchored_tdfa(pattern);
        let jit = JittedTdfa::compile(&tdfa)
            .unwrap_or_else(|e| panic!("compile {pattern:?}: {e}"));
        for input in inputs {
            let bytes = input.as_bytes();
            for start in 0..=bytes.len() {
                let want = tdfa_backend::execute(&tdfa, bytes, start);
                let got = jit.run(bytes, start);
                assert_eq!(
                    want.as_ref().map(|m| m.range.clone()),
                    got.as_ref().map(|m| m.range.clone()),
                    "pattern {pattern:?} input {input:?} start {start}",
                );
            }
        }
    }

    #[test]
    fn jit_literal() {
        assert_matches_interpreter("abc", &["abc", "xabc", "ab", "abcabc", "", "abx"]);
    }

    #[test]
    fn jit_quantifiers() {
        assert_matches_interpreter("a+", &["", "a", "aaa", "baaa", "aaab"]);
        assert_matches_interpreter("a*b", &["b", "ab", "aaab", "aaa", "xb"]);
        assert_matches_interpreter("[0-9]+", &["123", "", "12a34", "a1"]);
    }

    #[test]
    fn jit_alternation_and_groups() {
        assert_matches_interpreter("a|bc", &["a", "bc", "b", "abc", ""]);
        assert_matches_interpreter("(?:ab)+", &["ab", "abab", "aba", "a", ""]);
        assert_matches_interpreter("(?:foo|bar)+", &["foobar", "foo", "barbar", "baz"]);
    }

    /// End-to-end through the public backend: the `TdfaJitExecutor` must yield
    /// the same matches as the interpreter `TdfaExecutor` over a real
    /// `find_iter`, and actually run native code for a prefix-literal pattern.
    #[test]
    fn jit_backend_matches_interpreter() {
        use crate::automata::prefilter::{TdfaJitProgram, TdfaProgram};
        use crate::backends::{self, TdfaExecutor, TdfaJitExecutor};

        // `expect_jit`: `Some(true)` patterns must run native code (a rare
        // literal prefix + tail → anchored verify); `Some(false)` must stay
        // interpreted (a bare literal is the memmem-only whole-literal path; a
        // common-first-byte pattern like `a+b` has no selective prefilter and
        // takes the unanchored `Scan`, outside the capture-free tier). `None`
        // is selection-dependent — only correctness is checked.
        let cases: &[(&str, &str, Option<bool>)] = &[
            ("abc", "xx abc yy abcabc zz", Some(false)),
            ("foo[0-9]+", "foo12 foobar foo3 foo", Some(true)),
            ("a+b", "aaab ab cab b aaa", Some(false)),
            ("(?:cat|dog)s?", "cats dog doghouse ca dogs", None),
            ("xyz[a-z]*", "xyzabc xy xyz xyzzz", Some(true)),
        ];
        for &(pat, hay, expect_jit) in cases {
            let flags = crate::api::Flags::default();
            let mut re = backends::try_parse(pat.chars().map(u32::from), flags).expect("parse");
            // `try_from_ir` expects optimized IR (it's what lowers a literal run
            // into a `ByteSequence` the prefilter can use).
            backends::optimize(&mut re);
            let interp = TdfaProgram::try_from_ir(&re).expect("interp program");
            let jit = TdfaJitProgram::try_from_ir(&re).expect("jit program");

            let want: Vec<_> = backends::find::<TdfaExecutor>(&interp, hay, 0)
                .map(|m| (m.range.clone(), m.captures.clone()))
                .collect();
            let got: Vec<_> = backends::find::<TdfaJitExecutor>(&jit, hay, 0)
                .map(|m| (m.range.clone(), m.captures.clone()))
                .collect();
            assert_eq!(want, got, "pattern {pat:?} over {hay:?}");
            if let Some(want_jit) = expect_jit {
                assert_eq!(jit.jit_active(), want_jit, "jit activation for {pat:?}");
            }
        }
    }

    #[test]
    fn jit_byte_classes_and_negation() {
        assert_matches_interpreter("a.c", &["abc", "axc", "ac", "aXcc"]);
        assert_matches_interpreter("[^x]+", &["abc", "x", "abxc", "", "xxx"]);
        assert_matches_interpreter("[a-c]+", &["abc", "abcd", "d", "cba"]);
    }

    /// Throughput smoke check (run with `--ignored --nocapture`): compare the
    /// JIT against the interpreter fast loop on a large capture-free anchored
    /// scan. Not an assertion — prints MB/s for both so the speedup is visible.
    #[test]
    #[ignore = "performance, run manually with --ignored --nocapture"]
    fn jit_throughput_vs_interpreter() {
        use std::time::Instant;
        let tdfa = anchored_tdfa("[a-z]+");
        let jit = JittedTdfa::compile(&tdfa).expect("compile");
        let input = vec![b'a'; 4 * 1024 * 1024];
        let iters = 200;

        // Warm + time the interpreter.
        let mut sink = 0usize;
        let t = Instant::now();
        for _ in 0..iters {
            sink += tdfa_backend::execute(&tdfa, &input, 0).map_or(0, |m| m.range.end);
        }
        let interp = t.elapsed();

        let t = Instant::now();
        for _ in 0..iters {
            sink += jit.run(&input, 0).map_or(0, |m| m.range.end);
        }
        let jitted = t.elapsed();

        let mb = (input.len() * iters) as f64 / (1024.0 * 1024.0);
        eprintln!(
            "interp: {:.0} MB/s   jit: {:.0} MB/s   speedup: {:.2}x   (sink={sink})",
            mb / interp.as_secs_f64(),
            mb / jitted.as_secs_f64(),
            interp.as_secs_f64() / jitted.as_secs_f64(),
        );
    }

    /// Randomized differential test: for a spread of patterns, compare the JIT
    /// against the interpreter on many random inputs at every start offset.
    #[test]
    fn jit_fuzz_vs_interpreter() {
        use rand::rngs::SmallRng;
        use rand::{Rng, SeedableRng};

        let patterns = [
            "a+b+", "(?:ab|cd)+", "[0-9]+", "[a-c]*z", "x.y", "[^ab]+",
            "foo", "a?b?c?", "(?:a|b|c)+d",
        ];
        let alphabet = b"abcdz019x";
        let mut rng = SmallRng::seed_from_u64(0xC0DA_F00D);

        for pat in patterns {
            let tdfa = anchored_tdfa(pat);
            let Ok(jit) = JittedTdfa::compile(&tdfa) else {
                continue; // legitimately unsupported tier; backend falls back
            };
            for _ in 0..200 {
                let len = rng.gen_range(0..12);
                let bytes: Vec<u8> =
                    (0..len).map(|_| alphabet[rng.gen_range(0..alphabet.len())]).collect();
                for start in 0..=bytes.len() {
                    let want = tdfa_backend::execute(&tdfa, &bytes, start);
                    let got = jit.run(&bytes, start);
                    assert_eq!(
                        want.as_ref().map(|m| m.range.clone()),
                        got.as_ref().map(|m| m.range.clone()),
                        "pattern {pat:?} input {bytes:?} start {start}",
                    );
                }
            }
        }
    }

    /// Validate the executable-memory path end to end on this host: hand-encode
    /// a tiny function and call it. This is the platform tracer bullet — if RX
    /// allocation / protection / icache flush / calling convention work here,
    /// the codegen can build on top.
    #[test]
    fn exec_buffer_calls_handwritten_fn() {
        // A function `(usize, usize, usize) -> usize` returning its 3rd arg.
        #[cfg(target_arch = "aarch64")]
        let code: &[u8] = &[
            0xE0, 0x03, 0x02, 0xAA, // mov x0, x2
            0xC0, 0x03, 0x5F, 0xD6, // ret
        ];
        #[cfg(target_arch = "x86_64")]
        let code: &[u8] = &[
            0x48, 0x89, 0xD0, // mov rax, rdx
            0xC3, // ret
        ];
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        let code: &[u8] = &[];
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            return; // no encoder/ABI knowledge for this arch
        }

        #[allow(unreachable_code)]
        {
            let buf = ExecBuffer::new(code).expect("alloc RX");
            // SAFETY: `buf` outlives the call; the bytes implement this exact
            // C ABI signature on the target arch.
            let f: extern "C" fn(usize, usize, usize) -> usize =
                unsafe { core::mem::transmute(buf.entry_ptr()) };
            assert_eq!(f(10, 20, 30), 30);
            assert_eq!(f(1, 2, 3), 3);
        }
    }
}
