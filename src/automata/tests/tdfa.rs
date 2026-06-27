//! TDFA construction and execution tests (Milestone 2).

use crate::Flags;
use crate::automata::nfa::Nfa;
use crate::automata::nfa_backend::NfaMatch;
use crate::automata::nfa_backend::execute as execute_nfa_inner;
use crate::automata::tdfa::{MarkValue, Tdfa};
use crate::automata::tdfa_backend::execute as execute_tdfa_inner;

fn execute_nfa(nfa: &Nfa, input: &[u8]) -> Option<NfaMatch> {
    execute_nfa_inner(nfa, input, 0)
}

fn execute_tdfa(tdfa: &Tdfa, input: &[u8]) -> Option<NfaMatch> {
    execute_tdfa_inner(tdfa, input, 0)
}

fn parse_ir(pattern: &str) -> crate::ir::Regex {
    let flags = Flags {
        unicode: true,
        ..Default::default()
    };
    let mut re =
        crate::backends::try_parse(pattern.chars().map(u32::from), flags).expect("parse failed");
    crate::optimizer::optimize(&mut re);
    re
}

fn make_tdfa(pattern: &str) -> Tdfa {
    let re = parse_ir(pattern);
    let nfa = Nfa::try_from(&re).expect("nfa build failed");
    Tdfa::try_from(&nfa).expect("tdfa build failed")
}

/// Build the unanchored TDFA (implicit lazy `MatchAny*?` prefix) — what the
/// executors run for single-pass `find`.
fn make_tdfa_unanchored(pattern: &str) -> Tdfa {
    let re = parse_ir(pattern);
    let nfa = Nfa::try_from_unanchored(&re).expect("unanchored nfa build failed");
    Tdfa::try_from(&nfa).expect("tdfa build failed")
}

fn end(tdfa: &Tdfa, input: &[u8]) -> Option<usize> {
    execute_tdfa(tdfa, input).map(|m| m.range.end)
}

#[test]
fn greedy_consumes_all_matching_prefix() {
    // Greedy `a*` keeps the loop body alive in the closure (body comes before
    // GOAL in priority order, so truncate-at-first-GOAL retains it).
    let greedy = make_tdfa("a*");
    assert_eq!(end(&greedy, b""), Some(0));
    assert_eq!(end(&greedy, b"a"), Some(1));
    assert_eq!(end(&greedy, b"aaa"), Some(3));
    assert_eq!(end(&greedy, b"b"), Some(0));
    assert_eq!(end(&greedy, b"aab"), Some(2));
}

#[test]
fn lazy_stops_at_empty_match() {
    // Lazy `a*?` puts the loop-exit-to-GOAL path before the loop body. After
    // truncate-at-first-GOAL, the body is dropped. The TDFA accepts the empty
    // prefix and dead-states on the first byte — the correct leftmost-lazy
    // anchored match at position 0.
    let lazy = make_tdfa("a*?");
    assert_eq!(end(&lazy, b""), Some(0));
    assert_eq!(end(&lazy, b"a"), Some(0));
    assert_eq!(end(&lazy, b"aaa"), Some(0));
}

#[test]
fn anchored_basic() {
    let t = make_tdfa("abc");
    assert_eq!(end(&t, b"abc"), Some(3));
    assert_eq!(end(&t, b"abcd"), Some(3));
    assert_eq!(end(&t, b"ab"), None);
    assert_eq!(end(&t, b"xyz"), None);
}

#[test]
fn loop_matches() {
    let t = make_tdfa("ab*c");
    assert_eq!(end(&t, b"ac"), Some(2));
    assert_eq!(end(&t, b"abc"), Some(3));
    assert_eq!(end(&t, b"abbbc"), Some(5));
    assert_eq!(end(&t, b"a"), None);
    assert_eq!(end(&t, b"abd"), None);
}

#[test]
fn char_class() {
    let t = make_tdfa("[a-z]+");
    assert_eq!(end(&t, b"hello"), Some(5));
    assert_eq!(end(&t, b"Hi"), None);
    assert_eq!(end(&t, b"abc123"), Some(3));
}

