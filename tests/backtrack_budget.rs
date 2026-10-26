//! A backtracking budget bounds the work one search may do.

use regress::{BudgetedMatch, Regex};

/// The canonical exponential shape. Unbudgeted this takes minutes at n = 40;
/// with a budget it returns immediately and says so.
#[test]
fn exponential_pattern_reports_exhaustion_instead_of_running_away() {
    let re = Regex::new(r"^(a+)+$").unwrap();
    let hay = "a".repeat(40) + "!";
    assert!(matches!(
        re.find_from_budgeted(&hay, 0, 100_000),
        BudgetedMatch::BudgetExhausted
    ));
}

/// A budget generous enough for the search leaves the answer alone — both the
/// match and the no-match case, and the capture spans with them.
#[test]
fn a_sufficient_budget_does_not_change_the_answer() {
    let re = Regex::new(r"(\w+)@(\w+)\.com").unwrap();
    match re.find_from_budgeted("mail me at bob@example.com ok", 0, 1_000_000) {
        BudgetedMatch::Match(m) => {
            assert_eq!(m.range(), 11..26);
            assert_eq!(m.group(1), Some(11..14));
            assert_eq!(m.group(2), Some(15..22));
        }
        other => panic!(
            "expected a match, got {:?}",
            matches!(other, BudgetedMatch::NoMatch)
        ),
    }
    assert!(matches!(
        re.find_from_budgeted("nothing here", 0, 1_000_000),
        BudgetedMatch::NoMatch
    ));
}

/// The budget is per search, not per process: an exhausted search does not
/// poison a later one, and `find` itself stays unbudgeted.
#[test]
fn the_budget_is_per_search() {
    let re = Regex::new(r"^(a+)+$").unwrap();
    let hay = "a".repeat(40) + "!";
    assert!(matches!(
        re.find_from_budgeted(&hay, 0, 1_000),
        BudgetedMatch::BudgetExhausted
    ));
    assert!(matches!(
        re.find_from_budgeted("aaaa", 0, 1_000),
        BudgetedMatch::Match(_)
    ));
    assert!(Regex::new("ab").unwrap().find("xxabxx").is_some());
}

/// `start` behaves as it does for `find_from` — offsets are absolute and
/// lookbehind sees the text before them.
#[test]
fn budgeted_search_honours_the_start_offset() {
    let re = Regex::new(r"(?<=x)y").unwrap();
    assert!(matches!(
        re.find_from_budgeted("xyxy", 1, 1_000_000),
        BudgetedMatch::Match(_)
    ));
    match re.find_from_budgeted("xyxy", 2, 1_000_000) {
        BudgetedMatch::Match(m) => assert_eq!(m.range(), 3..4),
        _ => panic!("expected the second occurrence"),
    }
}

/// Abort the entire match if we exhaust the budget at a start position.
#[test]
fn exhaustion_does_not_skip_to_a_later_start() {
    // At 0, the middle branch explodes before `a*c` would match 0..31.
    // The `b` at 31 matches without backtracking.
    let re = Regex::new(r"b|(?:a+)+!|a*c").unwrap();
    let hay = "a".repeat(30) + "cb";
    assert!(matches!(
        re.find_from_budgeted(&hay, 0, 100_000),
        BudgetedMatch::BudgetExhausted
    ));
}

/// Abort the entire match if we exhaust the budget inside a lookaround, even a
/// negative one.
#[test]
fn exhaustion_in_a_lookaround_aborts_the_search() {
    // The lookahead body matches via `a*c`, so the correct answer is no match.
    let re = Regex::new(r"^(?!(?:a+)+!|a*c)").unwrap();
    let hay = "a".repeat(30) + "c";
    assert!(matches!(
        re.find_from_budgeted(&hay, 0, 100_000),
        BudgetedMatch::BudgetExhausted
    ));
    // Same for a negative lookbehind, which runs backwards. The body matches
    // via `ax`, so the correct answer is again no match.
    let re = Regex::new(r"x(?<!!(?:a+)+x|ax)").unwrap();
    let hay = "c".to_string() + &"a".repeat(30) + "x";
    assert!(matches!(
        re.find_from_budgeted(&hay, 0, 100_000),
        BudgetedMatch::BudgetExhausted
    ));
}

