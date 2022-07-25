//! Conversion of IR to finite automata.

use crate::automata::nfa::Nfa;

#[derive(Debug)]
pub enum Error {
    BudgetExceeded,
}
#[derive(Debug)]
pub struct Dfa {}

impl Dfa {
    pub fn try_from(_nfa: &Nfa) -> Result<Self, Error> {
        Err(Error::BudgetExceeded)
    }
}
