//! Classical backtracking execution engine

use crate::api::Match;
use crate::bytesearch;
use crate::cursor;
use crate::cursor::{Cursor, Cursorable, Forward, Position};
use crate::exec;
use crate::indexing::{AsciiInput, ElementType, InputIndexer, Utf8Input};
use crate::insn::{CompiledRegex, Insn, LoopFields, StartPredicate};
use crate::matchers;
use crate::matchers::CharProperties;
use crate::scm;
use crate::scm::SingleCharMatcher;
use crate::types::{CaptureGroupID, GroupData, LoopData, IP, MAX_CAPTURE_GROUPS};
use crate::util::DebugCheckIndex;
use std::hint::unreachable_unchecked;

#[derive(Clone, Debug)]
enum BacktrackInsn {
    /// Nothing more to backtrack.
    /// This "backstops" our stack.
    Exhausted,

    /// Restore the IP and position.
    SetPosition {
        ip: IP,
        pos: Position,
    },

    SetLoopData {
        id: u32,
        data: LoopData,
    },

    SetCaptureGroup {
        id: CaptureGroupID,
        data: GroupData,
    },

    EnterNonGreedyLoop {
        // The IP of the loop.
        // This is guaranteed to point to an EnterLoopInsn.
        ip: IP,
        data: LoopData,
    },

    GreedyLoop1Char {
        continuation: IP,
        min: Position,
        max: Position,
    },

    NonGreedyLoop1Char {
        continuation: IP,
        min: Position,
        max: Position,
    },
}

#[derive(Debug, Default)]
struct State {
    loops: Vec<LoopData>,
    groups: Vec<GroupData>,
}

impl State {
    fn new(re: &CompiledRegex) -> State {
        State {
            loops: vec![LoopData::new(); re.loops as usize],
            groups: vec![GroupData::new(); re.groups as usize],
        }
    }
}

#[derive(Debug)]
struct MatchAttempter<'a> {
    re: &'a CompiledRegex,
    bts: Vec<BacktrackInsn>,
    s: State,
}

impl<'a> MatchAttempter<'a> {
    fn new(re: &'a CompiledRegex) -> Self {
        Self {
            re,
            bts: vec![BacktrackInsn::Exhausted],
            s: State::new(re),
        }
    }

    #[inline(always)]
    fn push_backtrack(&mut self, bt: BacktrackInsn) {
        self.bts.push(bt)
    }

    #[inline(always)]
    fn pop_backtrack(&mut self) {
        // Note we never pop the last instruction so this will never be empty.
        debug_assert!(!self.bts.is_empty());
        if cfg!(feature = "prohibit-unsafe") {
            self.bts.pop();
        } else {
            unsafe { self.bts.set_len(self.bts.len() - 1) }
        }
    }

    fn prepare_to_enter_loop(
        bts: &mut Vec<BacktrackInsn>,
        pos: Position,
        loop_fields: &LoopFields,
        loop_data: &mut LoopData,
    ) {
        bts.push(BacktrackInsn::SetLoopData {
            id: loop_fields.loop_id,
            data: *loop_data,
        });
        loop_data.iters += 1;
        loop_data.entry = pos;
    }

