//! Emitter tests: golden snapshots (byte-for-byte) plus execution of the same
//! snapshot files against the backtracker oracle and the TDFA interpreter
//! (`include!` of the snapshots works because `lib.rs` aliases
//! `extern crate self as regress` under test).
//!
//! To regenerate a stale snapshot:
//! ```text
//! cargo run -p regress-tool --features codegen -- '<pattern>' --emit-rust \
//!     > src/automata/tdfa/rustgen/snapshots/<name>.rs
//! ```

use super::compile_to_rust;
use crate::Flags;
use crate::__codegen::CompiledMatcher;
use crate::automata::prefilter::TdfaProgram;

/// Assert the emitter reproduces the checked-in snapshot exactly (modulo the
/// tool's trailing newline).
#[track_caller]
fn assert_golden(name: &str, pattern: &str, flags: &str, want: &str) {
    let got = compile_to_rust(pattern, Flags::from(flags)).expect("pattern should AoT-compile");
    assert_eq!(
        got.trim_end(),
        want.trim_end(),
        "snapshot `{name}` is stale; regenerate with:\n  cargo run -p regress-tool \
         --features codegen -- '{pattern}' -f '{flags}' --emit-rust > src/automata/tdfa/rustgen/snapshots/{name}.rs"
    );
}

/// Cross-check the compiled matcher against the backtracker oracle and the
/// TDFA interpreter on each input: same match ranges, same captures, in the
/// same order.
#[track_caller]
fn oracle_check(m: &CompiledMatcher, pattern: &str, flags: &str, inputs: &[&str]) {
    // `compile_to_rust` forces unicode mode; hold the oracles to the same.
    let mut flags = Flags::from(flags);
    flags.unicode = true;
    let oracle = crate::Regex::with_flags(pattern, flags).expect("oracle build");
    let mut ire =
        crate::parse::try_parse(pattern.chars().map(u32::from), flags).expect("parse");
    crate::optimizer::optimize(&mut ire);
    let program = TdfaProgram::try_from_ir(&ire).expect("program build");
    for &input in inputs {
        let got: Vec<_> = m
            .find_iter(input)
            .map(|m| (m.range.clone(), m.captures.clone()))
            .collect();
        let bt: Vec<_> = oracle
            .find_iter(input)
            .map(|m| (m.range.clone(), m.captures.clone()))
            .collect();
        assert_eq!(got, bt, "AoT vs backtracker mismatch: `{pattern}` on {input:?}");
        let tdfa: Vec<_> = crate::backends::find::<crate::automata::executors::TdfaExecutor>(
            &program, input, 0,
        )
        .map(|m| (m.range.clone(), m.captures.clone()))
        .collect();
        assert_eq!(got, tdfa, "AoT vs TDFA interpreter mismatch: `{pattern}` on {input:?}");
    }
}

// ---- prefix_word: ByteSeq prefilter + warm-start skip + peeled `\w+` ----

const PREFIX_WORD: &str = r"Sherlock\w+";

#[test]
fn golden_prefix_word() {
    assert_golden("prefix_word", PREFIX_WORD, "", include_str!("snapshots/prefix_word.rs"));
}

#[test]
fn exec_prefix_word() {
    let m = include!("snapshots/prefix_word.rs");
    oracle_check(&m, PREFIX_WORD, "",
        &[
            "",
            "Sherlock",
            "Sherlocks",
            "I saw Sherlocks and SherlockX everywhere",
            "xSherlock1 Sherlock22x",
            "Sherloc k",
            "SherlockSherlock",
            "Sherlock münchen", // non-ASCII right after the run
        ],
    );
}

// ---- literal_dollar: `$` accept via the EOI branch (no per-byte guards) ----

const LITERAL_DOLLAR: &str = "Sherlock Holmes$";

#[test]
fn golden_literal_dollar() {
    assert_golden("literal_dollar", LITERAL_DOLLAR, "",
        include_str!("snapshots/literal_dollar.rs"),
    );
}

#[test]
fn exec_literal_dollar() {
    let m = include!("snapshots/literal_dollar.rs");
    oracle_check(&m, LITERAL_DOLLAR, "",
        &[
            "",
            "Sherlock Holmes",
            "meet Sherlock Holmes",
            "Sherlock Holmes!",
            "Sherlock Holmes\n",
            "Sherlock Holmes Sherlock Holmes",
        ],
    );
}

// ---- bracket_table: dense state → `__CLASSES` table dispatch ----

const BRACKET_TABLE: &str = "T[aeiou0-9%#!=<>@]+";

#[test]
fn golden_bracket_table() {
    assert_golden("bracket_table", BRACKET_TABLE, "",
        include_str!("snapshots/bracket_table.rs"),
    );
}

#[test]
fn exec_bracket_table() {
    let m = include!("snapshots/bracket_table.rs");
    oracle_check(&m, BRACKET_TABLE, "",
        &[
            "",
            "T",
            "Ta Te T0 T%%% Tx",
            "TTTaaa",
            "aTb",
            "T@!= T#",
        ],
    );
}

// ---- digits_window: ByteBracket prefilter + LitWindow secondary filter ----

const DIGITS_WINDOW: &str = "[0-9][0-9][0-9]-[0-9][0-9][0-9][0-9]";