/// The UTF-16 and UCS-2 searches take the same budget and report exhaustion
/// the same way.
#[cfg(feature = "utf16")]
#[test]
fn utf16_and_ucs2_searches_report_exhaustion() {
    let re = Regex::new(r"^(a+)+$").unwrap();
    let hay: Vec<u16> = ("a".repeat(40) + "!").encode_utf16().collect();
    assert!(matches!(
        re.find_from_utf16_budgeted(&hay, 0, 100_000),
        BudgetedMatch::BudgetExhausted
    ));
    assert!(matches!(
        re.find_from_ucs2_budgeted(&hay, 0, 100_000),
        BudgetedMatch::BudgetExhausted
    ));
    let ok: Vec<u16> = "aaaa".encode_utf16().collect();
    assert!(matches!(
        re.find_from_utf16_budgeted(&ok, 0, 1_000),
        BudgetedMatch::Match(_)
    ));
}

/// The caller's budget is the one applied: a search that only matches after
/// tens of thousands of backtracks gives up under a small budget and matches
/// under a large one.
#[cfg(feature = "utf16")]
#[test]
fn utf16_and_ucs2_searches_honour_the_given_budget() {
    // The first branch explodes before the second matches the whole input.
    let re = Regex::new(r"^(?:(?:a+)+!|a*c)").unwrap();
    let hay: Vec<u16> = ("a".repeat(16) + "c").encode_utf16().collect();
    assert!(matches!(
        re.find_from_utf16_budgeted(&hay, 0, 1_000),
        BudgetedMatch::BudgetExhausted
    ));
    assert!(matches!(
        re.find_from_ucs2_budgeted(&hay, 0, 1_000),
        BudgetedMatch::BudgetExhausted
    ));
    match re.find_from_utf16_budgeted(&hay, 0, 10_000_000) {
        BudgetedMatch::Match(m) => assert_eq!(m.range(), 0..17),
        _ => panic!("expected a match with a large budget"),
    }
    match re.find_from_ucs2_budgeted(&hay, 0, 10_000_000) {
        BudgetedMatch::Match(m) => assert_eq!(m.range(), 0..17),
        _ => panic!("expected a match with a large budget"),
    }
}

/// With a sufficient budget, the UTF-16 search gives the same answer as the
/// unbudgeted iterator, in code units: an astral character counts as two, and
/// a lone surrogate is matched as one unit.
#[cfg(feature = "utf16")]
#[test]
fn utf16_budget_does_not_change_the_answer() {
    let cases: &[(&str, &str, Vec<u16>, usize)] = &[
        (
            r"(\w+)@(\w+)\.com",
            "",
            "mail me at bob@example.com ok".encode_utf16().collect(),
            0,
        ),
        (r"b.", "u", "a😀b😀c".encode_utf16().collect(), 0),
        (r"\uD800.", "", vec![0x61, 0xD800, 0x62, 0x63], 0),
        (r"(?<=x)y", "", "xyxy".encode_utf16().collect(), 2),
        (r"z", "", "xyxy".encode_utf16().collect(), 0),
    ];
    for (pattern, flags, hay, start) in cases {
        let re = Regex::with_flags(pattern, *flags).unwrap();
        let expected = re.find_from_utf16(hay, *start).next();
        match (
            re.find_from_utf16_budgeted(hay, *start, 1_000_000),
            expected,
        ) {
            (BudgetedMatch::Match(m), Some(e)) => {
                assert_eq!(m.range(), e.range(), "{pattern}");
                assert_eq!(m.captures, e.captures, "{pattern}");
            }
            (BudgetedMatch::NoMatch, None) => {}
            _ => panic!("budgeted and unbudgeted searches disagree for {pattern}"),
        }
    }
}

/// An out-of-range `start` is no match, as for the unbudgeted iterator.
#[cfg(feature = "utf16")]
#[test]
fn utf16_start_past_the_end_is_no_match() {
    let re = Regex::new("a").unwrap();
    let hay: Vec<u16> = "aaa".encode_utf16().collect();
    assert!(re.find_from_utf16(&hay, 10).next().is_none());
    assert!(matches!(
        re.find_from_utf16_budgeted(&hay, 10, 1_000),
        BudgetedMatch::NoMatch
    ));
}