    fn run_loop(&mut self, loop_fields: &'a LoopFields, pos: Position, ip: IP) -> Option<IP> {
        let loop_data = &mut self.s.loops[loop_fields.loop_id as usize];
        let iteration = loop_data.iters;

        let do_taken = iteration < loop_fields.max_iters;
        let do_not_taken = iteration >= loop_fields.min_iters;

        let loop_taken_ip = ip + 1 as IP;
        let loop_not_taken_ip = loop_fields.exit as IP;

        // If we have looped more than the minimum number of iterations, reject empty
        // matches. ES6 21.2.2.5.1 Note 4: "once the minimum number of
        // repetitions has been satisfied, any more expansions of Atom that match the
        // empty character sequence are not considered for further repetitions."
        if loop_data.entry == pos && iteration > loop_fields.min_iters {
            return None;
        }

        match (do_taken, do_not_taken) {
            (false, false) => {
                // No arms viable.
                None
            }
            (false, true) => {
                // Only skipping is viable.
                Some(loop_not_taken_ip)
            }
            (true, false) => {
                // Only entering is viable.
                MatchAttempter::prepare_to_enter_loop(&mut self.bts, pos, loop_fields, loop_data);
                Some(loop_taken_ip)
            }
            (true, true) if !loop_fields.greedy => {
                // Both arms are viable; backtrack into the loop.
                loop_data.entry = pos;
                self.bts.push(BacktrackInsn::EnterNonGreedyLoop {
                    ip,
                    data: *loop_data,
                });
                Some(loop_not_taken_ip)
            }
            (true, true) => {
                debug_assert!(loop_fields.greedy, "Should be greedy");
                // Both arms are viable; backtrack out of the loop.
                self.bts.push(BacktrackInsn::SetPosition {
                    ip: loop_not_taken_ip,
                    pos,
                });
                MatchAttempter::prepare_to_enter_loop(&mut self.bts, pos, loop_fields, loop_data);
                Some(loop_taken_ip)
            }
        }
    }

    // Drive the loop up to \p max times.
    // \return the position (min, max), or None on failure.
    #[inline(always)]
    fn run_scm_loop_impl<Cursor: Cursorable, Scm: SingleCharMatcher<Cursor>>(
        &mut self,
        matcher: Scm,
        min: usize,
        max: usize,
        mut pos: Position,
        cursor: Cursor,
    ) -> Option<(Position, Position)> {
        debug_assert!(min <= max, "min should be <= max");
        // Drive the iteration min times.
        // That tells us the min position.
        for _ in 0..min {
            if !matcher.matches(&mut pos, cursor) {
                return None;
            }
        }
        let min_pos = pos;

        // Drive it up to the max.
        // TODO; this is dumb.
        for _ in 0..(max - min) {
            let saved = pos;
            if !matcher.matches(&mut pos, cursor) {
                pos = saved;
                break;
            }
        }
        let max_pos = pos;
        Some((min_pos, max_pos))
    }

