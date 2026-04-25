//! Execution engine bits.

use crate::api::{ExecConfig, ExecError, Match};
use crate::insn::CompiledRegex;
use crate::position::PositionType;

/// A trait for finding the next match in a regex.
/// This is broken out from Executor to avoid needing to thread lifetimes
/// around.
pub trait MatchProducer: core::fmt::Debug {
    /// The position type of our indexer.
    type Position: PositionType;

    /// \return an initial position for the given start offset.
    fn initial_position(&self, offset: usize) -> Option<Self::Position>;

    /// Attempt to match at the given location.
    /// \return either the Match and the position to start looking for the next
    /// match, or None on failure.
    fn next_match(
        &mut self,
        pos: Self::Position,
        next_start: &mut Option<Self::Position>,
    ) -> Option<Match>;

    /// Consume and return any [`ExecError`] that the last [`next_match`] call
    /// recorded (for example, a backtrack step limit set via [`ExecConfig`]
    /// was exceeded). Implementations that never produce a non-`None` error
    /// — i.e. have no bounded-execution config — can rely on the default
    /// implementation which always returns `None`.
    ///
    /// [`next_match`]: MatchProducer::next_match
    fn take_exec_error(&mut self) -> Option<ExecError> {
        None
    }
}

/// A trait for executing a regex.
pub trait Executor<'r, 't>: MatchProducer {
    /// The ASCII variant.
    type AsAscii: Executor<'r, 't>;

    /// Construct a new Executor.
    fn new(re: &'r CompiledRegex, text: &'t str) -> Self;

    /// Construct a new Executor honoring `config`. Implementations without
    /// bounded-execution support may ignore the config and delegate to
    /// [`new`](Executor::new); the default forwards accordingly so that
    /// adding config support is a non-breaking change per backend.
    fn new_with_config(re: &'r CompiledRegex, text: &'t str, _config: ExecConfig) -> Self
    where
        Self: Sized,
    {
        Self::new(re, text)
    }
}

/// A struct which enables iteration over matches.
#[derive(Debug)]
pub struct Matches<Producer: MatchProducer> {
    mp: Producer,
    position: Option<Producer::Position>,
}

impl<Producer: MatchProducer> Matches<Producer> {
    pub fn new(mp: Producer, start: usize) -> Self {
        let position = mp.initial_position(start);
        Matches { mp, position }
    }
}

impl<Producer: MatchProducer> Iterator for Matches<Producer> {
    type Item = Match;
    fn next(&mut self) -> Option<Self::Item> {
        let pos = self.position?;
        self.mp.next_match(pos, &mut self.position)
    }
}

/// A sibling of [`Matches`] that yields `Result<Match, ExecError>`, allowing
/// the caller to observe errors (such as [`ExecError::StepLimitExceeded`])
/// from bounded-execution backends. On the first error the iterator yields
/// exactly one `Err(...)` and then fuses — subsequent `.next()` calls return
/// `None`.
#[derive(Debug)]
pub struct RichMatches<Producer: MatchProducer> {
    mp: Producer,
    position: Option<Producer::Position>,
    errored: bool,
}

impl<Producer: MatchProducer> RichMatches<Producer> {
    pub fn new(mp: Producer, start: usize) -> Self {
        let position = mp.initial_position(start);
        RichMatches {
            mp,
            position,
            errored: false,
        }
    }
}

impl<Producer: MatchProducer> Iterator for RichMatches<Producer> {
    type Item = Result<Match, ExecError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.errored {
            return None;
        }
        let pos = self.position?;
        match self.mp.next_match(pos, &mut self.position) {
            Some(m) => Some(Ok(m)),
            None => match self.mp.take_exec_error() {
                Some(err) => {
                    self.errored = true;
                    self.position = None;
                    Some(Err(err))
                }
                None => None,
            },
        }
    }
}
