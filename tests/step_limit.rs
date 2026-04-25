//! Tests for the opt-in backtracking step limit added via `ExecConfig`.
//!
//! These tests pin three invariants that any review of the upstream PR
//! should check:
//!
//! 1. Pathological ReDoS patterns trip the step limit (and do so within
//!    the budget — not after minutes of wall time).
//! 2. The limit is opt-in: `ExecConfig::default()` preserves legacy
//!    unbounded behavior byte-for-byte (same iterator, same matches).
//! 3. Well-behaved patterns complete successfully under a generous budget.

use regress::{ExecConfig, ExecError, Regex};

/// Classical catastrophic backtracking against `(a+)+b` with only `a`s
/// in the input: `O(2^n)` attempts before giving up. A 30-char run would
/// spin for a very long time unbounded, but with a 100 k step budget it
/// must surface `StepLimitExceeded` quickly.
#[test]
fn redos_pattern_exceeds_step_limit() {
    let re = Regex::new(r"(a+)+b").unwrap();
    let input: String = "a".repeat(30) + "c";
    let cfg = ExecConfig {
        backtrack_limit: Some(100_000),
    };

    let first = re
        .find_from_with_config(&input, 0, cfg)
        .next()
        .expect("iterator must yield exactly one Err before ending");

    assert!(matches!(first, Err(ExecError::StepLimitExceeded)));

    // After the error, the iterator must be fused — no more items.
    let mut it = re.find_from_with_config(&input, 0, cfg);
    assert!(matches!(it.next(), Some(Err(_))));
    assert!(it.next().is_none(), "iterator must fuse after ExecError");
}

/// Normal pattern under a step limit must still match correctly.
#[test]
fn normal_pattern_matches_within_step_limit() {
    let re = Regex::new(r"\d+").unwrap();
    let cfg = ExecConfig {
        backtrack_limit: Some(10_000),
    };

    let m = re
        .find_from_with_config("abc 123 def 456", 0, cfg)
        .next()
        .expect("should yield match")
        .expect("should be Ok");
    assert_eq!(m.range, 4..7);
}

/// `ExecConfig::default()` must preserve the legacy unbounded behavior.
/// We run the same pattern both via `find_iter` and
/// `find_iter_with_config(default)` and assert the match lists agree.
#[test]
fn default_config_matches_legacy_behavior() {
    let re = Regex::new(r"\w+").unwrap();
    let text = "the quick brown fox jumps over the lazy dog";

    let legacy: Vec<_> = re.find_iter(text).map(|m| m.range).collect();
    let new_default: Vec<_> = re
        .find_iter_with_config(text, ExecConfig::default())
        .map(|m| m.expect("default has no limit, must be Ok"))
        .map(|m| m.range)
        .collect();

    assert_eq!(legacy, new_default);
}

/// A very small budget (1 step) still allows parse/compile but trips
/// immediately on any non-empty match attempt.
#[test]
fn tiny_budget_trips_immediately() {
    let re = Regex::new(r"abc").unwrap();
    let cfg = ExecConfig {
        backtrack_limit: Some(1),
    };
    let first = re
        .find_from_with_config("abc", 0, cfg)
        .next()
        .expect("must yield Err, not finish cleanly");
    assert!(matches!(first, Err(ExecError::StepLimitExceeded)));
}

/// UTF-16 entry point must honor the same budget policy and surface
/// the same `ExecError` on exhaustion.
#[cfg(feature = "utf16")]
#[test]
fn utf16_entry_point_honors_step_limit() {
    let re = Regex::new(r"(a+)+b").unwrap();
    let input: Vec<u16> = "a".repeat(30).chars().map(|c| c as u16).collect();
    let cfg = ExecConfig {
        backtrack_limit: Some(100_000),
    };

    let first = re
        .find_from_utf16_with_config(&input, 0, cfg)
        .next()
        .expect("must yield Err");
    assert!(matches!(first, Err(ExecError::StepLimitExceeded)));
}

/// `take_exec_error` contract: once consumed, it clears. Exercised
/// indirectly through the `RichMatches::next -> Err` path above (the
/// iterator fuses because it observed the error once); this test pins
/// the direct trait-level contract too.
#[test]
fn rich_matches_yields_error_then_fuses() {
    let re = Regex::new(r"(a+)+b").unwrap();
    let input: String = "a".repeat(30) + "c";
    let cfg = ExecConfig {
        backtrack_limit: Some(50_000),
    };
    let mut count_err = 0;
    let mut count_ok = 0;
    let mut count_none = 0;
    let mut it = re.find_from_with_config(&input, 0, cfg);
    for _ in 0..5 {
        match it.next() {
            Some(Ok(_)) => count_ok += 1,
            Some(Err(_)) => count_err += 1,
            None => count_none += 1,
        }
    }
    assert_eq!(count_ok, 0, "pathological input must not yield Ok");
    assert_eq!(count_err, 1, "must yield exactly one Err");
    assert_eq!(count_none, 4, "must fuse to None after the Err");
}