    // Given that ip points at a loop whose body matches exactly one character, run
    // a "single character loop". The big idea here is that we don't need to save
    // our position every iteration: we know that our loop body matches a single
    // character so we can backtrack by matching a character backwards.
    // \return the next IP, or None if the loop failed.
    fn run_scm_loop<Cursor: Cursorable>(
        &mut self,
        min: usize,
        max: usize,
        pos: &mut Position,
        cursor: Cursor,
        ip: IP,
        greedy: bool,
    ) -> Option<IP> {
        // Iterate as far as we can go.
        let loop_res = match self.re.insns.iat(ip + 1) {
            &Insn::Char(c) => {
                // Note this try_from may fail, for example if our char is outside ASCII.
                // In this case we wish to not match.
                let c = Cursor::Element::try_from(c)?;
                self.run_scm_loop_impl(scm::Char { c }, min, max, *pos, cursor)
            }
            &Insn::CharICase(c) => {
                let c = Cursor::Element::try_from(c)?;
                self.run_scm_loop_impl(scm::CharICase { c }, min, max, *pos, cursor)
            }
            Insn::Bracket(bc) => {
                self.run_scm_loop_impl(scm::Bracket { bc }, min, max, *pos, cursor)
            }
            Insn::AsciiBracket(bitmap) => {
                self.run_scm_loop_impl(scm::MatchByteSet { bytes: bitmap }, min, max, *pos, cursor)
            }
            Insn::MatchAny => self.run_scm_loop_impl(scm::MatchAny::new(), min, max, *pos, cursor),
            Insn::MatchAnyExceptLineTerminator => self.run_scm_loop_impl(
                scm::MatchAnyExceptLineTerminator::new(),
                min,
                max,
                *pos,
                cursor,
            ),
            Insn::CharSet(chars) => {
                self.run_scm_loop_impl(scm::CharSet { chars }, min, max, *pos, cursor)
            }
            &Insn::ByteSet2(bytes) => {
                self.run_scm_loop_impl(scm::MatchByteArraySet { bytes }, min, max, *pos, cursor)
            }
            &Insn::ByteSet3(bytes) => {
                self.run_scm_loop_impl(scm::MatchByteArraySet { bytes }, min, max, *pos, cursor)
            }
            &Insn::ByteSet4(bytes) => {
                self.run_scm_loop_impl(scm::MatchByteArraySet { bytes }, min, max, *pos, cursor)
            }
            Insn::ByteSeq1(bytes) => {
                self.run_scm_loop_impl(scm::MatchByteSeq { bytes }, min, max, *pos, cursor)
            }
            Insn::ByteSeq2(bytes) => {
                self.run_scm_loop_impl(scm::MatchByteSeq { bytes }, min, max, *pos, cursor)
            }
            Insn::ByteSeq3(bytes) => {
                self.run_scm_loop_impl(scm::MatchByteSeq { bytes }, min, max, *pos, cursor)
            }
            Insn::ByteSeq4(bytes) => {
                self.run_scm_loop_impl(scm::MatchByteSeq { bytes }, min, max, *pos, cursor)
            }
            Insn::ByteSeq5(bytes) => {
                self.run_scm_loop_impl(scm::MatchByteSeq { bytes }, min, max, *pos, cursor)
            }
            Insn::ByteSeq6(bytes) => {
                self.run_scm_loop_impl(scm::MatchByteSeq { bytes }, min, max, *pos, cursor)
            }
            _ => {
                // There should be no other SCMs.
                unreachable!("Missing SCM: {:?}", self.re.insns.iat(ip + 1));
            }
        };

        // If loop_res is none, we failed to match at least the minimum.
        let (min_pos, max_pos) = loop_res?;
        debug_assert!(
            if Cursor::FORWARD {
                min_pos <= max_pos
            } else {
                min_pos >= max_pos
            },
            "min should be <= (>=) max if cursor is tracking forwards (backwards)"
        );

        // Oh no where is the continuation? It's one past the loop body, which is one
        // past the loop. Strap in!
        let continuation = ip + 2;
        if min_pos != max_pos {
            // Backtracking is possible.
            let bti = if greedy {
                BacktrackInsn::GreedyLoop1Char {
                    continuation,
                    min: min_pos,
                    max: max_pos,
                }
            } else {
                BacktrackInsn::NonGreedyLoop1Char {
                    continuation,
                    min: min_pos,
                    max: max_pos,
                }
            };
            self.bts.push(bti);
        }

        // Start at the max (min) if greedy (nongreedy).
        *pos = if greedy { max_pos } else { min_pos };
        Some(continuation)
    }

    // Run a lookaround instruction, which is either forwards or backwards
    // (according to the direction of Cursor). The half-open range
    // start_group..end_group is the range of contained capture groups.
    // \return whether we matched and negate was false, or did not match but negate
    // is true.
    fn run_lookaround<Cursor: Cursorable>(
        &mut self,
        ip: IP,
        pos: Position,
        cursor: Cursor,
        start_group: CaptureGroupID,
        end_group: CaptureGroupID,
        negate: bool,
    ) -> bool {
        // Copy capture groups, because if the match fails (or if we are inverted)
        // we need to restore these.
        let range = (start_group as usize)..(end_group as usize);
        // TODO: consider retaining storage here?
        // Temporarily defeat backtracking.
        let saved_groups: Vec<GroupData> = self.s.groups.iat(range.clone()).to_vec();

        // Start with an "empty" backtrack stack.
        // TODO: consider using a stack-allocated array.
        let mut saved_bts = vec![BacktrackInsn::Exhausted];
        std::mem::swap(&mut self.bts, &mut saved_bts);

        // Enter into the lookaround's instruction stream.
        let matched = self.try_at_pos(ip, pos, cursor).is_some();

        // Put back our bts.
        std::mem::swap(&mut self.bts, &mut saved_bts);

        // If we are a positive lookahead that successfully matched, retain the
        // capture groups (but we need to set up backtracking). Otherwise restore
        // them.
        if matched && !negate {
            for (idx, cg) in saved_groups.iter().enumerate() {
                debug_assert!(idx + (start_group as usize) < MAX_CAPTURE_GROUPS);
                self.push_backtrack(BacktrackInsn::SetCaptureGroup {
                    id: (idx as CaptureGroupID) + start_group,
                    data: *cg,
                });
            }
        } else {
            self.s.groups.splice(range, saved_groups);
        }
        matched != negate
    }

