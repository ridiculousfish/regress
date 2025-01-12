//! PikeVM regex execution engine

use crate::api::Match;
use crate::bytesearch::charset_contains;
use crate::cursor;
use crate::cursor::{Backward, Direction, Forward};
use crate::exec;
use crate::indexing::{AsciiInput, ElementType, InputIndexer, Utf8Input};
use crate::insn::{CompiledRegex, Insn, LoopFields};
use crate::matchers;
use crate::matchers::CharProperties;
use crate::position::PositionType;
use crate::scm;
use crate::scm::SingleCharMatcher;
use crate::types::{GroupData, LoopData};
use crate::util::DebugCheckIndex;
#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};
use core::ops::Range;

#[derive(Debug, Clone)]
struct State<Position: PositionType> {
    /// Position in the input string.
    pos: Position,

    /// Offset in the bytecode.
    ip: usize,

    /// Loop datas.
    loops: Vec<LoopData<Position>>,

    /// Group datas.
    groups: Vec<GroupData<Position>>,
}

enum StateMatch<Position: PositionType> {
    Fail,
    Continue,
    Split(State<Position>),
    Complete,
}

fn run_loop<Position: PositionType>(
    s: &mut State<Position>,
    lf: &LoopFields,
    is_initial_entry: bool,
) -> StateMatch<Position> {
    debug_assert!(lf.max_iters >= lf.min_iters);
    let ld = &mut s.loops[lf.loop_id as usize];
    let exit = lf.exit as usize;
    let skip_ok;
    let enter_ok;
    if is_initial_entry {
        // Entering the loop for the "first" time.
        ld.iters = 0;
        enter_ok = lf.max_iters > 0;
        skip_ok = lf.min_iters == 0;
    } else {
        // Note that iters is the number of complete iterations.
        ld.iters += 1;
        // We can enter the loop if we have iterated less than the maximum number of
        // times.
        enter_ok = ld.iters < lf.max_iters;

        // We can skip the loop if we have iterated at least the minimum number of
        // times.
        skip_ok = ld.iters >= lf.min_iters;

        // Check if this iteration was beyond the minimum number of times, and our entry
        // position is the same as last time (ES6 21.2.2.5.1 note 4).
        // If so, we matched the empty string and we stop.
        if ld.iters > lf.min_iters && ld.entry == s.pos {
            return StateMatch::Fail;
        }
    }
    // Set up our fields as if we are going to enter the loop.
    ld.entry = s.pos;
    s.ip += 1;

    if !enter_ok && !skip_ok {
        StateMatch::Fail
    } else if !enter_ok {
        s.ip = exit;
        StateMatch::Continue
    } else if !skip_ok {
        StateMatch::Continue
    } else {
        debug_assert!(enter_ok && skip_ok);
        // We need to split our state.
        let mut newstate = s.clone();
        let exit_state = if lf.greedy { s } else { &mut newstate };
        exit_state.ip = exit;
        StateMatch::Split(newstate)
    }
}

