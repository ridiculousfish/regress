//! Conversion of IR to finite automata.

pub mod anchors;
pub mod dfa;
pub mod executors;
pub mod nfa;
pub mod nfa_backend;
mod nfa_optimize;
pub mod prefilter;
pub mod reverse;
pub mod tdfa;
pub mod tdfa_backend;
mod trie;
mod utf8;
pub mod util;

#[cfg(test)]
mod tests;
