//! Conversion of IR to non-deterministic finite automata.

use crate::automata::nfa_optimize::optimize_states;
use crate::automata::util::node_description;
use crate::ir::{self, Node};
use crate::types::CaptureGroupID;
use crate::unicode;
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::ToString, vec::Vec};
use core::iter::once;
use smallvec::{smallvec, SmallVec};

#[derive(Debug)]
pub struct Nfa {
    pub(super) start: StateHandle,
    pub(super) states: Box<[State]>,
    pub(super) num_registers: usize,
}

// A handle to a State in the NFA.
// This is implemented as an index but is remapped to a dense vector later.
pub type StateHandle = u32;

// An index of a capture register. Each capture group has two registers, for open and close.
// We reserve the first register pair for the entire match.
// Thus, the first explicit capture group has index 2 for open and 3 for close.
pub type RegisterIdx = u32;

pub const FULL_MATCH_START: RegisterIdx = 0;
pub const FULL_MATCH_END: RegisterIdx = 1;

// A captured position in the text.
pub type TextPos = usize;

// Sentinel position meaning no match.
pub const TEXT_POS_NO_MATCH: TextPos = usize::MAX;

// A closed range of bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ByteRange {
    pub start: u8,
    pub end: u8,
}

impl ByteRange {
    #[inline]
    pub fn new(start: u8, end: u8) -> Self {
        assert!(start <= end);
        Self { start, end }
    }
}

// A piece of an NFA, with a start node handle  and a set of "loose ends."
// These loose ends need epsilon transitions to the next start.
struct Fragment {
    start: StateHandle,
    ends: SmallVec<[StateHandle; 2]>,
}

impl Fragment {
    // Construct a Fragment from a start and from loose ends.
    #[inline]
    fn new(start: StateHandle, ends: impl IntoIterator<Item = StateHandle>) -> Self {
        Self {
            start,
            ends: ends.into_iter().collect(),
        }
    }
}

// An epsilon edge. Transitions on empty input, optionally writing to one or more registers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct EpsEdge {
    // Target of the transition.
    pub target: StateHandle,
    // Registers to write to on transition.
    pub ops: SmallVec<[RegisterIdx; 2]>,
}

impl EpsEdge {
    // Create a new epsilon edge going to a target state, with no register writes.
    fn to_target(target: StateHandle) -> Self {
        Self {
            target,
            ops: smallvec![],
        }
    }
}

// A state (node) in the TNFA.
#[derive(Debug, Default)]
pub(super) struct State {
    // Tagged epsilon transitions to other states, in priority order.
    pub(super) eps: Vec<EpsEdge>,

    // Transitions to other states, indexed by sorted byte ranges.
    pub(super) transitions: Vec<(ByteRange, StateHandle)>,
}

impl State {
    // Add an epsilon transition to another state, with no register writes.
    pub fn add_eps(&mut self, target: StateHandle) {
        self.eps.push(EpsEdge {
            target,
            ops: smallvec![],
        });
    }

    // Add an epsilon transition to another state, with one register write.
    pub fn add_eps_with_write(&mut self, target: StateHandle, reg: RegisterIdx) {
        self.eps.push(EpsEdge {
            target,
            ops: smallvec![reg],
        });
    }

    // Add a byte transition to another state.
    pub fn add_transition(&mut self, range: ByteRange, dest: StateHandle) {
        debug_assert!(range.start <= range.end);

        // Common fast path.
        if self.transitions.is_empty() {
            self.transitions.push((range, dest));
            return;
        }

        // Find insertion point by start and then insert.
        let i = self
            .transitions
            .partition_point(|(r, _)| r.start < range.start);

        // Should be no overlaps.
        debug_assert!(i == 0 || self.transitions[i - 1].0.end < range.start);
        debug_assert!(i == self.transitions.len() || self.transitions[i].0.start > range.end);

        // We're going to merge ranges.
        let mut merged = range;

        // Merge with previous values.
        let mut idx_start = i;
        while idx_start > 0 {
            let (prev_range, prev_state) = self.transitions[idx_start - 1];
            debug_assert!(prev_range.end < merged.start, "Ranges should never overlap");
            if prev_state == dest && prev_range.end + 1 == merged.start {
                merged.start = prev_range.start;
                idx_start -= 1;
            } else {
                break;
            }
        }

        // Merge with next values.
        let mut idx_end = i;
        while idx_end < self.transitions.len() {
            let (next_range, next_state) = self.transitions[idx_end];
            debug_assert!(next_range.start > merged.end, "Ranges should never overlap");
            if next_state == dest && next_range.start == merged.end + 1 {
                merged.end = next_range.end;
                idx_end += 1;
            } else {
                break;
            }
        }

        // Replace the (potentially empty) range with the merged one.
        self.transitions
            .splice(idx_start..idx_end, once((merged, dest)));
    }

    pub fn transition_for_byte(&self, byte: u8) -> Option<StateHandle> {
        let i = self.transitions.partition_point(|(r, _)| r.start <= byte);
        if i == 0 {
            return None;
        }
        let (r, dst) = &self.transitions[i - 1];
        (byte <= r.end).then_some(*dst)
    }

    fn goal() -> Self {
        Self::default()
    }
}

pub const GOAL_STATE: StateHandle = 0;

