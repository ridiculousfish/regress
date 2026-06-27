# `src/automata/` — finite-automata backends (NFA / DFA / TDFA)

Orientation for working in this subtree. The code's own module doc-comments are
the source of truth; this is the map that tells you where to look.

## Status: experimental, off the default path

This subsystem is **not wired into the public match API**. `Regex::find` still
runs the bytecode engines (`backends::find(&self.cr, …)` in `src/api.rs`), i.e.
the classical backtracker / PikeVM. Nothing here changes user-visible behavior
yet.

The automata backends are reachable only through:
- `pub mod automata` (gated on the `nfa` feature) — `src/lib.rs`
- the `backends` re-exports in `src/api.rs`: `Nfa`, `Tdfa`, `TdfaProgram`,
  `NfaExecutor`, `TdfaExecutor`, `Dfa`
- `regress-tool` (`--dump-*`, `--exec tnfa,tdfa`)
- `examples/*` benchmarks and `src/automata/tests/`

The **classical backtracker is the correctness oracle** — tests and benches
cross-check automata output against it. When in doubt, it is right.

Feature gate: `nfa` (default-on in `Cargo.toml`; it is what pulls in the
`smallvec` dep). The executors only support `Utf8Input`.

The optional `tdfa-jit` feature (off by default; requires `std`, incompatible
with `prohibit-unsafe`) adds a native-code TDFA backend selected as
`TdfaJitExecutor` over a `TdfaJitProgram` — a sibling to `TdfaExecutor` that
reuses all the prefilter machinery but runs the anchored verify automaton as
JIT-compiled machine code where supported (capture-free tier), falling back to
the interpreter otherwise. Cross-check it against the backtracker oracle the
same way; `--exec tdfa-jit` in `regress-tool` labels output `tdfa-jit` vs
`tdfa-jit(interp)` so you can see which path ran.

## Pipeline at a glance

```
ir::Regex (src/ir.rs)
  │
  ├─ Nfa::try_from(re)              nfa.rs:1013   (anchored: ^ only at byte 0)
  └─ Nfa::try_from_unanchored(re)   nfa.rs:1030   (implicit lazy .*? prefix)
        │  build() → recursive descent → optimize_states (nfa_optimize.rs)
        ▼
   ┌──────────────────────────────┬───────────────────────────────────────┐
   │ Dfa::try_from(nfa)  dfa.rs:113│ Tdfa::try_from(nfa)        tdfa.rs:1081 │
   │   capture-free subset constr. │   priority-ordered subset constr.,     │
   │                               │   per-thread tag maps                  │
   │                               │   Tdfa::optimize()         tdfa.rs:1234 │
   └──────────────────────────────┴───────────────────────────────────────┘
        ▼
   TdfaProgram::try_from_ir(re)   prefilter.rs:309   (picks a search Strategy)
        ▼
   NfaExecutor / TdfaExecutor     executors.rs       (single unanchored pass)
```

## Module map

| File | Role | Key entry points |
| --- | --- | --- |
| `nfa.rs` | IR → Thompson NFA (`Nfa`). Tagged eps edges, anchor/boundary predicates. | `try_from` :1013, `try_from_unanchored` :1030, `build` :1034. `GOAL_STATE`=0 :313, `FULL_MATCH_START/END`=0/1 :38. `EpsCondition` (incl. `ProgressSince` — the ES2015 nullable-loop rule). |
| `nfa_optimize.rs` | Eps-edge dedup + no-op state collapse. Runs inside `Nfa::build`. | `optimize_states` |
| `dfa.rs` | Capture-free powerset DFA (no tags). Byte equivalence classes. | `Dfa::try_from` :113, `compute_byte_classes` |
| `tdfa.rs` | Tagged DFA: priority-ordered subset construction tracking a tag→`InputMark` map per NFA thread. Anchored + unanchored start states; one per-state `StateGuards` table (`switches` = multiline `^`/`\b`/`\B`, `accepts` = `$`) decoded from the position's `boundary_signature`; `compile_moves`→`MoveOp` fast path. | `Tdfa::try_from` :1081 (correct, **unoptimized**), `Tdfa::optimize` :1234 (opt-in). `Error::PredicatedEpsNotSupported` :40 |
| `tdfa/opt.rs` | TDFA optimization, never part of `try_from`. `compact_marks` (copy-fold, dead-mark elim, register allocation) + Moore `minimize`. | `Tdfa::optimize` body |
| `nfa_backend.rs` | Anchored NFA executor (Thompson simulation over bytes). | `execute` :193, `NfaMatch` :75 |
| `tdfa_backend.rs` | Anchored TDFA byte-loop executor. Reusable `Scratch` mark buffers; leftmost accept with Laurikari-style fallback snapshot. | `execute`, `run_anchored_dyn`, `Scratch` |
| `tdfa/jit/` | **TDFA JIT** (feature `tdfa-jit`): hand-rolled native codegen that specializes a built `Tdfa` into machine code — states→code blocks, transitions→jump tables, `pos/end/input/acc` pinned in fixed registers. Two tiers: capture-free (the `exec_transitions` conditions) and anchored captures (inlined `MoveOp` stores + `finalize`, no fallback accepts). aarch64 + x86-64 encoders behind one `Assembler` trait; RX pages via `region`. | `JittedTdfa::compile`/`run`, `emit_capture_free`/`emit_capture` (drivers), `aarch64.rs`/`x86_64.rs` |
| `prefilter.rs` | `TdfaProgram` = automaton + a search `Strategy`: `WholeLiteral` (memmem only), `CaseFoldLiteral`, `Prefix` (+optional `PrefixSkip` warm-start), `ReverseInner`, `Scan` (plain unanchored). Selectivity gate `should_prefilter`. | `try_from_ir` :309, `should_prefilter` :286, `find_at` |
| `reverse.rs` | Reverse NFA/DFA for required-suffix literals (`\w+\s+Holmes`). Walks back to the leftmost start, then forward-verifies. Bails on conditional eps edges. | `reverse_nfa`, `reverse_find_start` |
| `casefold_search.rs` | SIMD case-insensitive literal scan: packed-pair on two rare anchor bytes (NEON on aarch64, SWAR fallback). Anchors chosen by `byte_frequencies`. | `CaseFoldSearcher` |
| `byte_frequencies.rs` | Static English byte-frequency rank table; lower rank = rarer = better SIMD anchor. | `BYTE_FREQUENCIES` |
| `utf8.rs` + `trie.rs` | Codepoint-set → valid UTF-8 byte-range paths → minimized byte trie spliced into the NFA. Surrogate / overlong handling. | `utf8_paths_from_code_point_set`, `build_from_code_point_set` |
| `anchors.rs` | Runtime predicate eval: `^ $ \b \B`, line terminators (incl. U+2028/9), word chars (incl. ſ / Kelvin under unicode-icase). | `EpsCondition::holds` |
| `util.rs` | Debug formatters + `BitSet`. | `to_readable_string`, `to_dot_string`, `to_stats_string` |

