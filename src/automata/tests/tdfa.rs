//! TDFA construction and execution tests (Milestone 2).

use crate::automata::nfa::Nfa;
use crate::automata::nfa_backend::execute_nfa;
use crate::automata::tdfa::Tdfa;
use crate::automata::tdfa_backend::{execute_anchored, execute_anchored_match};
use crate::Flags;

fn parse_ir(pattern: &str) -> crate::ir::Regex {
    let flags = Flags {
        unicode: true,
        ..Default::default()
    };
    let mut re = crate::backends::try_parse(pattern.chars().map(u32::from), flags)
        .expect("parse failed");
    crate::optimizer::optimize(&mut re);
    re
}

fn make_tdfa(pattern: &str) -> Tdfa {
    let re = parse_ir(pattern);
    let nfa = Nfa::try_from(&re).expect("nfa build failed");
    Tdfa::try_from(&nfa).expect("tdfa build failed")
}

fn parse_ir_nonunicode(pattern: &str) -> crate::ir::Regex {
    let flags = Flags::default();
    let mut re = crate::backends::try_parse(pattern.chars().map(u32::from), flags)
        .expect("parse failed");
    crate::optimizer::optimize(&mut re);
    re
}

fn make_tdfa_nonunicode(pattern: &str) -> Tdfa {
    let re = parse_ir_nonunicode(pattern);
    let nfa = Nfa::try_from(&re).expect("nfa build failed");
    Tdfa::try_from(&nfa).expect("tdfa build failed")
}

fn end(tdfa: &Tdfa, input: &[u8]) -> Option<usize> {
    execute_anchored(tdfa, input).map(|r| r.end)
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
fn committed_accept_sentinel_structure() {
    // The committed-accept sentinel (state 1) must always be accepting and
    // self-loop on every byte class, even if no real state qualified as
    // committed-accept — these are invariants of the sentinel slot.
    let t = make_tdfa("abc");
    assert!(t.accepting()[crate::automata::tdfa::TDFA_COMMITTED_ACCEPT_STATE as usize]);
    let row = crate::automata::tdfa::TDFA_COMMITTED_ACCEPT_STATE as usize * t.num_classes();
    for c in 0..t.num_classes() {
        assert_eq!(
            t.transitions()[row + c],
            crate::automata::tdfa::TDFA_COMMITTED_ACCEPT_STATE
        );
    }
}

#[test]
fn committed_accept_merges_any_star() {
    // `[\d\D]*` has a committed-accept tail: once we've consumed enough bytes
    // to reach a state from which every future byte stays accepting, we
    // collapse into the sentinel. (The *start* isn't committed-accept in
    // Unicode matching, since it branches to UTF-8 intermediate states — but
    // the tail does collapse.)
    let t = make_tdfa_nonunicode(r"[\d\D]*");
    let sentinel = crate::automata::tdfa::TDFA_COMMITTED_ACCEPT_STATE;
    assert!(
        t.transitions().iter().any(|&s| s == sentinel),
        "no transition targets the committed-accept sentinel"
    );
    assert_eq!(end(&t, b""), Some(0));
    assert_eq!(end(&t, b"anything goes"), Some(13));
}

#[test]
fn committed_accept_after_prefix() {
    // `abc[\d\D]*`: after matching "abc", we enter a committed-accept tail.
    let t = make_tdfa_nonunicode(r"abc[\d\D]*");
    assert_eq!(end(&t, b"abc"), Some(3));
    assert_eq!(end(&t, b"abcXYZ"), Some(6));
    assert_eq!(end(&t, b"ab"), None);
    // The committed-accept sentinel is reachable from the post-"abc" state.
    // Structural: some transition in the table must target sentinel 1.
    let target = crate::automata::tdfa::TDFA_COMMITTED_ACCEPT_STATE;
    assert!(t.transitions().iter().any(|&s| s == target));
}

#[test]
fn non_committed_greedy_loop_still_correct() {
    // `a*` on "aaab" — the accept state is NOT committed-accept (non-'a'
    // transitions go to dead), so it stays a real state and the normal scan
    // path finds length 3.
    let t = make_tdfa("a*");
    assert_eq!(end(&t, b"aaab"), Some(3));
    // Start is accepting but not committed-accept: its start id is >= 2.
    assert!(t.start() > crate::automata::tdfa::TDFA_LAST_SENTINEL);
}

#[test]
fn lazy_start_is_not_committed_accept() {
    // `a*?` start accepts but transitions to dead on 'a' → not committed.
    let t = make_tdfa("a*?");
    assert!(t.start() > crate::automata::tdfa::TDFA_LAST_SENTINEL);
    assert!(t.accepting()[t.start() as usize]);
    assert_eq!(end(&t, b"aaa"), Some(0));
}

// ===== M3: cross-validation against NFA backend =====

fn cross_check(pattern: &str, input: &[u8]) {
    let re = parse_ir(pattern);
    let nfa = Nfa::try_from(&re).expect("nfa build");
    let tdfa = Tdfa::try_from(&nfa).expect("tdfa build");
    let nfa_result = execute_nfa(&nfa, input);
    let tdfa_result = execute_anchored_match(&tdfa, input);
    assert_eq!(
        nfa_result, tdfa_result,
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
    assert!(!t.entry_commands().is_empty());
}