pub(super) struct Builder {
    // States indexed by handle.
    pub(super) states: Vec<State>,
    pub(super) state_budget: usize,
    pub(super) num_registers: usize,
    pub(super) unicode: bool,
}

#[derive(Debug)]
pub enum Error {
    UnsupportedInstruction(String),
    NotUTF8,
    BudgetExceeded,
}
pub type Result<T> = core::result::Result<T, Error>;

impl Builder {
    fn new(state_budget: usize, unicode: bool) -> Self {
        Builder {
            states: vec![State::goal()],
            state_budget,
            num_registers: 2, // Initially two registers for full match start and end
            unicode,
        }
    }

    // Build a start state, that captures the beginning.
    fn build_start(&mut self) -> Result<Fragment> {
        let start = self.make()?;
        let end = self.join_with_write(&[start], FULL_MATCH_START)?;
        Ok(Fragment::new(start, [end]))
    }

    // Build a transition to a goal state.
    // Crucially this has NO dangling ends.
    fn build_goal(&mut self) -> Result<Fragment> {
        let start = self.make()?;
        self.get(start)
            .add_eps_with_write(GOAL_STATE, FULL_MATCH_END);
        Ok(Fragment::new(start, []))
    }

    fn build(&mut self, node: &Node) -> Result<Fragment> {
        match node {
            Node::Empty => self.build_empty(),
            Node::Goal => self.build_goal(),
            Node::Char { c, icase } if *icase => self.build_icase_char(*c),
            Node::Char { c, icase } if !*icase => self.build_char(*c),
            Node::ByteSequence(seq) => self.build_sequence(seq),
            Node::ByteSet(bytes) => self.build_byte_set(bytes),
            Node::CharSet(chars) => self.build_char_set(chars),
            Node::Cat(seq) => self.build_cat(seq),
            Node::Alt(left, right) => self.build_alt(left, right),
            Node::Loop1CharBody { loopee, quant } => self.build_loop(loopee, quant),
            Node::Loop { loopee, quant, .. } => self.build_loop(loopee, quant),
            Node::CaptureGroup(contents, group_id) => self.build_capture_group(contents, *group_id),
            Node::NamedCaptureGroup(contents, group_id, _name) => {
                self.build_capture_group(contents, *group_id)
            }
            Node::BackRef(_) => Err(Error::UnsupportedInstruction(
                "Backreferences not supported by NFAs".to_string(),
            )),
            Node::Bracket(contents) => self.build_bracket(contents),

            // All other node types are unsupported
            unsupported => Err(Error::UnsupportedInstruction(node_description(unsupported))),
        }
    }

    /// Try adding a new state, returning its handle.
    fn make(&mut self) -> Result<StateHandle> {
        if self.states.len() < self.state_budget {
            self.states.push(State::default());
            let new_handle = self.states.len() as StateHandle - 1;
            Ok(new_handle)
        } else {
            Err(Error::BudgetExceeded)
        }
    }

    /// Access a state by handle.
    fn get(&mut self, idx: StateHandle) -> &mut State {
        &mut self.states[idx as usize]
    }

    /// Ensure there is a single-byte transition for `byte` from `from`.
    /// If it exists, return the existing destination; otherwise create it.
    fn ensure_edge(&mut self, from: StateHandle, byte: u8) -> Result<StateHandle> {
        if let Some(dst) = self.states[from as usize].transition_for_byte(byte) {
            Ok(dst)
        } else {
            let next = self.make()?;
            self.states[from as usize].add_transition(ByteRange::new(byte, byte), next);
            Ok(next)
        }
    }

    /// Add epsilons from a handle to more handles, without register writes.
    fn connect_eps(&mut self, from: StateHandle, to: impl IntoIterator<Item = StateHandle>) {
        let edges = to.into_iter().map(EpsEdge::to_target);
        self.get(from).eps.extend(edges);
    }

    /// Add epsilons from a list of handles to a single handle.
    fn join_eps(&mut self, from: &[StateHandle], to: StateHandle) {
        for &src in from {
            self.get(src).add_eps(to);
        }
    }

    /// Add epsilons from a list of handles to a list of handles.
    fn join_many_eps(&mut self, from: &[StateHandle], to: impl IntoIterator<Item = StateHandle>) {
        for dst in to {
            self.join_eps(from, dst);
        }
    }

    /// Add a single register write on an epsilon transition from a list of handles, producing a new handle.
    fn join_with_write(&mut self, from: &[StateHandle], reg: RegisterIdx) -> Result<StateHandle> {
        assert!(!from.is_empty());
        let target = self.make()?;
        // Join the existing states into one first, if we have more than one.
        let joined: StateHandle = if from.len() == 1 {
            from[0]
        } else {
            let joined = self.make()?;
            self.join_eps(from, joined);
            joined
        };
        self.get(joined).add_eps_with_write(target, reg);
        Ok(target)
    }

    /// Build an alternation of nodes.
    fn build_alt(&mut self, left: &Node, right: &Node) -> Result<Fragment> {
        let start = self.make()?;
        let left = self.build(left)?;
        let right = self.build(right)?;
        // Note: left has priority.
        self.connect_eps(start, [left.start, right.start]);

        // Construct all loose ends.
        let mut ends = left.ends;
        ends.extend(right.ends);
        Ok(Fragment { start, ends })
    }