#[test]
fn alternation_loop() {
    let t = make_tdfa("(ab|cd)+");
    assert_eq!(end(&t, b"ab"), Some(2));
    assert_eq!(end(&t, b"abcd"), Some(4));
    assert_eq!(end(&t, b"abcdab"), Some(6));
    assert_eq!(end(&t, b"abx"), Some(2));
    assert_eq!(end(&t, b"x"), None);
}

#[test]
fn leftmost_first_alternation() {
    // ECMAScript leftmost-first: `a|ab` picks `a` even though `ab` is longer.
    let t = make_tdfa("a|ab");
    assert_eq!(end(&t, b"ab"), Some(1));
    // Reversed order: `ab|a` picks `ab` when available, falls back to `a`.
    let t2 = make_tdfa("ab|a");
    assert_eq!(end(&t2, b"ab"), Some(2));
    assert_eq!(end(&t2, b"ac"), Some(1));
}

#[test]
fn dead_state_is_state_zero() {
    let t = make_tdfa("a");
    assert_eq!(crate::automata::tdfa::TDFA_DEAD_STATE, 0);
    assert!(!t.accepting()[0]);
}

#[test]
fn greedy_loop_correct() {
    // `a*` on "aaab" — the accept state has non-'a' transitions to dead, so
    // the scan path finds length 3.
    let t = make_tdfa("a*");
    assert_eq!(end(&t, b"aaab"), Some(3));
    assert!(t.start(0) != crate::automata::tdfa::TDFA_DEAD_STATE);
}

#[test]
fn lazy_start_is_accepting() {
    // `a*?` start accepts but transitions to dead on 'a'.
    let t = make_tdfa("a*?");
    assert!(t.start(0) != crate::automata::tdfa::TDFA_DEAD_STATE);
    assert!(t.accepting()[t.start(0) as usize]);
    assert_eq!(end(&t, b"aaa"), Some(0));
}

// ===== M3: cross-validation against NFA backend =====

fn cross_check(pattern: &str, input: &[u8]) {
    let re = parse_ir(pattern);
    let nfa = Nfa::try_from(&re).expect("nfa build");
    let tdfa = Tdfa::try_from(&nfa).expect("tdfa build");
    let nfa_result = execute_nfa(&nfa, input);
    let tdfa_result = execute_tdfa(&tdfa, input);
    assert_eq!(
        nfa_result,
        tdfa_result,
        "mismatch on pattern {:?} input {:?}",
        pattern,
        std::str::from_utf8(input).unwrap_or("<binary>")
    );
}

#[test]
fn xv_no_captures() {
    cross_check("abc", b"abc");
    cross_check("abc", b"abcd");
    cross_check("abc", b"ab");
    cross_check("a*", b"");
    cross_check("a*", b"aaa");
    cross_check("a*", b"aaab");
    cross_check("[a-z]+", b"hello");
    cross_check("[a-z]+", b"abc123");
    cross_check("(?:ab|cd)+", b"abcdab");
    cross_check("(?:ab|cd)+", b"x");
}

#[test]
fn xv_single_capture_mandatory() {
    cross_check("(abc)", b"abc");
    cross_check("(a+)b", b"aaab");
    cross_check("a(b*)c", b"abbbc");
    cross_check("a(b*)c", b"ac");
}

#[test]
fn xv_single_capture_optional() {
    cross_check("(a)?b", b"b");
    cross_check("(a)?b", b"ab");
    cross_check("(ab)?cd", b"cd");
    cross_check("(ab)?cd", b"abcd");
}

#[test]
fn xv_alternation_with_capture() {
    cross_check("(a)|(b)", b"a");
    cross_check("(a)|(b)", b"b");
}

#[test]
fn xv_greedy_vs_lazy_capture() {
    cross_check("(a*)a", b"aaa");
    // `(a*?)a` is intentionally NOT cross-validated: the NFA backend's
    // current execute_nfa is leftmost-LONGEST (returns the latest-arriving
    // GOAL thread), so on "aaa" it produces range 0..3, capture 0..2. The
    // TDFA is correctly leftmost-first (priority-ordered, no backtrack), so
    // it produces 0..1 with empty capture. The two backends genuinely
    // disagree here; aligning them is out of scope for M3.
}

