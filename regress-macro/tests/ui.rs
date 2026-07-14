//! Compile-failure tests: unsupported patterns and malformed invocations must
//! produce clear, spanned diagnostics. Regenerate the .stderr goldens with
//! `TRYBUILD=overwrite cargo test -p regress-macro --test ui`.

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