    // Build an empty node which matches the empty string.
    fn build_empty(&mut self) -> Result<Fragment> {
        let start = self.make()?;
        Ok(Fragment::new(start, [start]))
    }

    /// Build a sequence of nodes.
    fn build_cat(&mut self, cat: &[Node]) -> Result<Fragment> {
        let mut ends = smallvec![];
        let mut start = None;
        for node in cat {
            let next = self.build(node)?;
            if start.is_none() {
                start = Some(next.start);
            }
            self.join_eps(&ends, next.start);
            ends = next.ends;
        }
        // Handle empties (unlikely).
        let start = match start {
            Some(s) => s,
            None => self.make()?,
        };
        Ok(Fragment { start, ends })
    }

    fn build_icase_char(&mut self, c: u32) -> Result<Fragment> {
        let unfolded = if self.unicode {
            unicode::unfold_char(c)
        } else {
            unicode::unfold_uppercase_char(c)
        };
        self.build_char_set(&unfolded)
    }

    fn build_char(&mut self, c: u32) -> Result<Fragment> {
        // Convert u32 to char
        let ch = match char::from_u32(c) {
            Some(ch) => ch,
            None => return Err(Error::NotUTF8),
        };

        let mut buff = [0; 4];
        let s = ch.encode_utf8(&mut buff);
        self.build_sequence(s.as_bytes())
    }

    fn build_char_set(&mut self, cs: &[u32]) -> Result<Fragment> {
        // Handle empty character set - create a node with no outgoing edges (fails to match)
        if cs.is_empty() {
            let start = self.make()?;
            return Ok(Fragment::new(start, []));
        }

        if cs.len() == 1 {
            return self.build_char(cs[0]);
        }

        // For very large character sets (like [\s\S]), create a universal UTF-8 validator
        // instead of enumerating all codepoints
        if cs.len() > 500 {
            return self.build_universal_utf8_validator();
        }

        // Convert the character set to intervals for range-based optimization
        let mut intervals = Vec::new();
        let mut sorted_cs = cs.to_vec();
        sorted_cs.sort_unstable();

        let mut start_cp = sorted_cs[0];
        let mut end_cp = sorted_cs[0];

        for &cp in &sorted_cs[1..] {
            if cp == end_cp + 1 {
                // Consecutive codepoint - extend the current interval
                end_cp = cp;
            } else {
                // Gap found - close current interval and start new one
                intervals.push(crate::codepointset::Interval {
                    first: start_cp,
                    last: end_cp,
                });
                start_cp = cp;
                end_cp = cp;
            }
        }
        // Don't forget the last interval
        intervals.push(crate::codepointset::Interval {
            first: start_cp,
            last: end_cp,
        });

        // Use the same range-based approach as brackets
        self.build_byte_range_trie_from_intervals(&intervals)
    }

    fn build_bracket(&mut self, contents: &crate::types::BracketContents) -> Result<Fragment> {
        // Invert the code point set if necessary.
        let inverted_cps;
        let cps = if contents.invert {
            inverted_cps = contents.cps.inverted();
            &inverted_cps
        } else {
            &contents.cps
        };

        // Build byte range trie from Unicode intervals efficiently
        self.build_byte_range_trie_from_intervals(cps.intervals())
    }

    fn build_byte_range_trie_from_intervals(
        &mut self,
        intervals: &[crate::codepointset::Interval],
    ) -> Result<Fragment> {
        // Check if this is a truly universal character set (100% coverage)
        // Only use the universal validator if we literally match ALL valid Unicode codepoints
        if self.is_truly_universal_character_set(intervals) {
            return self.build_universal_utf8_validator();
        }

        // Use a more efficient UTF-8 trie construction approach
        self.build_efficient_utf8_trie(intervals)
    }

    /// Check if the given intervals represent a truly universal character set.
    /// This means they cover ALL valid Unicode codepoints (0x0 to 0x10FFFF) with no gaps.
    fn is_truly_universal_character_set(
        &self,
        intervals: &[crate::codepointset::Interval],
    ) -> bool {
        if intervals.is_empty() {
            return false;
        }

        // Sort intervals by start position to check for gaps
        let mut sorted_intervals: Vec<_> = intervals.iter().copied().collect();
        sorted_intervals.sort_by_key(|interval| interval.first);

        // Check that we start at 0 and end at or beyond 0x10FFFF with no gaps
        let mut expected_next = 0u32;
        for interval in sorted_intervals {
            // If there's a gap before this interval, not universal
            if interval.first > expected_next {
                return false;
            }
            // Update the expected next position (intervals are inclusive)
            expected_next = interval.last.saturating_add(1);
        }

        // We're universal if we've covered up to and beyond the last valid Unicode codepoint
        expected_next > 0x10FFFF
    }

