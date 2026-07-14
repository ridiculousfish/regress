//! End-to-end oracle tests: every `regex!` expansion is cross-checked against
//! `regress::Regex` (the classical backtracker — the engine's correctness
//! oracle) over a set of inputs, comparing full `find_iter` results (ranges +
//! captures, in order).
//!
//! The corpus grows with the AoT compiler's tiers; currently it covers the
//! capture-free tier (Prefix-strategy patterns).

use regress_macro::regex;

/// The Sherlock benchmark corpus — real prose for the prefilter paths.
const SHERLOCK: &str = include_str!("../../examples/data/sherlock.txt");

/// Expand `regex!` for the pattern, build the backtracker oracle for the same
/// pattern, and require identical `find_iter` output on every input.
/// `compile_to_rust` forces unicode mode, so the oracle gets `u` too.
macro_rules! check {
    ($pattern:literal, $flags:literal, [$($input:expr),* $(,)?]) => {{
        let m = regex!($pattern, $flags);
        let mut flags = regress::Flags::from($flags);
        flags.unicode = true;
        let oracle = regress::Regex::with_flags($pattern, flags).expect("oracle build");
        for input in [$($input),*] {
            let got: Vec<_> = m
                .find_iter(input)
                .map(|m| (m.range.clone(), m.captures.clone()))
                .collect();
            let want: Vec<_> = oracle
                .find_iter(input)
                .map(|m| (m.range.clone(), m.captures.clone()))
                .collect();
            assert_eq!(
                got, want,
                "AoT vs backtracker mismatch: pattern {:?} flags {:?} input {:?}",
                $pattern, $flags, input
            );
        }
    }};
}

#[test]
fn prefix_literal_tail() {
    check!(
        r"Sherlock\w+",
        "",
        ["", "Sherlock", "Sherlocks", "xSherlock1 Sherlock22x", SHERLOCK]
    );
    check!(
        r"Holmes [A-Z][a-z]+",
        "",
        ["Holmes Street", "Mr. Holmes said", "Holmes s", SHERLOCK]
    );
}

#[test]
fn end_anchor() {
    check!(
        "Sherlock Holmes$",
        "",
        [
            "",
            "Sherlock Holmes",
            "meet Sherlock Holmes",
            "Sherlock Holmes!",
            "Sherlock Holmes\n",
            SHERLOCK
        ]
    );
}

#[test]
fn byteset_and_bracket_prefilters() {
    check!(
        "T[aeiou0-9%#!=<>@]+",
        "",
        ["", "T", "Ta Te T0 T%%% Tx", "TTTaaa", SHERLOCK]
    );
    check!(
        "[0-9][0-9][0-9]-[0-9][0-9][0-9][0-9]",
        "",
        ["call 555-1234 now", "123-4567", "12-3456 1234-567", "1234-5678"]
    );
}

#[test]
fn alternation_shared_prefix() {
    check!(
        "Watson(?: and| or)? Holmes",
        "",
        ["Watson Holmes", "Watson and Holmes", "Watson or Holmes", SHERLOCK]
    );
}

#[test]
fn find_from_offsets() {
    let m = regex!(r"Sherlock\w+");
    let text = "Sherlocks and Sherlocked";
    let all: Vec<_> = m.find_iter(text).map(|m| m.range.clone()).collect();
    assert_eq!(all, vec![0..9, 14..24]);
    let from_one: Vec<_> = m.find_from(text, 1).map(|m| m.range.clone()).collect();
    assert_eq!(from_one, vec![14..24]);
    let past_end: Vec<_> = m.find_from(text, text.len() + 1).map(|m| m.range.clone()).collect();
    assert!(past_end.is_empty());
}

#[test]
fn capture_groups() {
    // Prefix strategy with groups (anchored capture tier + warm skip).
    check!(
        r"Sherlock (\w+)",
        "",
        ["Sherlock Holmes", "meet Sherlock Holmes and Sherlock Watson", "Sherlock ", SHERLOCK]
    );
    // Unanchored Scan: the `.*?` prefix stamps the match start.
    check!(
        "([a-z]+)[0-9]",
        "",
        ["", "abc1", "x abc1 de2 f", "abc12def3", "1a2b3", SHERLOCK]
    );
    // Multiple groups.
    check!("(a+)(b+)c", "", ["aabbc", "xaabbcx abc", "ab", "aabb"]);
    // Optional group: unmatched → None.
    check!(
        "Sherlock(ed)?",
        "",
        ["Sherlocked", "Sherlock", "Sherlock and Sherlocked", SHERLOCK]
    );
    // Fallback accepts: the accepting state can read on and re-accept later.
    check!("X(ab)*", "", ["X", "Xab", "Xababab", "Xaba", "aXabX"]);
}