#[test]
fn xv_nested_groups() {
    cross_check("((a)(b))c", b"abc");
}

#[test]
fn xv_non_capturing_inside_capturing() {
    cross_check("(a(?:bc)+d)", b"abcd");
    cross_check("(a(?:bc)+d)", b"abcbcd");
}

#[test]
fn finals_have_one_command_per_tag_when_accepting() {
    let t = make_tdfa("(abc)");
    let num_tags = t.num_tags();
    for s in 0..t.accepting().len() {
        if t.accepting()[s] {
            assert_eq!(
                t.finals()[s].len(),
                num_tags,
                "state {s} accepting but finals length != num_tags"
            );
        }
    }
}

#[test]
fn entry_commands_write_full_match_start() {
    // The start closure writes FULL_MATCH_START via an eps op; the resulting
    // entry_commands must be non-empty.
    let t = make_tdfa("abc");
    assert!(!t.entry_commands(0).is_empty());
}

#[test]
fn optimize_preserves_matches_and_shrinks() {
    // For each capture-heavy pattern, `optimize()` must produce byte-identical
    // matches to the raw build while using strictly fewer marks.
    let cases: &[(&str, &[&str])] = &[
        ("(a*)(a{3,4})", &["aaaaa", "aaa", "aaaaaaa"]),
        ("((a)(b)){1,16}", &["abab", "ababab", "ab"]),
        ("(?:(a)|(b)|(c))*", &["abcabc", "aaa", ""]),
        ("(a*?)*", &["aaaa", "", "a"]),
        ("(?:([^,]*),?)*", &["a,bb,ccc", ",,", "x"]),
        (r"(?:(\d)(\d)(\d))*", &["123456", "12", "123"]),
    ];
    for (pattern, inputs) in cases {
        let re = parse_ir(pattern);
        let nfa = Nfa::try_from(&re).expect("nfa build failed");
        let raw = Tdfa::try_from(&nfa).expect("tdfa build failed");
        let mut opt = raw.clone();
        opt.optimize();
        assert!(
            opt.num_marks() < raw.num_marks(),
            "{pattern}: expected fewer marks, {} -> {}",
            raw.num_marks(),
            opt.num_marks(),
        );
        for input in *inputs {
            assert_eq!(
                execute_tdfa(&raw, input.as_bytes()),
                execute_tdfa(&opt, input.as_bytes()),
                "pattern {pattern:?} input {input:?}: optimization changed the match",
            );
        }
    }
}

/// Regression: an unanchored alternation's per-state private register sets used
/// to blow up ~O(n²) in the number of arms (e.g. 10 arms → ~100 marks), because
/// the register allocator was both skipped for moderate mark counts and defeated
/// by spurious interference from canonicalize's `m:=pos; x:=m` parallel shifts.
/// Both are fixed (RA runs, and its liveness no longer treats a stamped-then-
/// copied mark as live-before), so the count must stay flat — O(1), not O(n²).
#[test]
fn unanchored_alternation_marks_stay_bounded() {
    let arm = |i: u8| format!("{}{}", (b'a' + i) as char, (b'A' + i) as char);
    let small: Vec<String> = (0..2).map(arm).collect();
    let big: Vec<String> = (0..12).map(arm).collect();
    let mut t2 = make_tdfa_unanchored(&small.join("|"));
    let mut t12 = make_tdfa_unanchored(&big.join("|"));
    t2.optimize();
    t12.optimize();
    // 12 arms must not need materially more marks than 2 (flat, not quadratic).
    assert!(
        t12.num_marks() <= t2.num_marks() + 2 && t12.num_marks() < 10,
        "unanchored alternation marks grew with arms: 2-arm={}, 12-arm={}",
        t2.num_marks(),
        t12.num_marks(),
    );
}