#[test]
fn golden_digits_window() {
    assert_golden("digits_window", DIGITS_WINDOW, "",
        include_str!("snapshots/digits_window.rs"),
    );
}

#[test]
fn exec_digits_window() {
    let m = include!("snapshots/digits_window.rs");
    oracle_check(&m, DIGITS_WINDOW, "",
        &[
            "",
            "call 555-1234 now",
            "123-4567",
            "12-3456 1234-567",
            "999-999",
            "1234-5678",
            "00-111-2222-3",
        ],
    );
}

// ---- capture tier: Prefix strategy with a group + warm-start skip ----

const CAP_PREFIX_GROUP: &str = r"Sherlock (\w+)";

#[test]
fn golden_cap_prefix_group() {
    assert_golden("cap_prefix_group", CAP_PREFIX_GROUP, "",
        include_str!("snapshots/cap_prefix_group.rs"),
    );
}

#[test]
fn exec_cap_prefix_group() {
    let m = include!("snapshots/cap_prefix_group.rs");
    oracle_check(&m, CAP_PREFIX_GROUP, "",
        &[
            "",
            "Sherlock Holmes",
            "Sherlock  Holmes",
            "meet Sherlock Holmes and Sherlock Watson",
            "Sherlock ",
        ],
    );
}

// ---- capture tier: unanchored Scan (`.*?` stamps the match start) ----

const CAP_SCAN_GROUP: &str = "([a-z]+)[0-9]";

#[test]
fn golden_cap_scan_group() {
    assert_golden("cap_scan_group", CAP_SCAN_GROUP, "",
        include_str!("snapshots/cap_scan_group.rs"),
    );
}

#[test]
fn exec_cap_scan_group() {
    let m = include!("snapshots/cap_scan_group.rs");
    oracle_check(&m, CAP_SCAN_GROUP, "",
        &[
            "",
            "abc1",
            "x abc1 de2 f",
            "ABC1",
            "abc12def3",
            "1a2b3",
            "ab", // no digit: prefix loop runs off the end
        ],
    );
}

// ---- capture tier: named group ----

const CAP_NAMED: &str = r"Dr (?<name>[A-Z]\w+)";

#[test]
fn golden_cap_named() {
    assert_golden("cap_named", CAP_NAMED, "", include_str!("snapshots/cap_named.rs"));
}

#[test]
fn exec_cap_named() {
    let m = include!("snapshots/cap_named.rs");
    oracle_check(&m, CAP_NAMED, "",
        &["", "Dr Watson", "Dr Watson and Dr Mortimer", "Dr who", "a Dr X2"],
    );
    let found = m.find("call Dr Watson now").expect("match");
    assert_eq!(found.named_group("name"), Some(8..14));
}

// ---- whole_literal: memmem only, stub verify ----

const WHOLE_LITERAL: &str = "Holmes";

#[test]
fn golden_whole_literal() {
    assert_golden("whole_literal", WHOLE_LITERAL, "", include_str!("snapshots/whole_literal.rs"));
}

#[test]
fn exec_whole_literal() {
    let m = include!("snapshots/whole_literal.rs");
    oracle_check(&m, WHOLE_LITERAL, "",
        &["", "Holmes", "Mr. Holmes and Mrs. HolmesHolmes", "holmes", "Holme"],
    );
}

// ---- casefold_lit: /i literal → CaseFoldLiteral (Teddy cross-product) ----

const CASEFOLD_LIT: &str = "Sherlock";

#[test]
fn golden_casefold_lit() {
    assert_golden("casefold_lit", CASEFOLD_LIT, "i", include_str!("snapshots/casefold_lit.rs"));
}

#[test]
fn exec_casefold_lit() {
    let m = include!("snapshots/casefold_lit.rs");
    oracle_check(&m, CASEFOLD_LIT, "i",
        &[
            "",
            "Sherlock",
            "sherlock SHERLOCK sHeRlOcK",
            "ſherlock", // the width-changing long-s fold
            "Sherloc",
        ],
    );
}

// ---- reverse_inner: interior literal, reverse-DFA walk, forward verify ----

const REVERSE_INNER: &str = r"(\w+)@(\w+)";

#[test]
fn golden_reverse_inner() {
    assert_golden("reverse_inner", REVERSE_INNER, "", include_str!("snapshots/reverse_inner.rs"));
}

#[test]
fn exec_reverse_inner() {
    let m = include!("snapshots/reverse_inner.rs");
    oracle_check(&m, REVERSE_INNER, "",
        &[
            "",
            "user@host",
            "a@b c@d",
            "@host",
            "user@",
            "mail me at first.last@example.com today",
            "a@@b",
        ],
    );
}

// ---- unsupported patterns surface as EmitError, not panics ----

#[test]
fn unsupported_patterns_error() {
    use super::EmitError;
    for pattern in [
        r"(a)\1",   // backreference (NFA rejects)
        r"(?=a)b",  // lookahead (NFA rejects)
        r"\bword",  // word boundary → per-byte guards
        r"(\w+)$",  // `$` + captures → EOI accept in the capture tier (not yet)
    ] {
        let err = compile_to_rust(pattern, Flags::default())
            .map(|_| ())
            .expect_err(pattern);
        match err {
            EmitError::Build(_) | EmitError::Unsupported(_) => {}
            other => panic!("unexpected error kind for `{pattern}`: {other:?}"),
        }
    }
}
