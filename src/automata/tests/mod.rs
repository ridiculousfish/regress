//! Automata tests

// The `casefold` and `reverse` suites assert that a pattern selects a
// byte-literal prefilter strategy (`CaseFoldLiteral` / `ReverseInner`). Those
// strategies key off `ByteSequence` IR nodes, which the optimizer only produces
// outside `utf16` mode ("UTF-16 should never match against bytes" —
// `optimizer::form_literal_bytes`). Under `utf16` the literal stays a `Char`
// run, the strategy falls back to a plain scan, and the assertions can't hold —
// so these suites are UTF-8-only.
#[cfg(not(feature = "utf16"))]
mod casefold;
mod dfa;
mod empty_loop;
mod nfa_backend;
#[cfg(not(feature = "utf16"))]
mod reverse;
mod tdfa;
mod word_boundary;