## Invariants & gotchas

- **Single unanchored pass.** Executors don't loop per start position; the
  automaton's implicit lazy `.*?` prefix scans forward to the leftmost match in
  one linear pass (`executors.rs`, `next_match_single_pass`). The **full** input
  plus a start offset is passed in so `^`/`$` evaluate against real byte
  positions — don't slice the haystack instead.
- **Unmatched groups are `Some(0..0)`, not `None`.** This matches the bytecode
  engines' convention (zero-width capture at input start); the harness formatters
  rely on it. See `executors.rs` header comment.
- **Tags vs InputMarks — don't conflate.** *Tags* (`TagIdx`, `0..num_capture_tags`
  plus the `FULL_MATCH_*` sentinels) are semantic capture slots. *InputMarks* are
  ephemeral IDs minted during TDFA Phase A and later register-allocated away in
  `tdfa/opt.rs`.
- **`Tdfa::try_from` is unoptimized but correct.** Optimization (`Tdfa::optimize`)
  is a separate opt-in pass. If a bug reproduces only after `optimize`, suspect
  `tdfa/opt.rs` (minimization / register allocation), not construction.
- **`PredicatedEpsNotSupported`.** `Tdfa::try_from` rejects some patterns whose
  `^`/`$`/`\b` eps edges the TDFA path doesn't yet handle — fall back to the NFA
  backend for those.

## Build / run / inspect (verified)

```bash
# Tests (modules: nfa_backend, dfa, tdfa, casefold, reverse, empty_loop, word_boundary)
cargo test --features nfa
cargo test --features nfa automata::tests::tdfa

# TDFA JIT (aarch64 native; x86-64 via cross-compile + Rosetta on Apple Silicon)
cargo test --features tdfa-jit automata::tdfa::jit
cargo test --features tdfa-jit --target x86_64-apple-darwin automata::tdfa::jit

# Benchmarks — cross-backend MB/s (Backtrack, PikeVM, NFA, TDFA; realistic_bench
# also has a `regex`-crate reference column)
cargo run --release --example realistic_bench               # Sherlock corpus
cargo run --release --example realistic_bench -- Holmes     # filter cases by name
cargo run --release --example backend_bench
cargo run --release --example tdfa_bench
cargo run --release --example tdfa_stress --features nfa     # needs the feature
cargo run --release --example prof_selfloop -- tdfa 3000     # profiler target

# Inspect / drive an automaton (regress-tool; needs --features nfa for tnfa/tdfa)
cargo run -p regress-tool --features nfa -- 'a+b*' --dump-nfa --stats-only
cargo run -p regress-tool --features nfa -- 'a+b*' --dump-dfa            # builds the TDFA
cargo run -p regress-tool --features nfa -- 'a+b*' --dump-nfa-dot | dot -Tpng > nfa.png
cargo run -p regress-tool --features nfa -- '(a*)b' --exec bt,pikevm,tnfa,tdfa 'aaab' 'bab'
cargo run -p regress-tool -- 'a+b*' --dump-phases            # IR + bytecode, no nfa feature
```

`--exec` backend names: `bt` (backtracker), `pikevm`, `tnfa`, `tdfa`.

## Related auto-memory notes

The user's persistent memory tracks the ongoing perf work on this subsystem;
keep them in sync when state changes here:
`pikevm-slower-than-backtracker`, `fa-backends-anchored-retry`,
`tdfa-literal-prefilter`, `tdfa-accept-snapshot`.
