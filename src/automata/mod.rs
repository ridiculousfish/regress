//! Conversion of IR to finite automata.

pub mod dfa;
pub mod nfa;
pub mod nfa_backend;
mod nfa_optimize;
mod utf8;
mod util;

#[cfg(test)]
mod tests;
