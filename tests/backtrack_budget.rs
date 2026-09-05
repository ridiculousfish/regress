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