fn try_match_state<Input: InputIndexer, Dir: Direction>(
    re: &CompiledRegex,
    input: &Input,
    s: &mut State<Input::Position>,
    dir: Dir,
) -> StateMatch<Input::Position> {
    macro_rules! nextinsn_or_fail {
        ($e:expr) => {
            if $e {
                s.ip += 1;
                StateMatch::Continue
            } else {
                StateMatch::Fail
            }
        };
    }
    match &re.insns[s.ip] {
        Insn::Goal => StateMatch::Complete,
        Insn::JustFail => StateMatch::Fail,
        &Insn::Char(c) => match cursor::next(input, dir, &mut s.pos) {
            Some(c2) => nextinsn_or_fail!(c == c2.as_u32()),
            _ => StateMatch::Fail,
        },
        &Insn::CharICase(c) => match cursor::next(input, dir, &mut s.pos) {
            Some(c2) => {
                nextinsn_or_fail!(c == c2.as_u32() || input.fold(c2).as_u32() == c)
            }
            _ => StateMatch::Fail,
        },

        Insn::CharSet(v) => match cursor::next(input, dir, &mut s.pos) {
            Some(c) => nextinsn_or_fail!(charset_contains(v, c.as_u32())),
            _ => StateMatch::Fail,
        },

        Insn::ByteSeq1(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq2(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq3(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq4(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq5(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq6(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq7(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq8(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq9(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq10(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq11(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq12(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq13(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq14(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq15(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),
        Insn::ByteSeq16(v) => nextinsn_or_fail!(cursor::try_match_lit(input, dir, &mut s.pos, v)),

        Insn::StartOfLine => {
            let matches = match input.peek_left(s.pos) {
                None => true,
                Some(c) if re.flags.multiline && Input::CharProps::is_line_terminator(c) => true,
                _ => false,
            };
            nextinsn_or_fail!(matches)
        }

        Insn::EndOfLine => {
            let matches = match input.peek_right(s.pos) {
                None => true, // we're at the right of the string
                Some(c) if re.flags.multiline && Input::CharProps::is_line_terminator(c) => true,
                _ => false,
            };
            nextinsn_or_fail!(matches)
        }

        Insn::MatchAny => match cursor::next(input, dir, &mut s.pos) {
            Some(_) => nextinsn_or_fail!(true),
            _ => StateMatch::Fail,
        },

        Insn::MatchAnyExceptLineTerminator => match cursor::next(input, dir, &mut s.pos) {
            Some(c2) => nextinsn_or_fail!(!Input::CharProps::is_line_terminator(c2)),
            _ => StateMatch::Fail,
        },

        &Insn::Jump { target } => {
            s.ip = target as usize;
            StateMatch::Continue
        }

        &Insn::Alt { secondary } => {
            let mut left = s.clone();
            left.ip += 1;
            s.ip = secondary as usize;
            StateMatch::Split(left)
        }

        &Insn::BeginCaptureGroup(group_idx) => {
            let group = &mut s.groups[group_idx as usize];
            if Dir::FORWARD {
                core::debug_assert!(!group.start_matched(), "Group should not have been entered");
                group.start = Some(s.pos);
            } else {
                core::debug_assert!(!group.end_matched(), "Group should not have been entered");
                group.end = Some(s.pos);
            }
            nextinsn_or_fail!(true)
        }

        &Insn::EndCaptureGroup(group_idx) => {
            let group = &mut s.groups[group_idx as usize];
            if Dir::FORWARD {
                core::debug_assert!(group.start_matched(), "Group should have been entered");
                group.end = Some(s.pos);
            } else {
                core::debug_assert!(group.end_matched(), "Group should have been exited");
                group.start = Some(s.pos);
            }
            core::debug_assert!(
                group.end >= group.start,
                "Exit pos should be after start pos"
            );
            nextinsn_or_fail!(true)
        }

        &Insn::ResetCaptureGroup(group_idx) => {
            s.groups[group_idx as usize].reset();
            nextinsn_or_fail!(true)
        }

        &Insn::BackRef(group_idx) => {
            let matched;
            let group = &mut s.groups[group_idx as usize];
            if let Some(orig_range) = group.as_range() {
                if re.flags.icase {
                    matched = matchers::backref_icase(input, dir, orig_range, &mut s.pos);
                } else {
                    matched = matchers::backref(input, dir, orig_range, &mut s.pos)
                }
            } else {
                // This group has not been exited, and therefore the match succeeds
                // (ES6 21.2.2.9).
                matched = true;
            }
            nextinsn_or_fail!(matched)
        }

        &Insn::Lookahead {
            negate,
            start_group: _,
            end_group: _,
            continuation,
        } => {
            // Enter into the lookaround's instruction stream.
            s.ip += 1;
            let saved_pos = s.pos;
            let attempt_succeeded =
                MatchAttempter::<Input>::new(re).try_at_pos(*input, s, Forward::new());
            let matched = attempt_succeeded != negate;
            if matched {
                s.ip = continuation as usize;
                s.pos = saved_pos;
                StateMatch::Continue
            } else {
                StateMatch::Fail
            }
        }

        &Insn::Lookbehind {
            negate,
            start_group: _,
            end_group: _,
            continuation,
        } => {
            // Enter into the lookaround's instruction stream.
            s.ip += 1;
            let saved_pos = s.pos;
            let attempt_succeeded = MatchAttempter::new(re).try_at_pos(*input, s, Backward::new());
            let matched = attempt_succeeded != negate;
            if matched {
                s.ip = continuation as usize;
                s.pos = saved_pos;
                StateMatch::Continue
            } else {
                StateMatch::Fail
            }
        }
        Insn::EnterLoop(lf) => run_loop(s, lf, true),
        &Insn::LoopAgain { begin } => {
            s.ip = begin as usize;
            match re.insns.iat(s.ip) {
                Insn::EnterLoop(ref lf) => run_loop(s, lf, false),
                _ => panic!("LoopAgain does not point at EnterLoop"),
            }
        }
        Insn::Loop1CharBody { .. } => panic!("Loop1CharBody unimplemented for pikevm"),
        &Insn::Bracket(idx) => match cursor::next(input, dir, &mut s.pos) {
            Some(c) => nextinsn_or_fail!(Input::CharProps::bracket(&re.brackets[idx], c)),
            _ => StateMatch::Fail,
        },

        Insn::AsciiBracket(bytes) => {
            nextinsn_or_fail!(scm::MatchByteSet { bytes }.matches(input, dir, &mut s.pos))
        }
        &Insn::ByteSet2(bytes) => {
            nextinsn_or_fail!(scm::MatchByteArraySet { bytes }.matches(input, dir, &mut s.pos))
        }
        &Insn::ByteSet3(bytes) => {
            nextinsn_or_fail!(scm::MatchByteArraySet { bytes }.matches(input, dir, &mut s.pos))
        }
        &Insn::ByteSet4(bytes) => {
            nextinsn_or_fail!(scm::MatchByteArraySet { bytes }.matches(input, dir, &mut s.pos))
        }

        &Insn::WordBoundary { invert } => {
            let prev_wordchar = input
                .peek_left(s.pos)
                .is_some_and(Input::CharProps::is_word_char);
            let curr_wordchar = input
                .peek_right(s.pos)
                .is_some_and(Input::CharProps::is_word_char);
            let is_boundary = prev_wordchar != curr_wordchar;
            nextinsn_or_fail!(is_boundary != invert)
        }
    }
}

fn successful_match<Input: InputIndexer>(
    input: Input,
    start: Input::Position,
    state: &State<Input::Position>,
    group_names: Box<[Box<str>]>,
) -> Match {
    let group_to_offset = |mr: &GroupData<Input::Position>| -> Option<Range<usize>> {
        mr.as_range().map(|r| Range {
            start: input.pos_to_offset(r.start),
            end: input.pos_to_offset(r.end),
        })
    };
    let captures = state.groups.iter().map(group_to_offset).collect();
    Match {
        range: input.pos_to_offset(start)..input.pos_to_offset(state.pos),
        captures,
        group_names,
    }
}

#[derive(Debug)]
struct MatchAttempter<'a, Input: InputIndexer> {
    states: Vec<State<Input::Position>>,
    re: &'a CompiledRegex,
}

impl<'a, Input: InputIndexer> MatchAttempter<'a, Input> {
    fn new(re: &'a CompiledRegex) -> Self {
        Self {
            states: Vec::new(),
            re,
        }
    }

    fn try_at_pos<Dir: Direction>(
        &mut self,
        input: Input,
        init_state: &mut State<Input::Position>,
        dir: Dir,
    ) -> bool {
        debug_assert!(self.states.is_empty(), "Should be no states");
        self.states.push(init_state.clone());
        while !self.states.is_empty() {
            let s = self.states.last_mut().unwrap();
            match try_match_state(self.re, &input, s, dir) {
                StateMatch::Fail => {
                    self.states.pop();
                }
                StateMatch::Continue => {}
                StateMatch::Complete => {
                    // Give the successful state to the caller.
                    core::mem::swap(init_state, s);
                    self.states.clear();
                    return true;
                }
                StateMatch::Split(newstate) => self.states.push(newstate),
            }
        }
        false
    }
}

#[derive(Debug)]
pub struct PikeVMExecutor<'r, Input: InputIndexer> {
    input: Input,
    matcher: MatchAttempter<'r, Input>,
}

impl<'r, 't> exec::Executor<'r, 't> for PikeVMExecutor<'r, Utf8Input<'t>> {
    type AsAscii = PikeVMExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = Utf8Input::new(text, re.flags.unicode);
        Self {
            input,
            matcher: MatchAttempter::new(re),
        }
    }
}

impl<'r, 't> exec::Executor<'r, 't> for PikeVMExecutor<'r, AsciiInput<'t>> {
    type AsAscii = PikeVMExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = AsciiInput::new(text);
        Self {
            input,
            matcher: MatchAttempter::new(re),
        }
    }
}

impl<Input: InputIndexer> exec::MatchProducer for PikeVMExecutor<'_, Input> {
    type Position = Input::Position;

    fn initial_position(&self, offset: usize) -> Option<Self::Position> {
        self.input.try_move_right(self.input.left_end(), offset)
    }

    fn next_match(
        &mut self,
        pos: Self::Position,
        next_start: &mut Option<Self::Position>,
    ) -> Option<Match> {
        let re = self.matcher.re;
        // Note the "initial" loop position is ignored. Use whatever is most convenient.
        let mut state = State {
            pos,
            ip: 0,
            loops: vec![LoopData::new(pos); re.loops as usize],
            groups: vec![GroupData::new(); re.groups as usize],
        };
        loop {
            let start = state.pos;
            if self
                .matcher
                .try_at_pos(self.input, &mut state, Forward::new())
            {
                let end = state.pos;
                if end != start {
                    *next_start = Some(end)
                } else {
                    *next_start = self.input.next_right_pos(end)
                }
                return Some(successful_match(
                    self.input,
                    start,
                    &state,
                    re.group_names.clone(),
                ));
            }
            match self.input.next_right_pos(start) {
                Some(nextpos) => state.pos = nextpos,
                None => break,
            }
        }
        None
    }
}