    /// Build an efficient UTF-8 trie from Unicode intervals.
    /// Instead of enumerating individual codepoints, this works with UTF-8 byte patterns.
    fn build_efficient_utf8_trie(
        &mut self,
        intervals: &[crate::codepointset::Interval],
    ) -> Result<Fragment> {
        let start = self.make()?;
        let end = self.make()?;

        // Group intervals by UTF-8 byte length and build patterns for each group
        let mut one_byte_ranges = Vec::new();
        let mut two_byte_patterns = Vec::new();
        let mut three_byte_patterns = Vec::new();
        let mut four_byte_patterns = Vec::new();

        for &interval in intervals {
            self.collect_utf8_patterns(
                interval,
                &mut one_byte_ranges,
                &mut two_byte_patterns,
                &mut three_byte_patterns,
                &mut four_byte_patterns,
            );
        }

        // Create shared continuation byte states
        let cont1 = self.make()?; // [80-BF] -> end
        let cont2 = self.make()?; // [80-BF] -> cont1
        let cont3 = self.make()?; // [80-BF] -> cont2

        // Set up continuation byte transitions
        self.get(cont1)
            .add_transition(ByteRange::new(0x80, 0xBF), end);
        self.get(cont2)
            .add_transition(ByteRange::new(0x80, 0xBF), cont1);
        self.get(cont3)
            .add_transition(ByteRange::new(0x80, 0xBF), cont2);

        // Add 1-byte UTF-8 patterns (ASCII)
        for (first_byte, last_byte) in one_byte_ranges {
            self.get(start)
                .add_transition(ByteRange::new(first_byte, last_byte), end);
        }

        // Add 2-byte UTF-8 patterns
        for (first_byte_start, first_byte_end, second_byte_start, second_byte_end) in
            two_byte_patterns
        {
            if first_byte_start == first_byte_end
                && second_byte_start == 0x80
                && second_byte_end == 0xBF
            {
                // Simple case: single first byte, all continuation bytes
                self.get(start)
                    .add_transition(ByteRange::new(first_byte_start, first_byte_start), cont1);
            } else {
                // Complex case: need intermediate states for specific ranges
                for first_byte in first_byte_start..=first_byte_end {
                    let intermediate = self.make()?;
                    self.get(start)
                        .add_transition(ByteRange::new(first_byte, first_byte), intermediate);
                    self.get(intermediate)
                        .add_transition(ByteRange::new(second_byte_start, second_byte_end), end);
                }
            }
        }

        // Add 3-byte UTF-8 patterns
        for (fb_start, fb_end, sb_start, sb_end, tb_start, tb_end) in three_byte_patterns {
            if fb_start == fb_end
                && sb_start == 0x80
                && sb_end == 0xBF
                && tb_start == 0x80
                && tb_end == 0xBF
            {
                // Simple case: single first byte, all continuation bytes
                self.get(start)
                    .add_transition(ByteRange::new(fb_start, fb_start), cont2);
            } else {
                // Complex case: create intermediate states
                for first_byte in fb_start..=fb_end {
                    let intermediate1 = self.make()?;
                    self.get(start)
                        .add_transition(ByteRange::new(first_byte, first_byte), intermediate1);

                    for second_byte in sb_start..=sb_end {
                        let intermediate2 = self.make()?;
                        self.get(intermediate1).add_transition(
                            ByteRange::new(second_byte, second_byte),
                            intermediate2,
                        );
                        self.get(intermediate2)
                            .add_transition(ByteRange::new(tb_start, tb_end), end);
                    }
                }
            }
        }

        // Add 4-byte UTF-8 patterns
        for (fb_start, fb_end, sb_start, sb_end, tb_start, tb_end, fob_start, fob_end) in
            four_byte_patterns
        {
            if fb_start == fb_end
                && sb_start == 0x80
                && sb_end == 0xBF
                && tb_start == 0x80
                && tb_end == 0xBF
                && fob_start == 0x80
                && fob_end == 0xBF
            {
                // Simple case: single first byte, all continuation bytes
                self.get(start)
                    .add_transition(ByteRange::new(fb_start, fb_start), cont3);
            } else {
                // Complex case: create intermediate states
                for first_byte in fb_start..=fb_end {
                    let intermediate1 = self.make()?;
                    self.get(start)
                        .add_transition(ByteRange::new(first_byte, first_byte), intermediate1);

                    for second_byte in sb_start..=sb_end {
                        let intermediate2 = self.make()?;
                        self.get(intermediate1).add_transition(
                            ByteRange::new(second_byte, second_byte),
                            intermediate2,
                        );

                        for third_byte in tb_start..=tb_end {
                            let intermediate3 = self.make()?;
                            self.get(intermediate2).add_transition(
                                ByteRange::new(third_byte, third_byte),
                                intermediate3,
                            );
                            self.get(intermediate3)
                                .add_transition(ByteRange::new(fob_start, fob_end), end);
                        }
                    }
                }
            }
        }

        Ok(Fragment::new(start, [end]))
    }

    /// Collect UTF-8 byte patterns for a Unicode interval without enumerating all codepoints.
    fn collect_utf8_patterns(
        &self,
        interval: crate::codepointset::Interval,
        one_byte: &mut Vec<(u8, u8)>,
        two_byte: &mut Vec<(u8, u8, u8, u8)>,
        three_byte: &mut Vec<(u8, u8, u8, u8, u8, u8)>,
        four_byte: &mut Vec<(u8, u8, u8, u8, u8, u8, u8, u8)>,
    ) {
        let first = interval.first;
        let last = interval.last;

        // Split the interval by UTF-8 encoding boundaries
        let mut current = first;

        while current <= last {
            if current < 0x80 {
                // 1-byte UTF-8
                let range_end = (last.min(0x7F)) as u8;
                one_byte.push((current as u8, range_end));
                current = 0x80;
            } else if current < 0x800 {
                // 2-byte UTF-8
                let range_end = last.min(0x7FF);
                self.add_two_byte_pattern(current, range_end, two_byte);
                current = 0x800;
            } else if current < 0x10000 {
                // 3-byte UTF-8
                let range_end = last.min(0xFFFF);
                self.add_three_byte_pattern(current, range_end, three_byte);
                current = 0x10000;
            } else if current < 0x110000 {
                // 4-byte UTF-8
                let range_end = last.min(0x10FFFF);
                self.add_four_byte_pattern(current, range_end, four_byte);
                break;
            } else {
                break;
            }
        }
    }

