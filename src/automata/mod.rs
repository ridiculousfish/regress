//! Conversion of IR to finite automata.

pub mod dfa;
pub mod dfa_backend;
pub mod nfa;
pub mod nfa_backend;
mod nfa_optimize;
pub mod tdfa;
pub mod tdfa_backend;
mod utf8;
pub mod util;

#[cfg(test)]
mod tests;