#[test]
fn optimized_commands_fold_post_ra_stamped_temporaries() {
    let mut t = make_tdfa("(?:(a)|(b)|(c))*");
    t.optimize();

    for cmds in t.transition_commands() {
        let copy_sources: Vec<_> = cmds
            .iter()
            .filter_map(|cmd| match cmd.src {
                MarkValue::Copy(src) => Some(src),
                MarkValue::CurrentPos => None,
            })
            .collect();
        for cmd in cmds {
            let MarkValue::Copy(src) = cmd.src else {
                continue;
            };
            if copy_sources.contains(&cmd.dst) {
                continue;
            }
            assert!(
                !cmds
                    .iter()
                    .any(|stamp| stamp.dst == src && matches!(stamp.src, MarkValue::CurrentPos)),
                "optimized command list still copies from a mark stamped in the same list: {cmds:?}",
            );
        }
    }
}

// ===== Unanchored single-pass search =====

/// Leftmost match via the *old* anchored-retry semantics: try the anchored
/// automaton at each successive byte offset, first hit wins. Inputs here are
/// ASCII, so every byte offset is a codepoint boundary. This is the reference
/// the new single-pass unanchored `execute` must reproduce.
fn anchored_retry_first(anchored: &Tdfa, input: &[u8]) -> Option<NfaMatch> {
    (0..=input.len()).find_map(|off| execute_tdfa_inner(anchored, input, off))
}

#[test]
fn unanchored_matches_not_at_start() {
    let t = make_tdfa_unanchored("abc");
    assert_eq!(execute_tdfa(&t, b"xxabc").map(|m| m.range), Some(2..5));
    assert_eq!(execute_tdfa(&t, b"abc").map(|m| m.range), Some(0..3));
    assert!(execute_tdfa(&t, b"xyz").is_none());
}

#[test]
fn unanchored_leftmost_wins() {
    let t = make_tdfa_unanchored("abc");
    // Earliest start wins, even though a later occurrence also matches.
    assert_eq!(execute_tdfa(&t, b"abcZabc").map(|m| m.range), Some(0..3));
    assert_eq!(execute_tdfa(&t, b"--abc--abc").map(|m| m.range), Some(2..5));
}

#[test]
fn unanchored_captures_midstring() {
    let t = make_tdfa_unanchored("(a)(b)c");
    let m = execute_tdfa(&t, b"zzabc").expect("should match");
    assert_eq!(m.range, 2..5);
    assert_eq!(m.captures, vec![Some(2..3), Some(3..4)]);
}

#[test]
fn unanchored_caret_only_at_start() {
    // The implicit prefix must not let `^` fire after skipping bytes.
    let t = make_tdfa_unanchored("^abc");
    assert_eq!(execute_tdfa(&t, b"abc").map(|m| m.range), Some(0..3));
    assert!(execute_tdfa(&t, b"xabc").is_none());
}

#[test]
fn unanchored_dollar_takes_leftmost() {
    // Regression for the `$`-conditional leftmost bug: the match completes via
    // an anchor conditional (no byte-GOAL state), so the skip thread isn't
    // pruned — the executor must still pick the earliest start.
    let t = make_tdfa_unanchored("abc.$");
    assert_eq!(execute_tdfa(&t, b"xxabc1").map(|m| m.range), Some(2..6));
}

#[test]
fn multiple_accepts_take_highest_priority() {
    // Two alternation branches reach `$` at the same position with different
    // captures, so the state carries >1 accept. Leftmost-first must keep the
    // earlier branch (group 1), not the last-iterated (lowest-priority) one.
    // Regression for a consider_accept tie-break bug on equal (start, end).
    let t = make_tdfa("(a)$|(a)$");
    let m = execute_tdfa(&t, b"a").expect("should match");
    assert_eq!(m.range, 0..1);
    assert_eq!(m.captures[0], Some(0..1), "group 1 (first branch) must win");
    assert_eq!(m.captures[1], None, "group 2 (second branch) unmatched");
}

#[test]
fn unanchored_word_boundary() {
    let t = make_tdfa_unanchored(r"\bcat\b");
    assert_eq!(execute_tdfa(&t, b"a cat!").map(|m| m.range), Some(2..5));
    // "cat" preceded by a word char => no `\b` before it.
    assert!(execute_tdfa(&t, b"scat").is_none());
}