    /// Attempt to backtrack.
    /// \return true if we backtracked, false if we exhaust the backtrack stack.
    fn try_backtrack<Cursor: Cursorable>(
        &mut self,
        ip: &mut IP,
        pos: &mut Position,
        cursor: Cursor,
    ) -> bool {
        loop {
            // We always have a single Exhausted instruction backstopping our stack,
            // so we do not need to check for empty bts.
            debug_assert!(!self.bts.is_empty(), "Backtrack stack should not be empty");
            let bt = match self.bts.last_mut() {
                Some(bt) => bt,
                None => {
                    if cfg!(feature = "prohibit-unsafe") {
                        unreachable!();
                    } else {
                        unsafe { unreachable_unchecked() }
                    }
                }
            };
            match bt {
                BacktrackInsn::Exhausted => return false,

                BacktrackInsn::SetPosition {
                    ip: saved_ip,
                    pos: saved_pos,
                } => {
                    *ip = *saved_ip;
                    *pos = *saved_pos;
                    self.pop_backtrack();
                    return true;
                }
                BacktrackInsn::SetLoopData { id, data } => {
                    *self.s.loops.mat(*id as usize) = *data;
                    self.pop_backtrack();
                }
                BacktrackInsn::SetCaptureGroup { id, data } => {
                    *self.s.groups.mat(*id as usize) = *data;
                    self.pop_backtrack();
                }

                &mut BacktrackInsn::EnterNonGreedyLoop { ip: loop_ip, data } => {
                    // Must pop before we enter the loop.
                    self.pop_backtrack();
                    *ip = loop_ip + 1;
                    *pos = data.entry;
                    let loop_fields = match &self.re.insns.iat(loop_ip) {
                        Insn::EnterLoop(loop_fields) => loop_fields,
                        _ => {
                            if cfg!(feature = "prohibit-unsafe") {
                                unreachable!();
                            } else {
                                unsafe { unreachable_unchecked() }
                            }
                        }
                    };
                    let loop_data = self.s.loops.mat(loop_fields.loop_id as usize);
                    *loop_data = data;
                    MatchAttempter::prepare_to_enter_loop(
                        &mut self.bts,
                        *pos,
                        loop_fields,
                        loop_data,
                    );
                    return true;
                }

                BacktrackInsn::GreedyLoop1Char {
                    continuation,
                    min,
                    max,
                } => {
                    // The match failed at the max location.
                    debug_assert!(
                        if Cursor::FORWARD {
                            max >= min
                        } else {
                            max <= min
                        },
                        "max should be >= min (or <= if tracking backwards)"
                    );
                    if *max == *min {
                        // We have backtracked this loop as far as possible.
                        self.bts.pop();
                    } else {
                        // Move opposite the direction of the cursor.
                        cursor.retreat_by_char_known_valid(max);
                        *pos = *max;
                        *ip = *continuation;
                        return true;
                    }
                }

                BacktrackInsn::NonGreedyLoop1Char {
                    continuation,
                    min,
                    max,
                } => {
                    // The match failed at the min location.
                    debug_assert!(
                        if Cursor::FORWARD {
                            max >= min
                        } else {
                            max <= min
                        },
                        "max should be >= min (or <= if tracking backwards)"
                    );
                    if *max == *min {
                        // We have backtracked this loop as far as possible.
                        self.bts.pop();
                    } else {
                        // Move opposite the direction of the cursor.
                        cursor.advance_by_char_known_valid(min);
                        *pos = *min;
                        *ip = *continuation;
                        return true;
                    }
                }
            }
        }
    }