    fn add_two_byte_pattern(&self, start: u32, end: u32, patterns: &mut Vec<(u8, u8, u8, u8)>) {
        let start_first = ((start >> 6) & 0x1F) as u8 | 0b1100_0000;
        let start_second = (start & 0x3F) as u8 | 0b1000_0000;
        let end_first = ((end >> 6) & 0x1F) as u8 | 0b1100_0000;
        let end_second = (end & 0x3F) as u8 | 0b1000_0000;

        if start_first == end_first {
            // Same first byte
            patterns.push((start_first, start_first, start_second, end_second));
        } else {
            // Multiple first bytes
            patterns.push((start_first, start_first, start_second, 0xBF));
            if start_first + 1 < end_first {
                patterns.push((start_first + 1, end_first - 1, 0x80, 0xBF));
            }
            patterns.push((end_first, end_first, 0x80, end_second));
        }
    }

    fn add_three_byte_pattern(
        &self,
        start: u32,
        end: u32,
        patterns: &mut Vec<(u8, u8, u8, u8, u8, u8)>,
    ) {
        let start_first = ((start >> 12) & 0x0F) as u8 | 0b1110_0000;
        let start_second = ((start >> 6) & 0x3F) as u8 | 0b1000_0000;
        let start_third = (start & 0x3F) as u8 | 0b1000_0000;
        let end_first = ((end >> 12) & 0x0F) as u8 | 0b1110_0000;
        let end_second = ((end >> 6) & 0x3F) as u8 | 0b1000_0000;
        let end_third = (end & 0x3F) as u8 | 0b1000_0000;

        if start_first == end_first {
            if start_second == end_second {
                // Same first and second byte
                patterns.push((
                    start_first,
                    start_first,
                    start_second,
                    start_second,
                    start_third,
                    end_third,
                ));
            } else {
                // Same first byte, different second bytes
                patterns.push((
                    start_first,
                    start_first,
                    start_second,
                    start_second,
                    start_third,
                    0xBF,
                ));
                if start_second + 1 < end_second {
                    patterns.push((
                        start_first,
                        start_first,
                        start_second + 1,
                        end_second - 1,
                        0x80,
                        0xBF,
                    ));
                }
                patterns.push((
                    start_first,
                    start_first,
                    end_second,
                    end_second,
                    0x80,
                    end_third,
                ));
            }
        } else {
            // Different first bytes - this gets complex, for now use a simpler approach
            patterns.push((start_first, end_first, 0x80, 0xBF, 0x80, 0xBF));
        }
    }

    fn add_four_byte_pattern(
        &self,
        start: u32,
        end: u32,
        patterns: &mut Vec<(u8, u8, u8, u8, u8, u8, u8, u8)>,
    ) {
        let start_first = ((start >> 18) & 0x07) as u8 | 0b1111_0000;
        let end_first = ((end >> 18) & 0x07) as u8 | 0b1111_0000;

        // For simplicity, use full continuation byte ranges for 4-byte patterns
        patterns.push((start_first, end_first, 0x80, 0xBF, 0x80, 0xBF, 0x80, 0xBF));
    }