// ---- Start-anchored prefix-skip optimization ----
//
// A regex with a leading non-multiline `^` (not inside an alternation) can only
// match at offset 0, so `try_from_unanchored` drops the lazy `MatchAny*?` prefix
// and builds the anchored form. These tests pin both the behavior and the fact
// that the prefix is actually skipped (state-count equality with the anchored
// build), and guard the multiline carve-out.

/// Parse with the multiline flag set (otherwise like `parse_ir`).
fn parse_ir_multiline(pattern: &str) -> crate::ir::Regex {
    let flags = Flags {
        unicode: true,
        multiline: true,
        ..Default::default()
    };
    let mut re =
        crate::backends::try_parse(pattern.chars().map(u32::from), flags).expect("parse failed");
    crate::optimizer::optimize(&mut re);
    re
}

fn make_tdfa_unanchored_ml(pattern: &str) -> Tdfa {
    let re = parse_ir_multiline(pattern);
    let nfa = Nfa::try_from_unanchored(&re).expect("unanchored nfa build failed");
    Tdfa::try_from(&nfa).expect("tdfa build failed")
}

#[test]
fn start_anchored_skips_prefix() {
    // The unanchored build of a start-anchored regex is identical to the
    // anchored build (the prefix would otherwise add states).
    let anchored = make_tdfa("^abc");
    let unanchored = make_tdfa_unanchored("^abc");
    assert_eq!(
        anchored.num_states(),
        unanchored.num_states(),
        "start-anchored regex should skip the unanchored prefix"
    );
    assert_eq!(
        execute_tdfa(&unanchored, b"abcdef").map(|m| m.range),
        Some(0..3)
    );
    assert!(execute_tdfa(&unanchored, b"xabc").is_none());
}

#[test]
fn start_anchored_no_match_at_nonzero_offset() {
    // Non-multiline `^a` matches only at position 0; a search starting later
    // finds nothing.
    let t = make_tdfa_unanchored("^a");
    assert_eq!(
        execute_tdfa_inner(&t, b"aaa", 0).map(|m| m.range),
        Some(0..1)
    );
    assert!(execute_tdfa_inner(&t, b"aaa", 1).is_none());
}

#[test]
fn start_anchored_through_capture_group() {
    // `is_start_anchored` descends through a leading capture group.
    let t = make_tdfa_unanchored("^(a)(b)c");
    assert_eq!(make_tdfa("^(a)(b)c").num_states(), t.num_states());
    let m = execute_tdfa(&t, b"abc").expect("should match");
    assert_eq!(m.range, 0..3);
    assert_eq!(m.captures, vec![Some(0..1), Some(1..2)]);
    assert!(execute_tdfa(&t, b"zabc").is_none());
}

#[test]
fn multiline_caret_keeps_prefix() {
    // With multiline, `^` matches at every line start, so the prefix must be
    // retained — the build must NOT collapse to the anchored form.
    let anchored = make_tdfa("^a"); // anchored form, no prefix
    let ml = make_tdfa_unanchored_ml("^a");
    assert_ne!(
        anchored.num_states(),
        ml.num_states(),
        "multiline ^ must keep the unanchored prefix"
    );
    // Leftmost match is at the line start after `\n` (input doesn't start with 'a').
    assert_eq!(execute_tdfa(&ml, b"b\na").map(|m| m.range), Some(2..3));
}

#[test]
fn top_level_alt_not_anchored() {
    // `^a|b` is an Alt at top level: the `b` arm is unanchored, so the prefix is
    // kept and `b` matches mid-string.
    let t = make_tdfa_unanchored("^a|b");
    assert_eq!(execute_tdfa(&t, b"zzb").map(|m| m.range), Some(2..3));
    assert_eq!(execute_tdfa(&t, b"a").map(|m| m.range), Some(0..1));
}

