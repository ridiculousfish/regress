//! `MatchProducer`/`Executor` adapters wrapping the anchored automata
//! backends so they fit the same iteration framework as the bytecode engines.
//!
//! The wrapped backends (`nfa_backend::execute`, `tdfa_backend::execute`)
//! produce a single anchored match against a byte slice. These adapters
//! implement search semantics by trying anchored execute at each codepoint
//! boundary, mirroring the `next_match_with_prefix_search` loop in
//! `classicalbacktrack.rs`. No start-predicate prefix search yet — pure
//! byte-position advancement.
//!
//! Only `Utf8Input` is supported; the underlying backends consume raw bytes,
//! and the harness only exercises these executors with UTF-8.
//!
//! `group_names` on returned `Match`es is populated from the NFA/TDFA's own
//! group-name table (collected during construction from the IR).
//!
//! Capture groups in the returned `Match` use a `Some(0..0)` sentinel for
//! groups that didn't participate, matching the convention the bytecode
//! engines use (an unmatched group is represented as a zero-width capture at
//! the start of the input, NOT as `None`). The harness's match formatters
//! treat empty-string captures as such.

use crate::api::Match;
use crate::automata::nfa::Nfa;
use crate::automata::nfa_backend;
use crate::automata::tdfa::Tdfa;
use crate::automata::tdfa_backend;
use crate::exec::{Executor, MatchProducer};
use crate::indexing::{InputIndexer, Utf8Input};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// Common search loop: try anchored execute at `pos`; advance one codepoint
/// on miss; on hit, set `next_start` (handling zero-width) and return the
/// match offset into the original input.
#[inline]
fn next_match_anchored_loop<'t, F>(
    input: Utf8Input<'t>,
    group_names: &[Box<str>],
    mut pos: <Utf8Input<'t> as InputIndexer>::Position,
    next_start: &mut Option<<Utf8Input<'t> as InputIndexer>::Position>,
    mut exec: F,
) -> Option<Match>
where
    F: FnMut(&[u8], usize) -> Option<nfa_backend::NfaMatch>,
{
    let bytes = input.contents();
    loop {
        let offset = input.pos_to_offset(pos);
        // Pass the FULL input plus a start offset so anchor predicates
        // (`^`/`$`) can evaluate against actual byte positions in the
        // input — otherwise `^` non-multiline would falsely fire at every
        // attempted start position.
        if let Some(m) = exec(bytes, offset) {
            let start = m.range.start;
            let end = m.range.end;
            let end_pos = input.try_move_right(input.left_end(), end);
            if end == start {
                // Zero-width match: bump one codepoint to make forward progress.
                *next_start = end_pos.and_then(|p| input.next_right_pos(p));
            } else {
                *next_start = end_pos;
            }
            return Some(Match {
                range: start..end,
                captures: m.captures,
                group_names: group_names.to_vec().into_boxed_slice(),
            });
        }
        pos = input.next_right_pos(pos)?;
    }
}

/// NFA-backed executor. Source = `Nfa`.
#[derive(Debug)]
pub struct NfaExecutor<'r, 't> {
    nfa: &'r Nfa,
    input: Utf8Input<'t>,
}

impl<'r, 't> MatchProducer for NfaExecutor<'r, 't> {
    type Position = <Utf8Input<'t> as InputIndexer>::Position;

    fn initial_position(&self, offset: usize) -> Option<Self::Position> {
        self.input.try_move_right(self.input.left_end(), offset)
    }

    fn next_match(
        &mut self,
        pos: Self::Position,
        next_start: &mut Option<Self::Position>,
    ) -> Option<Match> {
        let nfa = self.nfa;
        let names = nfa.group_names();
        next_match_anchored_loop(self.input, names, pos, next_start, |full, start| {
            nfa_backend::execute(nfa, full, start)
        })
    }
}

impl<'r, 't> Executor<'r, 't> for NfaExecutor<'r, 't> {
    type Source = Nfa;
    type AsAscii = NfaExecutor<'r, 't>;

    fn new(source: &'r Nfa, text: &'t str) -> Self {
        Self {
            nfa: source,
            input: Utf8Input::new(text, /* unicode */ true),
        }
    }
}

/// TDFA-backed executor. Source = `Tdfa`.
#[derive(Debug)]
pub struct TdfaExecutor<'r, 't> {
    tdfa: &'r Tdfa,
    input: Utf8Input<'t>,
}

impl<'r, 't> MatchProducer for TdfaExecutor<'r, 't> {
    type Position = <Utf8Input<'t> as InputIndexer>::Position;

    fn initial_position(&self, offset: usize) -> Option<Self::Position> {
        self.input.try_move_right(self.input.left_end(), offset)
    }

    fn next_match(
        &mut self,
        pos: Self::Position,
        next_start: &mut Option<Self::Position>,
    ) -> Option<Match> {
        let tdfa = self.tdfa;
        let names = tdfa.group_names();
        next_match_anchored_loop(self.input, names, pos, next_start, |full, start| {
            tdfa_backend::execute(tdfa, full, start)
        })
    }
}

impl<'r, 't> Executor<'r, 't> for TdfaExecutor<'r, 't> {
    type Source = Tdfa;
    type AsAscii = TdfaExecutor<'r, 't>;

    fn new(source: &'r Tdfa, text: &'t str) -> Self {
        Self {
            tdfa: source,
            input: Utf8Input::new(text, /* unicode */ true),
        }
    }
}