    /// Attempt to match at a given IP and position.
    fn try_at_pos<Cursor: Cursorable>(
        &mut self,
        mut ip: IP,
        mut pos: Position,
        cursor: Cursor,
    ) -> Option<Position> {
        debug_assert!(
            self.bts.len() == 1,
            "Should be only initial exhausted backtrack insn"
        );
        let re = self.re;

        // These are not really loops, they are just labels that we effectively 'goto'
        // to.
        #[allow(clippy::never_loop)]
        'nextinsn: loop {
            'backtrack: loop {
                // Helper macro to either increment ip and go to the next insn, or backtrack.
                macro_rules! next_or_bt {
                    ($e:expr) => {
                        if $e {
                            ip += 1;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    };
                };

                match re.insns.iat(ip) {
                    &Insn::Char(c) => {
                        let m = match Cursor::Element::try_from(c) {
                            Some(c) => scm::Char { c }.matches(&mut pos, cursor),
                            None => false,
                        };
                        next_or_bt!(m);
                    }

                    Insn::CharSet(chars) => {
                        let m = scm::CharSet { chars }.matches(&mut pos, cursor);
                        next_or_bt!(m);
                    }

                    &Insn::ByteSet2(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet { bytes }.matches(&mut pos, cursor))
                    }
                    &Insn::ByteSet3(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet { bytes }.matches(&mut pos, cursor))
                    }
                    &Insn::ByteSet4(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet { bytes }.matches(&mut pos, cursor))
                    }

