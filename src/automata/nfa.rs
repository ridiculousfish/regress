//! Conversion of IR to non-deterministic finite automata.

use crate::automata::nfa_optimize::optimize_states;
use crate::automata::utf8::utf8_paths_from_code_point_set;
use crate::automata::util::node_description;
use crate::codepointset::CodePointSet;
use crate::ir::{self, Node};
use crate::types::{BracketContents, CaptureGroupID};
use crate::unicode;
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::ToString, vec::Vec};
use core::iter::once;
use smallvec::{SmallVec, smallvec};
use std::collections::HashMap;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ByteRange {
    pub start: u8,
    pub end: u8,
}

impl ByteRange {
    #[inline]
    pub const fn new(start: u8, end: u8) -> Self {
        assert!(start <= end);
        Self { start, end }
    }

    #[allow(unused)]
    #[inline]
    pub const fn contains(self, b: u8) -> bool {
        self.start <= b && b <= self.end
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
            Node::CaptureGroup(contents, group_id) => self.build_capture_group(contents, *group_id),
            Node::NamedCaptureGroup(contents, group_id, _name) => {
                self.build_capture_group(contents, *group_id)
            }
            Node::BackRef(_) => Err(Error::UnsupportedInstruction(
                "Backreferences not supported by NFAs".to_string(),
            )),
            Node::Bracket(contents) => self.build_bracket(contents),
            Node::LookaroundAssertion { .. } => Err(Error::UnsupportedInstruction(
                "Lookaround assertions not supported by NFAs".to_string(),
            )),
            Node::Loop1CharBody { loopee, quant } => self.build_loop(loopee, quant),
            Node::Loop { loopee, quant, .. } => self.build_loop(loopee, quant),

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
        // Note in practice char sets are quite small, as enforced in the IR,
        // so this loop is fine.
        let mut cps = CodePointSet::new();
        for &c in cs {
            cps.add_one(c);
        }
        self.build_from_code_point_set(&cps)
    }

    fn build_bracket(&mut self, contents: &BracketContents) -> Result<Fragment> {
        // Invert if necessary
        let inverted_cps;
        let cps = if contents.invert {
            inverted_cps = contents.cps.inverted();
            &inverted_cps
        } else {
            &contents.cps
        };
        self.build_from_code_point_set(cps)
    }

    fn build_from_code_point_set(&mut self, cps: &CodePointSet) -> Result<Fragment> {
        if cps.is_empty() {
            // Can't match anything - a state with no exits.
            let fail = self.make()?;
            return Ok(Fragment::new(fail, []));
        }
        if cps.contains_all_codepoints() {
            return self.build_universal_utf8_validator();
        }

        // Get a list of paths of byte ranges. These all join into a final state.
        // Join them via suffixes. Maximum length of any path is 4 (max bytes in a UTF-8 char).
        let trie_start = self.make()?;
        let trie_end = self.make()?;
        let paths = utf8_paths_from_code_point_set(cps);
        let mut suffix_to_state: HashMap<&[ByteRange], u32> = HashMap::new();
        // Put the end state as the empty suffix.
        suffix_to_state.insert(&[], trie_end);

        for path in &paths {
            let suffix = path.as_ref();
            // Find the longest cached suffix.
            // This terminates because suffix_to_state contains the empty slice.
            let mut k = 0;
            let target = loop {
                if let Some(&state) = suffix_to_state.get(&suffix[k..]) {
                    break state;
                }
                k += 1;
            };

            // Build missing prefix states.
            let mut cursor = target;
            for idx in (0..k).rev() {
                let state = if idx == 0 { trie_start } else { self.make()? };
                self.get(state).add_transition(suffix[idx], cursor);
                cursor = state;
            }
        }
        Ok(Fragment::new(trie_start, [trie_end]))
    }

    fn build_universal_utf8_validator(&mut self) -> Result<Fragment> {
        // Consume a single UTF-8 char.
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

        // 3-byte sequences: E0 [A0–BF] [80–BF] (avoid overlongs)
        let e0_b2 = self.make()?;
        let e0_b3 = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xE0, 0xE0), e0_b2);
        self.get(e0_b2)
            .add_transition(ByteRange::new(0xA0, 0xBF), e0_b3);
        self.get(e0_b3)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        // 3-byte sequences: [E1–EC] and [EE–EF:] [80–BF] [80–BF]  (normal 3-byte)
        let e_gen_b2 = self.make()?;
        let e_gen_b3 = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xE1, 0xEC), e_gen_b2);
        self.get(start)
            .add_transition(ByteRange::new(0xEE, 0xEF), e_gen_b2);
        self.get(e_gen_b2)
            .add_transition(ByteRange::new(0x80, 0xBF), e_gen_b3);
        self.get(e_gen_b3)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        // 3-byte sequences: ED: [80–9F] [80–BF]  (exclude surrogates)
        let ed_b2 = self.make()?;
        let ed_b3 = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xED, 0xED), ed_b2);
        self.get(ed_b2)
            .add_transition(ByteRange::new(0x80, 0x9F), ed_b3);
        self.get(ed_b3)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        // 4-byte sequences: F0 [90–BF] [80–BF] [80–BF]  (avoid overlong encodings below U+10000)
        let f0_b2 = self.make()?;
        let f0_b3 = self.make()?;
        let f0_b4 = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xF0, 0xF0), f0_b2);
        self.get(f0_b2)
            .add_transition(ByteRange::new(0x90, 0xBF), f0_b3);
        self.get(f0_b3)
            .add_transition(ByteRange::new(0x80, 0xBF), f0_b4);
        self.get(f0_b4)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        // F1–F3 80–BF 80–BF 80–BF  (normal 4-byte)
        let fgen_b2 = self.make()?;
        let fgen_b3 = self.make()?;
        let fgen_b4 = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xF1, 0xF3), fgen_b2);
        self.get(fgen_b2)
            .add_transition(ByteRange::new(0x80, 0xBF), fgen_b3);
        self.get(fgen_b3)
            .add_transition(ByteRange::new(0x80, 0xBF), fgen_b4);
        self.get(fgen_b4)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        // F4 80–8F 80–BF 80–BF  (limit to U+10FFFF; disallow F5–FF entirely)
        let f4_b2 = self.make()?;
        let f4_b3 = self.make()?;
        let f4_b4 = self.make()?;
        self.get(start)
            .add_transition(ByteRange::new(0xF4, 0xF4), f4_b2);
        self.get(f4_b2)
            .add_transition(ByteRange::new(0x80, 0x8F), f4_b3);
        self.get(f4_b3)
            .add_transition(ByteRange::new(0x80, 0xBF), f4_b4);
        self.get(f4_b4)
            .add_transition(ByteRange::new(0x80, 0xBF), end);

        Ok(Fragment::new(start, [end]))
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
