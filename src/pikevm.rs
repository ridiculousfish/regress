//! PikeVM regex execution engine

use crate::api::Match;
use crate::bytesearch::{charset_contains, ByteSet};
use crate::cursor;
use crate::cursor::{Cursor, Cursorable, Forward, Position};
use crate::exec;
use crate::indexing::{AsciiInput, ElementType, InputIndexer, Utf8Input};
use crate::insn::{CompiledRegex, Insn, LoopFields};
use crate::matchers;
use crate::matchers::CharProperties;
use crate::types::{GroupData, LoopData};
use crate::util::DebugCheckIndex;

#[derive(Debug, Clone)]
struct State {
    /// Position in the input string.
    pos: Position,

    /// Offset in the bytecode.
    ip: usize,

    /// Loop datas.
    loops: Vec<LoopData>,

    /// Group datas.
    groups: Vec<GroupData>,
}

enum StateMatch {
    Fail,
    Continue,
    Split(State),
    Complete,
}

fn run_loop(s: &mut State, lf: &LoopFields, is_initial_entry: bool) -> StateMatch {
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

fn try_match_state<Cursor: Cursorable>(
    re: &CompiledRegex,
    cursor: Cursor,
    s: &mut State,
) -> StateMatch {
    macro_rules! nextinsn_or_fail {
        ($e:expr) => {
            if $e {
                s.ip += 1;
                StateMatch::Continue
            } else {
                StateMatch::Fail
            };
        };
    };
    match &re.insns[s.ip] {
        Insn::Goal => StateMatch::Complete,
        Insn::JustFail => StateMatch::Fail,
        &Insn::Char(c) => match cursor.next(&mut s.pos) {
            Some(c2) => nextinsn_or_fail!(c == c2.as_char()),
            _ => StateMatch::Fail,
        },
        &Insn::CharICase(c) => {
            let c = match Cursor::Element::try_from(c) {
                Some(c) => c,
                None => return StateMatch::Fail,
            };
            match cursor.next(&mut s.pos) {
                Some(c2) => nextinsn_or_fail!(c == c2 || Cursor::CharProps::fold(c2) == c),
                _ => StateMatch::Fail,
            }
        }

        Insn::CharSet(v) => match cursor.next(&mut s.pos) {
            Some(c) => nextinsn_or_fail!(charset_contains(v, c.as_char())),
            _ => StateMatch::Fail,
        },

        Insn::ByteSeq1(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq2(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq3(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq4(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq5(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq6(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq7(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq8(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq9(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq10(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq11(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq12(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq13(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq14(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq15(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),
        Insn::ByteSeq16(v) => nextinsn_or_fail!(cursor.try_match_lit(&mut s.pos, v)),

        Insn::StartOfLine => {
            let matches = match cursor.peek_left(s.pos) {
                None => true,
                Some(c) if re.flags.multiline && Cursor::CharProps::is_line_terminator(c) => true,
                _ => false,
            };
            nextinsn_or_fail!(matches)
        }

        Insn::EndOfLine => {
            let matches = match cursor.peek_right(s.pos) {
                None => true, // we're at the right of the string
                Some(c) if re.flags.multiline && Cursor::CharProps::is_line_terminator(c) => true,
                _ => false,
            };
            nextinsn_or_fail!(matches)
        }

        Insn::MatchAny => match cursor.next(&mut s.pos) {
            Some(_) => nextinsn_or_fail!(true),
            _ => StateMatch::Fail,
        },

        Insn::MatchAnyExceptLineTerminator => match cursor.next(&mut s.pos) {
            Some(c2) => nextinsn_or_fail!(!Cursor::CharProps::is_line_terminator(c2)),
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
            if Cursor::FORWARD {
                std::debug_assert!(!group.start_matched(), "Group should not have been entered");
                group.start = s.pos;
            } else {
                std::debug_assert!(!group.end_matched(), "Group should not have been entered");
                group.end = s.pos;
            }
            nextinsn_or_fail!(true)
        }

        &Insn::EndCaptureGroup(group_idx) => {
            let group = &mut s.groups[group_idx as usize];
            if Cursor::FORWARD {
                std::debug_assert!(group.start_matched(), "Group should have been entered");
                group.end = s.pos;
            } else {
                std::debug_assert!(group.end_matched(), "Group should have been exited");
                group.start = s.pos;
            }
            std::debug_assert!(
                group.end.pos >= group.start.pos,
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
                    matched = matchers::backref_icase(orig_range, &mut s.pos, cursor);
                } else {
                    matched = matchers::backref(orig_range, &mut s.pos, cursor)
                }
            } else {
                // This group has not been exited, and therefore the match succeeds
                // (ES6 21.2.2.9).
                matched = true;
            }
            nextinsn_or_fail!(matched)
        }

        &Insn::LookaheadInsn {
            negate,
            start_group: _,
            end_group: _,
            continuation,
        } => {
            // Enter into the lookaround's instruction stream.
            s.ip += 1;
            let saved_pos = s.pos;
            let attempt_succeeded = MatchAttempter::new(re).try_at_pos(s, cursor.as_forward());
            let matched = attempt_succeeded != negate;
            if matched {
                s.ip = continuation as usize;
                s.pos = saved_pos;
                StateMatch::Continue
            } else {
                StateMatch::Fail
            }
        }

        &Insn::LookbehindInsn {
            negate,
            start_group: _,
            end_group: _,
            continuation,
        } => {
            // Enter into the lookaround's instruction stream.
            s.ip += 1;
            let saved_pos = s.pos;
            let attempt_succeeded = MatchAttempter::new(re).try_at_pos(s, cursor.as_backward());
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
                Insn::EnterLoop(ref lf) => run_loop(s, &lf, false),
                _ => panic!("LoopAgain does not point at EnterLoop"),
            }
        }
        Insn::Loop1CharBody { .. } => panic!("Loop1CharBody unimplemented for pikevm"),
        Insn::Bracket(bc) => match cursor.next(&mut s.pos) {
            Some(c) => nextinsn_or_fail!(Cursor::CharProps::bracket(bc, c)),
            _ => StateMatch::Fail,
        },

        Insn::AsciiBracket(bitmap) => match cursor.next_byte(&mut s.pos) {
            Some(c) => nextinsn_or_fail!(bitmap.contains(c)),
            _ => StateMatch::Fail,
        },

        &Insn::ByteSet2(bytes) => match cursor.next_byte(&mut s.pos) {
            Some(c) => nextinsn_or_fail!(bytes.contains(c)),
            _ => StateMatch::Fail,
        },

        &Insn::ByteSet3(bytes) => match cursor.next_byte(&mut s.pos) {
            Some(c) => nextinsn_or_fail!(bytes.contains(c)),
            _ => StateMatch::Fail,
        },

        &Insn::ByteSet4(bytes) => match cursor.next_byte(&mut s.pos) {
            Some(c) => nextinsn_or_fail!(bytes.contains(c)),
            _ => StateMatch::Fail,
        },

        &Insn::WordBoundary { invert } => {
            let prev_wordchar = cursor
                .peek_left(s.pos)
                .map_or(false, Cursor::CharProps::is_word_char);
            let curr_wordchar = cursor
                .peek_right(s.pos)
                .map_or(false, Cursor::CharProps::is_word_char);
            let is_boundary = prev_wordchar != curr_wordchar;
            nextinsn_or_fail!(is_boundary != invert)
        }
    }
}

fn successful_match(start: usize, state: &State) -> Match {
    let captures = state.groups.iter().map(GroupData::as_range).collect();
    Match {
        total_range: start..state.pos.pos,
        captures,
    }
}

#[derive(Debug)]
struct MatchAttempter<'a> {
    states: Vec<State>,
    re: &'a CompiledRegex,
}

impl<'a> MatchAttempter<'a> {
    fn new(re: &'a CompiledRegex) -> Self {
        Self {
            states: Vec::new(),
            re,
        }
    }

    fn try_at_pos<Cursor: Cursorable>(&mut self, init_state: &mut State, cursor: Cursor) -> bool {
        debug_assert!(self.states.is_empty(), "Should be no states");
        self.states.push(init_state.clone());
        while !self.states.is_empty() {
            let s = self.states.last_mut().unwrap();
            match try_match_state(self.re, cursor, s) {
                StateMatch::Fail => {
                    self.states.pop();
                }
                StateMatch::Continue => {}
                StateMatch::Complete => {
                    // Give the successful state to the caller.
                    std::mem::swap(init_state, s);
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
    cursor: Cursor<Forward, Input>,
    matcher: MatchAttempter<'r>,
}

impl<'r, 't> exec::Executor<'r, 't> for PikeVMExecutor<'r, Utf8Input<'t>> {
    type AsAscii = PikeVMExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = Utf8Input::new(text);
        Self {
            cursor: cursor::starting_cursor(input),
            matcher: MatchAttempter::new(re),
        }
    }
}

impl<'r, 't> exec::Executor<'r, 't> for PikeVMExecutor<'r, AsciiInput<'t>> {
    type AsAscii = PikeVMExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = AsciiInput::new(text);
        Self {
            cursor: cursor::starting_cursor(input),
            matcher: MatchAttempter::new(re),
        }
    }
}

impl<'a, Input: InputIndexer> exec::MatchProducer for PikeVMExecutor<'a, Input> {
    fn next_match(&mut self, mut pos: usize, next_start: &mut Option<usize>) -> Option<Match> {
        let re = &self.matcher.re;
        let mut state = State {
            pos: Position { pos },
            ip: 0,
            loops: vec![LoopData::new(); re.loops as usize],
            groups: vec![GroupData::new(); re.groups as usize],
        };
        loop {
            state.pos.pos = pos;
            if self.matcher.try_at_pos(&mut state, self.cursor) {
                let end = state.pos;
                *next_start = if end.pos != pos {
                    Some(end.pos)
                } else {
                    self.cursor.input.index_after_inc(end.pos)
                };
                return Some(successful_match(pos, &state));
            }
            match self.cursor.input.index_after_inc(pos) {
                Some(nextpos) => pos = nextpos,
                None => break,
            }
        }
        None
    }
}