    fn add_utf8_ranges_for_interval(
        &mut self,
        start: StateHandle,
        end: StateHandle,
        tail1: &mut Option<StateHandle>, // [80-BF] -> end
        tail2: &mut Option<StateHandle>, // [80-BF] -> tail1
        interval: crate::codepointset::Interval,
    ) -> Result<()> {
        let first = interval.first;
        let last = interval.last;

        // UTF-8 encoding ranges:
        // 1-byte: 0x00-0x7F         (0xxxxxxx) -> end
        // 2-byte: 0x80-0x7FF        (110xxxxx 10xxxxxx) -> [C2-DF] -> tail1
        // 3-byte: 0x800-0xFFFF      (1110xxxx 10xxxxxx 10xxxxxx) -> [E0-EF] -> tail2
        // 4-byte: 0x10000-0x10FFFF  (11110xxx 10xxxxxx 10xxxxxx 10xxxxxx) -> [F0-F4] -> tail2 -> tail1

        // 1-byte UTF-8 range: direct to end
        if first <= 0x7F {
            let range_start = first as u8;
            let range_end = (last.min(0x7F)) as u8;
            if range_start <= range_end {
                self.get(start)
                    .add_transition(ByteRange::new(range_start, range_end), end);
            }
        }

        // 2-byte UTF-8 range: valid range is [C2-DF] [80-BF] (excludes overlong [C0-C1])
        if last >= 0x80 && first <= 0x7FF {
            let cp_start = first.max(0x80);
            let cp_end = last.min(0x7FF);
            if cp_start <= cp_end {
                // Calculate UTF-8 first byte range for 2-byte sequences
                let first_byte_start = self.utf8_first_byte_2(cp_start).max(0xC2); // Exclude overlong
                let first_byte_end = self.utf8_first_byte_2(cp_end);
                if first_byte_start <= first_byte_end {
                    // Lazily create tail1 if needed
                    if tail1.is_none() {
                        let t1 = self.make()?;
                        self.get(t1).add_transition(ByteRange::new(0x80, 0xBF), end);
                        *tail1 = Some(t1);
                    }
                    self.get(start).add_transition(
                        ByteRange::new(first_byte_start, first_byte_end),
                        tail1.unwrap(),
                    );
                }
            }
        }

        // 3-byte UTF-8 range: needs careful handling to avoid overlong sequences
        // Valid: [E0] [A0-BF] [80-BF] (for U+0800-U+0FFF)
        //        [E1-EF] [80-BF] [80-BF] (for U+1000-U+FFFF)
        if last >= 0x800 && first <= 0xFFFF {
            let cp_start = first.max(0x800);
            let cp_end = last.min(0xFFFF);
            if cp_start <= cp_end {
                self.add_3byte_utf8_ranges(start, end, tail1, tail2, cp_start, cp_end)?;
            }
        }

        // 4-byte UTF-8 range: needs careful handling to avoid overlong sequences
        // Valid: [F0] [90-BF] [80-BF] [80-BF] (for U+10000-U+3FFFF)
        //        [F1-F3] [80-BF] [80-BF] [80-BF] (for U+40000-U+FFFFF)
        //        [F4] [80-8F] [80-BF] [80-BF] (for U+100000-U+10FFFF)
        if last >= 0x10000 && first <= 0x10FFFF {
            let cp_start = first.max(0x10000);
            let cp_end = last.min(0x10FFFF);
            if cp_start <= cp_end {
                self.add_4byte_utf8_ranges(start, end, tail1, tail2, cp_start, cp_end)?;
            }
        }

        Ok(())
    }

    fn add_3byte_utf8_ranges(
        &mut self,
        start: StateHandle,
        end: StateHandle,
        tail1: &mut Option<StateHandle>,
        tail2: &mut Option<StateHandle>,
        cp_start: u32,
        cp_end: u32,
    ) -> Result<()> {
        // Build a proper UTF-8 trie for 3-byte sequences in the range [cp_start, cp_end]

        // Helper to ensure tail nodes exist
        if tail1.is_none() {
            let t1 = self.make()?;
            self.get(t1).add_transition(ByteRange::new(0x80, 0xBF), end);
            *tail1 = Some(t1);
        }
        if tail2.is_none() {
            let t2 = self.make()?;
            self.get(t2)
                .add_transition(ByteRange::new(0x80, 0xBF), tail1.unwrap());
            *tail2 = Some(t2);
        }

        // Convert the codepoint range to UTF-8 byte sequences and build trie
        let mut start_bytes = [0u8; 4];
        let mut end_bytes = [0u8; 4];

        let start_char = char::from_u32(cp_start).ok_or(Error::NotUTF8)?;
        let end_char = char::from_u32(cp_end).ok_or(Error::NotUTF8)?;

        let start_utf8 = start_char.encode_utf8(&mut start_bytes);
        let end_utf8 = end_char.encode_utf8(&mut end_bytes);

        // Ensure we're dealing with 3-byte sequences
        if start_utf8.len() != 3 || end_utf8.len() != 3 {
            return Err(Error::NotUTF8);
        }

        // If start and end have same first 2 bytes, we can optimize
        if start_bytes[0] == end_bytes[0] && start_bytes[1] == end_bytes[1] {
            // Same prefix - create path for specific bytes with UTF-8 validation
            let b0 = start_bytes[0];
            let b1 = start_bytes[1];

            // Validate UTF-8 overlong sequences
            if b0 == 0xE0 && b1 < 0xA0 {
                return Err(Error::NotUTF8); // Overlong sequence
            }

            let b0_state = self.ensure_edge(start, b0)?;
            let b1_state = self.ensure_edge(b0_state, b1)?;

            // Add range for final byte directly to end
            self.get(b1_state)
                .add_transition(ByteRange::new(start_bytes[2], end_bytes[2]), end);
        } else {
            // Different prefixes - fall back to individual codepoints
            for cp in cp_start..=cp_end {
                let ch = char::from_u32(cp).ok_or(Error::NotUTF8)?;
                let mut bytes = [0u8; 4];
                let utf8_str = ch.encode_utf8(&mut bytes);

                let mut current = start;
                for &byte in utf8_str.as_bytes() {
                    current = self.ensure_edge(current, byte)?;
                }
                self.get(current).add_eps(end);
            }
        }

        Ok(())
    }

