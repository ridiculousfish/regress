//! `MatchProducer`/`Executor` adapters wrapping the automata backends so they
//! fit the same iteration framework as the bytecode engines.
//!
//! The wrapped backends (`nfa_backend::execute`, `tdfa_backend::execute`) are
//! built **unanchored** (see `Nfa::try_from_unanchored`): a lazy `MatchAny*?`
//! prefix lets a single `execute(bytes, offset)` pass find the leftmost match
//! at or after `offset`. So each `next_match` is one linear scan — no
//! per-codepoint retry loop.
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
use crate::automata::prefilter::TdfaProgram;
use crate::exec::{Executor, MatchProducer};
use crate::indexing::{InputIndexer, Utf8Input};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// Single unanchored pass: run `exec(bytes, offset)` once. The automaton's
/// implicit lazy prefix already scans forward to the leftmost match at or
/// after `offset`, so there's no per-position retry. On a hit, set
/// `next_start` (handling zero-width) and return the match; on a miss there
/// are no further matches.
#[inline]
fn next_match_single_pass<'t, F>(
    input: Utf8Input<'t>,
    group_names: &[Box<str>],
    pos: <Utf8Input<'t> as InputIndexer>::Position,
    next_start: &mut Option<<Utf8Input<'t> as InputIndexer>::Position>,
    mut exec: F,
) -> Option<Match>
where
    F: FnMut(&[u8], usize) -> Option<nfa_backend::NfaMatch>,
{
    let bytes = input.contents();
    // Pass the FULL input plus a start offset so anchor predicates (`^`/`$`)
    // evaluate against actual byte positions: `^` non-multiline fires only at
    // offset 0, and matches resume correctly from a non-zero `offset` on later
    // `find_iter` calls.
    let offset = input.pos_to_offset(pos);
    let m = exec(bytes, offset)?;
    let start = m.range.start;
    let end = m.range.end;
    let end_pos = input.try_move_right(input.left_end(), end);
    if end == start {
        // Zero-width match: bump one codepoint to make forward progress.
        *next_start = end_pos.and_then(|p| input.next_right_pos(p));
    } else {
        *next_start = end_pos;
    }
    Some(Match {
        range: start..end,
        captures: m.captures,
        group_names: group_names.to_vec().into_boxed_slice(),
    })
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
        next_match_single_pass(self.input, names, pos, next_start, |full, start| {
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

/// TDFA-backed executor. Source = `TdfaProgram` (an automaton plus the strategy
/// used to drive it: plain unanchored scan, or a literal prefilter that skips
/// to candidates and verifies with an anchored automaton).
#[derive(Debug)]
pub struct TdfaExecutor<'r, 't> {
    program: &'r TdfaProgram,
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
        let program = self.program;
        let names = program.group_names();
        // `find_at` already locates the leftmost match at or after the offset
        // (via the prefilter when one is available), so this stays a single
        // logical pass — same adapter as the NFA executor.
        next_match_single_pass(self.input, names, pos, next_start, |full, start| {
            program.find_at(full, start)
        })
    }
}

impl<'r, 't> Executor<'r, 't> for TdfaExecutor<'r, 't> {
    type Source = TdfaProgram;
    type AsAscii = TdfaExecutor<'r, 't>;

    fn new(source: &'r TdfaProgram, text: &'t str) -> Self {
        Self {
            program: source,
            input: Utf8Input::new(text, /* unicode */ true),
        }
    }
}
