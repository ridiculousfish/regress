# Opt-in backtracking step limit

This document describes the `ExecConfig` / `ExecError` API added to
`regress` for bounded-execution callers (JavaScript engines, untrusted-
input processors, etc.) that need to protect against
[ReDoS](https://en.wikipedia.org/wiki/ReDoS).

## Motivation

`regress` uses classical backtracking (see
[`src/classicalbacktrack.rs`](../src/classicalbacktrack.rs)). This is
the same architecture as V8's default engine and Ruby's `Regexp`, and
it has the same failure mode: certain patterns exhibit
super-polynomial step counts on certain inputs. Classic example:

```text
pattern  (a+)+b
input    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
         (30 'a's followed by a single 'c')
```

Before this change, `Regex::find("aaaa…c")` could run for minutes
before giving up. Not a bug — classical backtracking semantics — but a
real DoS vector when `regress` sits behind untrusted input in a
JavaScript engine or log-processing pipeline.

The fix used by V8 (`--regexp-backtrack-limit`) and Ruby
(`Regexp.timeout=`) is a **per-exec step budget**: abort after N
backtracking steps and let the caller decide whether to retry, reject,
or escalate.

## API

Three additive public items, all in `regress::` (re-exported from
`api.rs`):

```rust
/// Runtime configuration for a regex execution.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExecConfig {
    /// Maximum backtracking steps per call. `None` = unbounded
    /// (legacy behavior). `Some(10_000_000)` is a reasonable default.
    pub backtrack_limit: Option<u64>,
}

/// Error returned when execution could not complete under `ExecConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecError {
    StepLimitExceeded,
}

/// `Result`-yielding counterpart of `Matches`.
pub type RichMatches<'r, 't> = exec::RichMatches<backends::DefaultExecutor<'r, 't>>;
```

New methods on `Regex`:

| Method                               | Feature    | Notes                          |
|--------------------------------------|------------|--------------------------------|
| `find_iter_with_config`              | —          | Matches `find_iter`.           |
| `find_from_with_config`              | —          | Matches `find_from`.           |
| `find_from_utf16_with_config`        | `utf16`    | Matches `find_from_utf16`.     |
| `find_from_ucs2_with_config`         | `utf16`    | Matches `find_from_ucs2`.      |

All four return a `RichMatches` iterator yielding
`Result<Match, ExecError>`. On budget exhaustion the iterator yields
exactly **one** `Err(ExecError::StepLimitExceeded)` and then ends (it
fuses — subsequent `.next()` calls return `None`).

## Backwards compatibility

- Existing APIs (`find`, `find_iter`, `find_from`, `find_from_utf16`,
  `find_from_ucs2`, `replace`, `replace_all`, `replace_with`,
  `replace_all_with`) are **bit-for-bit unchanged**. They still yield
  `Match` directly and never observe `ExecError`.
- `MatchProducer::take_exec_error` has a default impl returning
  `None`, so downstream impls of the trait do not need to change.
- `Executor::new_with_config` has a default impl delegating to `new`,
  so backends without bounded-execution support compile without edits.
- The `ExecConfig::default()` value has `backtrack_limit: None`,
  which means *no change in behavior*. Bounded execution is strictly
  opt-in.

## Semantics

- The step counter is incremented at the top of the main dispatch
  loop in `MatchAttempter::try_at_pos`, which catches:
  - every bytecode instruction dispatch, AND
  - every backtrack-stack pop (since `try_backtrack` re-enters via
    `continue 'nextinsn`).
- The counter resets at the start of each `next_match` call (one
  budget per exec, not per compiled regex). This matches V8's
  `--regexp-backtrack-limit` semantics.
- Recursive lookaround shares the same counter — a hostile nested
  `(?=(a+)+b)` cannot multiply its budget past the outer limit.
- On exhaustion the backtrack stack is trimmed to its clean-state
  sentinel so subsequent (no-op) calls do not trip an internal
  `debug_assert`.

## Choosing a budget

| Budget       | Effect                                                      |
|-------------:|-------------------------------------------------------------|
| `100`        | Trips on almost any non-trivial pattern. Diagnostic only.   |
| `100_000`    | Aggressive. Kills `(a+)+b` vs 30 `a`s in <1 ms.             |
| `10_000_000` | V8's default. Kills most ReDoS in <100 ms, passes realistic patterns. |
| `100_000_000`| Ruby-ish. Passes almost all realistic patterns, still bounds runaway. |
| `None`       | Legacy unbounded. DO NOT use with untrusted inputs.         |

Pick based on your P99 wall-clock budget: the backtracker does
roughly 10 M steps per CPU-second in release mode on modern x86_64,
so 10 M steps ≈ 100 ms.

## Example

```rust
use regress::{ExecConfig, ExecError, Regex};

let re = Regex::new(r"(a+)+b")?;
let cfg = ExecConfig { backtrack_limit: Some(10_000_000) };

let s = "a".repeat(30) + "c";
let first = re.find_from_with_config(&s, 0, cfg).next();
match first {
    Some(Ok(m))  => { /* matched */ }
    Some(Err(ExecError::StepLimitExceeded)) => {
        // Reject the input, raise the limit, or escalate.
    }
    None => { /* no match, budget not exceeded */ }
}
# Ok::<(), regress::Error>(())
```

## Test coverage

See [`tests/step_limit.rs`](../tests/step_limit.rs) for:

- ReDoS pattern exceeds limit → `Err(StepLimitExceeded)`.
- Normal pattern under a limit → `Ok(match)`.
- Default config matches legacy behavior byte-for-byte.
- Tiny budget (1) trips immediately.
- UTF-16 entry point honors the same budget.
- `RichMatches` fuses after the first `Err`.