    fn add_4byte_utf8_ranges(
        &mut self,
        start: StateHandle,
        end: StateHandle,
        tail1: &mut Option<StateHandle>,
        tail2: &mut Option<StateHandle>,
        cp_start: u32,
        cp_end: u32,
    ) -> Result<()> {
        // Build a proper UTF-8 trie for 4-byte sequences in the range [cp_start, cp_end]
        // This needs to handle the specific byte patterns rather than broad ranges

        // Helper to ensure tail nodes exist
        if tail1.is_none() {
            let t1 = self.make()?;
            self.get(t1).add_transition(ByteRange::new(0x80, 0xBF), end);
            *tail1 = Some(t1);
        }
        if tail2.is_none() {
            let t2 = self.make()?;
            self.get(t2)
                .add_transition(ByteRange::new(0x80, 0xBF), tail1.unwrap());
            *tail2 = Some(t2);
        }

        // Convert the codepoint range to UTF-8 byte sequences and build trie
        // We'll encode the start and end codepoints and build paths for the range
        let mut start_bytes = [0u8; 4];
        let mut end_bytes = [0u8; 4];

        let start_char = char::from_u32(cp_start).ok_or(Error::NotUTF8)?;
        let end_char = char::from_u32(cp_end).ok_or(Error::NotUTF8)?;

        start_char.encode_utf8(&mut start_bytes);
        end_char.encode_utf8(&mut end_bytes);

        // If start and end have same first 3 bytes, we can optimize
        if start_bytes[0] == end_bytes[0]
            && start_bytes[1] == end_bytes[1]
            && start_bytes[2] == end_bytes[2]
        {
            // Same prefix - create path for specific bytes
            let b0_state = self.ensure_edge(start, start_bytes[0])?;
            let b1_state = self.ensure_edge(b0_state, start_bytes[1])?;
            let b2_state = self.ensure_edge(b1_state, start_bytes[2])?;

            // Add range for final byte directly to end (bypassing epsilon)
            self.get(b2_state)
                .add_transition(ByteRange::new(start_bytes[3], end_bytes[3]), end);
        } else {
            // Different prefixes - fall back to individual codepoints
            for cp in cp_start..=cp_end {
                let ch = char::from_u32(cp).ok_or(Error::NotUTF8)?;
                let mut bytes = [0u8; 4];
                let utf8_str = ch.encode_utf8(&mut bytes);

                let mut current = start;
                for &byte in utf8_str.as_bytes() {
                    current = self.ensure_edge(current, byte)?;
                }
                self.get(current).add_eps(end);
            }
        }

        Ok(())
    }