                    Insn::ByteSeq1(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq2(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq3(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq4(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq5(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq6(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq7(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq8(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq9(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq10(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq11(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq12(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq13(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq14(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq15(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),
                    Insn::ByteSeq16(v) => next_or_bt!(cursor.try_match_lit(&mut pos, v)),

                    &Insn::CharICase(c) => {
                        let m = match Cursor::Element::try_from(c) {
                            Some(c) => scm::CharICase { c }.matches(&mut pos, cursor),
                            None => false,
                        };
                        next_or_bt!(m)
                    }

                    Insn::AsciiBracket(bitmap) => {
                        next_or_bt!(scm::MatchByteSet { bytes: bitmap }.matches(&mut pos, cursor))
                    }

                    Insn::Bracket(bc) => next_or_bt!(scm::Bracket { bc }.matches(&mut pos, cursor)),

                    Insn::MatchAny => next_or_bt!(scm::MatchAny::new().matches(&mut pos, cursor)),

                    Insn::MatchAnyExceptLineTerminator => next_or_bt!(
                        scm::MatchAnyExceptLineTerminator::new().matches(&mut pos, cursor)
                    ),

                    &Insn::WordBoundary { invert } => {
                        let prev_wordchar = cursor
                            .peek_left(pos)
                            .map_or(false, Cursor::CharProps::is_word_char);
                        let curr_wordchar = cursor
                            .peek_right(pos)
                            .map_or(false, Cursor::CharProps::is_word_char);
                        let is_boundary = prev_wordchar != curr_wordchar;
                        next_or_bt!(is_boundary != invert)
                    }

                    Insn::StartOfLine => {
                        let matches = match cursor.peek_left(pos) {
                            None => true,
                            Some(c)
                                if re.flags.multiline
                                    && Cursor::CharProps::is_line_terminator(c) =>
                            {
                                true
                            }
                            _ => false,
                        };
                        next_or_bt!(matches)
                    }
                    Insn::EndOfLine => {
                        let matches = match cursor.peek_right(pos) {
                            None => true, // we're at the right of the string
                            Some(c)
                                if re.flags.multiline
                                    && Cursor::CharProps::is_line_terminator(c) =>
                            {
                                true
                            }
                            _ => false,
                        };
                        next_or_bt!(matches)
                    }

                    &Insn::Jump { target } => {
                        ip = target as usize;
                        continue 'nextinsn;
                    }

                    &Insn::BeginCaptureGroup(cg_idx) => {
                        let cg: &mut GroupData = self.s.groups.mat(cg_idx as usize);
                        self.bts.push(BacktrackInsn::SetCaptureGroup {
                            id: cg_idx,
                            data: *cg,
                        });
                        if Cursor::FORWARD {
                            cg.start = pos
                        } else {
                            cg.end = pos
                        }
                        next_or_bt!(true)
                    }

                    &Insn::EndCaptureGroup(cg_idx) => {
                        let cg: &mut GroupData = self.s.groups.mat(cg_idx as usize);
                        if Cursor::FORWARD {
                            debug_assert!(
                                cg.start_matched(),
                                "Capture group should have been entered"
                            );
                            cg.end = pos;
                        } else {
                            debug_assert!(
                                cg.end_matched(),
                                "Capture group should have been entered"
                            );
                            cg.start = pos
                        }
                        next_or_bt!(true)
                    }

                    &Insn::ResetCaptureGroup(cg_idx) => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        self.bts.push(BacktrackInsn::SetCaptureGroup {
                            id: cg_idx,
                            data: *cg,
                        });
                        cg.reset();
                        next_or_bt!(true)
                    }

                    &Insn::BackRef(cg_idx) => {
                        let cg: &mut GroupData = self.s.groups.mat(cg_idx as usize);
                        // Backreferences to a capture group that did not match always succeed (ES5
                        // 15.10.2.9).
                        // Note we may be in the capture group we are examining, e.g. /(abc\1)/.
                        let matched;
                        if let Some(orig_range) = cg.as_range() {
                            if re.flags.icase {
                                matched = matchers::backref_icase(orig_range, &mut pos, cursor);
                            } else {
                                matched = matchers::backref(orig_range, &mut pos, cursor);
                            }
                        } else {
                            // This group has not been exited and so the match succeeds (ES6
                            // 21.2.2.9).
                            matched = true;
                        }
                        next_or_bt!(matched)
                    }

                    &Insn::LookaheadInsn {
                        negate,
                        start_group,
                        end_group,
                        continuation,
                    } => {
                        if self.run_lookaround(
                            ip + 1,
                            pos,
                            cursor.as_forward(),
                            start_group,
                            end_group,
                            negate,
                        ) {
                            ip = continuation as IP;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    &Insn::LookbehindInsn {
                        negate,
                        start_group,
                        end_group,
                        continuation,
                    } => {
                        if self.run_lookaround(
                            ip + 1,
                            pos,
                            cursor.as_backward(),
                            start_group,
                            end_group,
                            negate,
                        ) {
                            ip = continuation as IP;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    &Insn::Alt { secondary } => {
                        self.push_backtrack(BacktrackInsn::SetPosition {
                            ip: secondary as IP,
                            pos,
                        });
                        next_or_bt!(true);
                    }

                    Insn::EnterLoop(fields) => {
                        // Entering a loop, not re-entering it.
                        self.s.loops.mat(fields.loop_id as usize).iters = 0;
                        match self.run_loop(&fields, pos, ip) {
                            Some(next_ip) => {
                                ip = next_ip;
                                continue 'nextinsn;
                            }
                            None => {
                                break 'backtrack;
                            }
                        }
                    }

                    &Insn::LoopAgain { begin } => {
                        let act = match re.insns.iat(begin as IP) {
                            Insn::EnterLoop(fields) => self.run_loop(fields, pos, begin as IP),
                            _ => {
                                if cfg!(feature = "prohibit-unsafe") {
                                    unreachable!();
                                } else {
                                    unsafe { unreachable_unchecked() }
                                }
                            }
                        };
                        match act {
                            Some(next_ip) => {
                                ip = next_ip;
                                continue 'nextinsn;
                            }
                            None => break 'backtrack,
                        }
                    }

                    &Insn::Loop1CharBody {
                        min_iters,
                        max_iters,
                        greedy,
                    } => {
                        if let Some(next_ip) =
                            self.run_scm_loop(min_iters, max_iters, &mut pos, cursor, ip, greedy)
                        {
                            ip = next_ip;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }
                    Insn::Goal => {
                        // Keep all but the initial give-up bts.
                        self.bts.truncate(1);
                        return Some(pos);
                    }

                    Insn::JustFail => {
                        break 'backtrack;
                    }
                }
            }

            // This after the backtrack loop.
            // A break 'backtrack will jump here.
            if self.try_backtrack(&mut ip, &mut pos, cursor) {
                continue 'nextinsn;
            } else {
                // We have exhausted the backtracking stack.
                debug_assert!(self.bts.len() == 1, "Should have exhausted backtrack stack");
                return None;
            }
        }

        // This is outside the nextinsn loop.
        // It is an error to get here.
        // Every instruction should either continue 'nextinsn, or break 'backtrack.
        {
            #![allow(unreachable_code)]
            if cfg!(feature = "prohibit-unsafe") {
                unreachable!();
            } else {
                unsafe { unreachable_unchecked() }
            }
        }
    }
}

#[derive(Debug)]
pub struct BacktrackExecutor<'r, Input: InputIndexer> {
    cursor: Cursor<Forward, Input>,
    matcher: MatchAttempter<'r>,
}

impl<'r, Input: InputIndexer> BacktrackExecutor<'r, Input> {
    fn successful_match(&mut self, start: usize, end: usize) -> Match {
        let captures = self
            .matcher
            .s
            .groups
            .iter()
            .map(GroupData::as_range)
            .collect();
        Match {
            total_range: start..end,
            captures,
        }
    }

    /// \return the next match, searching the remaining bytes using the given
    /// prefix searcher to quickly find the first potential match location.
    fn next_match_with_prefix_search<PrefixSearch: bytesearch::ByteSearcher>(
        &mut self,
        upos: usize,
        next_start: &mut Option<usize>,
        prefix_search: &PrefixSearch,
    ) -> Option<Match> {
        let mut pos = Position { pos: upos };
        loop {
            // Find the next start location.
            let rem = self.cursor.remaining_bytes(pos);
            if let Some(start_pos) = prefix_search.find_in(rem) {
                pos.pos += start_pos
            } else {
                return None;
            }
            if let Some(end) = self.matcher.try_at_pos(0, pos, self.cursor) {
                // If we matched the empty string, we have to increment.
                if end != pos {
                    *next_start = Some(end.pos)
                } else {
                    *next_start = self.cursor.input.index_after_inc(end.pos);
                }
                return Some(self.successful_match(pos.pos, end.pos));
            }
            match self.cursor.input.index_after_inc(pos.pos) {
                Some(nextpos) => pos.pos = nextpos,
                None => return None,
            }
        }
    }
}

impl<'a, Input: InputIndexer> exec::MatchProducer for BacktrackExecutor<'a, Input> {
    fn next_match(&mut self, upos: usize, next_start: &mut Option<usize>) -> Option<Match> {
        match &self.matcher.re.start_pred {
            StartPredicate::Arbitrary => {
                self.next_match_with_prefix_search(upos, next_start, &bytesearch::EmptyString {})
            }
            StartPredicate::ByteSeq1(bytes) => {
                self.next_match_with_prefix_search(upos, next_start, bytes)
            }
            StartPredicate::ByteSeq2(bytes) => {
                self.next_match_with_prefix_search(upos, next_start, bytes)
            }
            StartPredicate::ByteSeq3(bytes) => {
                self.next_match_with_prefix_search(upos, next_start, bytes)
            }
            StartPredicate::ByteSeq4(bytes) => {
                self.next_match_with_prefix_search(upos, next_start, bytes)
            }
            &StartPredicate::ByteSet2(bytes) => self.next_match_with_prefix_search(
                upos,
                next_start,
                &bytesearch::ByteArraySet(bytes),
            ),
            StartPredicate::ByteBracket(bitmap) => {
                self.next_match_with_prefix_search(upos, next_start, bitmap)
            }
        }
    }
}

impl<'r, 't> exec::Executor<'r, 't> for BacktrackExecutor<'r, Utf8Input<'t>> {
    type AsAscii = BacktrackExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = Utf8Input::new(text);
        Self {
            cursor: cursor::starting_cursor(input),
            matcher: MatchAttempter::new(re),
        }
    }
}

impl<'r, 't> exec::Executor<'r, 't> for BacktrackExecutor<'r, AsciiInput<'t>> {
    type AsAscii = BacktrackExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = AsciiInput::new(text);
        Self {
            cursor: cursor::starting_cursor(input),
            matcher: MatchAttempter::new(re),
        }
    }
}