#[test]
fn all_arms_anchored_alt_skips_prefix() {
    // `^a|^b` is correctly detected as anchored.
    let t = make_tdfa_unanchored("^a|^b");
    assert_eq!(make_tdfa("^a|^b").num_states(), t.num_states());
    assert_eq!(execute_tdfa(&t, b"b").map(|m| m.range), Some(0..1));
    assert!(execute_tdfa(&t, b"zzb").is_none());
    // This descends into capture groups as well.
    let t2 = make_tdfa_unanchored("(^a)|(^b)");
    assert_eq!(make_tdfa("(^a)|(^b)").num_states(), t2.num_states());
    assert!(execute_tdfa(&t2, b"xb").is_none());
}

#[test]
fn unanchored_matches_anchored_retry() {
    // The new single-pass unanchored `execute` (raw and optimized) must agree
    // with the old anchored-retry reference on the leftmost match, over a
    // brute-force sweep of short ASCII strings. Also pins that `optimize()`
    // stays sound on the prefixed automaton.
    let pats = [
        "abc",
        "a|ab",
        "(a)(b)*",
        "[a-z]+",
        "ab|cb|db",
        "a*",
        "a+b",
        "x(yz|yw)+",
        "(?:([^,]*),?)*",
        "abc$",
        "^ab",
        r"\bca",
    ];
    let alpha = b"abcxyz,";
    let base = alpha.len() as u64;
    for pat in pats {
        let re = parse_ir(pat);
        let anchored = Tdfa::try_from(&Nfa::try_from(&re).expect("nfa")).expect("tdfa");
        let raw = Tdfa::try_from(&Nfa::try_from_unanchored(&re).expect("nfa un")).expect("tdfa un");
        let mut opt = raw.clone();
        opt.optimize();
        for len in 0..=4u32 {
            for mut code in 0..base.pow(len) {
                let mut s = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    s.push(alpha[(code % base) as usize]);
                    code /= base;
                }
                let reference = anchored_retry_first(&anchored, &s);
                assert_eq!(
                    execute_tdfa(&raw, &s),
                    reference,
                    "raw unanchored != retry for {pat:?} on {:?}",
                    String::from_utf8_lossy(&s),
                );
                assert_eq!(
                    execute_tdfa(&opt, &s),
                    reference,
                    "opt unanchored != retry for {pat:?} on {:?}",
                    String::from_utf8_lossy(&s),
                );
            }
        }
    }
}

#[test]
fn minimize_shrinks_and_preserves_matches() {
    // Tag-free patterns with equivalent intermediate states must shrink.
    for pat in ["ab|cb|db|eb", "a[bc]d|e[bc]d"] {
        let nfa = Nfa::try_from(&parse_ir(pat)).expect("nfa");
        let raw = Tdfa::try_from(&nfa).expect("tdfa");
        let mut opt = raw.clone();
        opt.optimize();
        assert!(
            opt.num_states() < raw.num_states(),
            "{pat}: expected fewer states, {} -> {}",
            raw.num_states(),
            opt.num_states(),
        );
    }

    // Brute-force differential: the optimized automaton must agree with the raw
    // one on every short string (mix of tag-free and capture-bearing patterns).
    let pats = [
        "ab|cb|db|eb",
        "a[bc]d|e[bc]d",
        "(a)(b)*",
        "(?:([^,]*),?)*",
        "foo|bar|baz",
        "(a*?)*",
        "x(yz|yw)+",
        "(a|b)*c",
    ];
    let alpha = b"abcdefxyz,";
    let base = alpha.len() as u64;
    for pat in pats {
        let nfa = Nfa::try_from(&parse_ir(pat)).expect("nfa");
        let raw = Tdfa::try_from(&nfa).expect("tdfa");
        let mut opt = raw.clone();
        opt.optimize();
        for len in 0..=4u32 {
            for mut code in 0..base.pow(len) {
                let mut s = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    s.push(alpha[(code % base) as usize]);
                    code /= base;
                }
                assert_eq!(
                    execute_tdfa(&raw, &s),
                    execute_tdfa(&opt, &s),
                    "pattern {pat:?} input {:?}: optimization changed the match",
                    String::from_utf8_lossy(&s),
                );
            }
        }
    }
}