    fn build_universal_utf8_validator(&mut self) -> Result<Fragment> {
        // Create a universal UTF-8 validator that accepts any valid UTF-8 sequence
        // This is more efficient than enumerating all possible codepoints
        let start = self.make()?;
        let end = self.make()?;

        // 1-byte sequences: 0x00-0x7F directly to end
        self.get(start)
            .add_transition(ByteRange::new(0x00, 0x7F), end);

        // 2-byte sequences: [C2-DF] [80-BF] (excludes overlong C0-C1)
        let two_byte_intermediate = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xC2, 0xDF), two_byte_intermediate);
        self.get(two_byte_intermediate)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        // 3-byte sequences:
        // [E0] [A0-BF] [80-BF] (excludes overlong sequences)
        let e0_intermediate = self.make()?;
        let e0_continuation = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xE0, 0xE0), e0_intermediate);
        self.get(e0_intermediate)
            .add_transition(ByteRange::new(0xA0, 0xBF), e0_continuation);
        self.get(e0_continuation)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        // [E1-EF] [80-BF] [80-BF] (normal 3-byte sequences)
        let three_byte_intermediate = self.make()?;
        let three_byte_continuation = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xE1, 0xEF), three_byte_intermediate);
        self.get(three_byte_intermediate)
            .add_transition(ByteRange::new(0x80, 0xBF), three_byte_continuation);
        self.get(three_byte_continuation)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        // 4-byte sequences:
        // [F0] [90-BF] [80-BF] [80-BF] (excludes overlong sequences)
        let f0_intermediate = self.make()?;
        let f0_continuation1 = self.make()?;
        let f0_continuation2 = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xF0, 0xF0), f0_intermediate);
        self.get(f0_intermediate)
            .add_transition(ByteRange::new(0x90, 0xBF), f0_continuation1);
        self.get(f0_continuation1)
            .add_transition(ByteRange::new(0x80, 0xBF), f0_continuation2);
        self.get(f0_continuation2)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        // [F1-F3] [80-BF] [80-BF] [80-BF] (normal 4-byte sequences)
        let four_byte_intermediate = self.make()?;
        let four_byte_continuation1 = self.make()?;
        let four_byte_continuation2 = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xF1, 0xF3), four_byte_intermediate);
        self.get(four_byte_intermediate)
            .add_transition(ByteRange::new(0x80, 0xBF), four_byte_continuation1);
        self.get(four_byte_continuation1)
            .add_transition(ByteRange::new(0x80, 0xBF), four_byte_continuation2);
        self.get(four_byte_continuation2)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        // [F4] [80-8F] [80-BF] [80-BF] (restricted to U+10FFFF max)
        let f4_intermediate = self.make()?;
        let f4_continuation1 = self.make()?;
        let f4_continuation2 = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xF4, 0xF4), f4_intermediate);
        self.get(f4_intermediate)
            .add_transition(ByteRange::new(0x80, 0x8F), f4_continuation1);
        self.get(f4_continuation1)
            .add_transition(ByteRange::new(0x80, 0xBF), f4_continuation2);
        self.get(f4_continuation2)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        Ok(Fragment::new(start, [end]))
    }

    // Helper functions to compute UTF-8 first bytes for different sequence lengths
    fn utf8_first_byte_2(&self, codepoint: u32) -> u8 {
        // 2-byte UTF-8: 110xxxxx
        0xC0 | ((codepoint >> 6) as u8 & 0x1F)
    }

    fn utf8_first_byte_3(&self, codepoint: u32) -> u8 {
        // 3-byte UTF-8: 1110xxxx
        0xE0 | ((codepoint >> 12) as u8 & 0x0F)
    }

    fn utf8_first_byte_4(&self, codepoint: u32) -> u8 {
        // 4-byte UTF-8: 11110xxx
        0xF0 | ((codepoint >> 18) as u8 & 0x07)
    }

    fn build_sequence(&mut self, seq: &[u8]) -> Result<Fragment> {
        let start = self.make()?;
        let mut cursor = start;
        for &b in seq {
            let next = self.make()?;
            self.get(cursor).add_transition(ByteRange::new(b, b), next);
            cursor = next;
        }
        Ok(Fragment::new(start, [cursor]))
    }

    fn build_byte_set(&mut self, bytes: &[u8]) -> Result<Fragment> {
        let start = self.make()?;
        let end = self.make()?;
        let start_state = self.get(start);
        for &b in bytes {
            start_state.add_transition(ByteRange::new(b, b), end);
        }
        Ok(Fragment::new(start, [end]))
    }

    /// Build a Loop node (handles both Loop and Loop1CharBody).
    fn build_loop(&mut self, loopee: &Node, quant: &crate::ir::Quantifier) -> Result<Fragment> {
        let start = self.make()?;
        let greedy = quant.greedy;
        let mut current_ends = smallvec![start];

        // Unroll minimum iterations. Finite automata can't count.
        for _i in 0..quant.min {
            let mut next_ends: SmallVec<[StateHandle; 2]> = smallvec![];
            for current_end in current_ends {
                let body = self.build(loopee)?;
                self.get(current_end).add_eps(body.start);
                next_ends.extend(body.ends);
            }
            current_ends = next_ends;
        }

        match quant.max {
            None => {
                // Unbounded. Ends can continue looping or exit.
                let body = self.build(loopee)?; // loopee
                let exit: u32 = self.make()?; // way out of the loop
                let recur = body.start; // return to the loop

                // Add eps to the loop body and end. Prefer the body if we are greedy,
                // the end if non-greedy.
                self.join_many_eps(
                    &current_ends,
                    if greedy { [recur, exit] } else { [exit, recur] },
                );

                // Add eps from the end of the loop body back to the start,
                // or to exit.
                self.join_many_eps(
                    &body.ends,
                    if greedy { [recur, exit] } else { [exit, recur] },
                );

                Ok(Fragment::new(start, [exit]))
            }
            Some(max) if max != quant.min => {
                debug_assert!(max > quant.min);
                // Bounded loop. Every iteration from min to max can bail out.
                let exit = self.make()?;

                // Add optional iterations from min to max
                for _i in quant.min..max {
                    let body = self.build(loopee)?;
                    let recur = body.start;

                    // Add eps to the loop body and end. Prefer the body if we are greedy,
                    // the end if non-greedy.
                    self.join_many_eps(
                        &current_ends,
                        if greedy { [recur, exit] } else { [exit, recur] },
                    );
                    current_ends = body.ends;
                }

                // Last iteration goes to exit.
                self.join_eps(&current_ends, exit);

                Ok(Fragment::new(start, [exit]))
            }
            Some(_) => {
                // max == min - exact number of iterations
                Ok(Fragment {
                    start,
                    ends: current_ends,
                })
            }
        }
    }

    fn build_capture_group(
        &mut self,
        contents: &Node,
        group_id: CaptureGroupID,
    ) -> Result<Fragment> {
        // The group ID is the capture group index - i.e. a value of 0 means the first capture group (NOT the entire match).
        // Convert it to our register indexes. Here 0 and 1 correspond to the full match.
        let open_reg = (group_id as RegisterIdx + 1) * 2;
        let close_reg = open_reg + 1;

        // Record if we have a new largest number of registers.
        self.num_registers = self.num_registers.max(close_reg as usize + 1);

        let open_group = self.make()?;
        let body = self.build(contents)?;

        self.get(open_group)
            .add_eps_with_write(body.start, open_reg);
        let close_group = self.join_with_write(&body.ends, close_reg)?;
        Ok(Fragment::new(open_group, [close_group]))
    }
}

/// Try converting a regular expression to a NFA.
/// \return the NFA on success, or none if the IR has unsupported instructions,
/// or we would exceed a budget.
impl Nfa {
    pub fn try_from(re: &ir::Regex) -> Result<Self> {
        let mut b: Builder = Builder::new(2048, re.flags.unicode);

        // Create the start, capturing it.
        let Fragment { start, ends } = b.build_start()?;

        // Should have no ends on the pattern since it's top level - everything ends in goal.
        let pattern_fragment = b.build(&re.node)?;
        assert!(pattern_fragment.ends.is_empty());

        // Connect start to pattern.
        b.join_eps(&ends, pattern_fragment.start);

        optimize_states(&mut b.states);

        Ok(Nfa {
            start,
            states: b.states.into_boxed_slice(),
            num_registers: b.num_registers,
        })
    }

    pub(super) fn at(&self, idx: StateHandle) -> &State {
        &self.states[idx as usize]
    }

    pub fn start(&self) -> StateHandle {
        self.start
    }

    pub fn num_registers(&self) -> usize {
        self.num_registers
    }
}