#[test]
fn literal_strategies() {
    // WholeLiteral: memmem span is the match.
    check!("Holmes", "", ["", "Holmes", "Mr. HolmesHolmes", "holmes", SHERLOCK]);
    // MultiLiteral: Teddy over a literal alternation.
    check!(
        "Sherlock|Holmes|Watson",
        "",
        ["", "Watson and Holmes", "WatsonHolmesSherlock", SHERLOCK]
    );
}

#[test]
fn casefold_and_altprefix() {
    // CaseFoldLiteral: /i literal, incl. the width-changing ſ fold.
    check!("Sherlock", "i", ["", "sherlock SHERLOCK", "ſherlock", SHERLOCK]);
    // AltPrefix: branches with literal prefixes, automaton verifies tails.
    check!(
        "Sher[a-z]+|Hol[a-z]+",
        "",
        ["", "Sherlock Holmes", "Sheridan Holyoke", "Sher Hol", SHERLOCK]
    );
}

#[test]
fn reverse_inner() {
    check!(
        r"(\w+)@(\w+)",
        "",
        ["", "user@host", "a@b c@d", "@h", "u@", "first.last@example.com", SHERLOCK]
    );
    check!(r"\w+\s+Holmes", "", ["Sherlock Holmes", "x  Holmes y Holmes", SHERLOCK]);
}

#[test]
fn named_groups() {
    let m = regex!(r"Dr (?<name>[A-Z]\w+)");
    let found = m.find("call Dr Watson now").expect("match");
    assert_eq!(found.named_group("name"), Some(8..14));
    assert_eq!(found.group(1), Some(8..14));
    assert_eq!(found.named_group("nope"), None);
}

#[test]
fn zero_width_matches() {
    // Nullable pattern: empty matches everywhere, iterator must advance one
    // codepoint (including across multi-byte chars).
    check!("([a-z]*)", "", ["", "ab", "a1b", "é", "aéb", "日本語abc"]);
}

/// Fuzz-lite: fixed patterns (AoT compilation is at build time) × generated
/// inputs from a small alphabet chosen to exercise the automata's edges.
/// Deterministic xorshift so failures reproduce.
#[test]
fn generated_inputs_vs_oracle() {
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut rng = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    const ALPHABET: &[u8] = b"abcXS01 herlok\xc3\xa9"; // incl. a multi-byte char's bytes
    let mut inputs: Vec<String> = Vec::new();
    for len in [0usize, 1, 2, 3, 7, 20, 64] {
        for _ in 0..8 {
            let bytes: Vec<u8> = (0..len).map(|_| ALPHABET[(rng() % ALPHABET.len() as u64) as usize]).collect();
            inputs.push(String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    let inputs: Vec<&str> = inputs.iter().map(String::as_str).collect();
    macro_rules! fuzz {
        ($pattern:literal) => {{
            let m = regex!($pattern);
            let mut flags = regress::Flags::default();
            flags.unicode = true;
            let oracle = regress::Regex::with_flags($pattern, flags).expect("oracle build");
            for input in &inputs {
                let got: Vec<_> = m
                    .find_iter(input)
                    .map(|m| (m.range.clone(), m.captures.clone()))
                    .collect();
                let want: Vec<_> = oracle
                    .find_iter(input)
                    .map(|m| (m.range.clone(), m.captures.clone()))
                    .collect();
                assert_eq!(got, want, "pattern {:?} input {:?}", $pattern, input);
            }
        }};
    }
    fuzz!(r"Sherlock\w+");
    fuzz!(r"Sherlock (\w+)");
    fuzz!("([a-z]+)[0-9]");
    fuzz!("(a+)(b+)c");
    fuzz!("X(ab)*");
    fuzz!("([a-z]*)");
    fuzz!("S[a-z]*k");
    fuzz!("a(bc|b)c");
    fuzz!("X((a)(b))?Y");
    fuzz!("X(?:ab|a)(c?)");
}

#[test]
fn usable_in_a_static() {
    static RE: regress::__codegen::CompiledMatcher = regex!(r"Sherlock\w+");
    assert!(RE.find("Sherlocks").is_some());
    assert!(RE.find("Watson").is_none());
}

#[test]
fn default_flags_argument() {
    // One-argument form and trailing comma both parse.
    let a = regex!("Sherlock Holmes$");
    let b = regex!("Sherlock Holmes$",);
    assert_eq!(
        a.find("Sherlock Holmes").map(|m| m.range),
        b.find("Sherlock Holmes").map(|m| m.range)
    );
}
