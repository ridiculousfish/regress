//! Execution engine bits.

use crate::api::Match;
use crate::insn::CompiledRegex;

/// A trait for finding the next match in a regex.
/// This is broken out from Executor to avoid needing to thread lifetimes
/// around.
pub trait MatchProducer: std::fmt::Debug {
    /// Attempt to match at the given location.
    /// \return either the Match and the position to start looking for the next
    /// match, or None on failure.
    fn next_match(&mut self, pos: usize, next_start: &mut Option<usize>) -> Option<Match>;
}

/// A trait for executing a regex.
pub trait Executor<'r, 't>: MatchProducer {
    /// The ASCII variant.
    type AsAscii: Executor<'r, 't>;

    /// Construct a new Executor.
    fn new(re: &'r CompiledRegex, text: &'t str) -> Self;
}

/// A struct which enables iteration over matches.
#[derive(Debug)]
pub struct Matches<Producer: MatchProducer> {
    mp: Producer,
    offset: Option<usize>,
}

impl<Producer: MatchProducer> Matches<Producer> {
    pub fn new(mp: Producer, start: usize) -> Self {
        Matches {
            mp,
            offset: Some(start),
        }
    }
}

impl<Producer: MatchProducer> Iterator for Matches<Producer> {
    type Item = Match;
    fn next(&mut self) -> Option<Self::Item> {
        let start = self.offset?;
        self.mp.next_match(start, &mut self.offset)
    }
}
