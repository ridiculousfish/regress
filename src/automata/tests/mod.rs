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
// The interior-literal window filter keys off a `ByteSequence` IR node for the
// required literal, which the optimizer only emits outside `utf16` mode.
#[cfg(not(feature = "utf16"))]
mod litwindow;
// Prefix-skip needs the `ByteSequence`/byte-class start predicates the optimizer
// emits only outside `utf16` mode (same as `litwindow`/`reverse`/`casefold`).
#[cfg(not(feature = "utf16"))]
mod prefix_skip;
// `MultiLiteral` / `AltPrefix` are `prefilter-teddy`-only strategies keyed off
// `ByteSequence` IR nodes (UTF-8-only, like `casefold`/`reverse`).
#[cfg(all(feature = "prefilter-teddy", not(feature = "utf16")))]
mod altprefix;
#[cfg(all(feature = "prefilter-teddy", not(feature = "utf16")))]
mod multiliteral;
mod nfa_backend;
#[cfg(not(feature = "utf16"))]
mod reverse;
mod tdfa;
mod word_boundary;
