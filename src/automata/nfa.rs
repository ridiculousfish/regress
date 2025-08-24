//! Conversion of IR to non-deterministic finite automata.

use crate::ir::{self, Node};
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::ToString, vec::Vec};
use core::{fmt, iter::once};
use smallvec::{SmallVec, smallvec};

#[derive(Debug)]
pub struct Nfa {
    start: StateHandle,
    states: Box<[State]>,
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

// A closed range of bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
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
#[derive(Debug, Default)]
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
pub struct State {
    // Tagged epsilon transitions to other states, in priority order.
    pub eps: Vec<EpsEdge>,

    // Transitions to other states, indexed by sorted byte ranges.
    pub transitions: Vec<(ByteRange, StateHandle)>,
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

    // Dead states and goal states are effectively the same.
    fn dead() -> Self {
        Self::default()
    }

    fn goal() -> Self {
        Self::default()
    }
}

pub const DEAD_STATE: StateHandle = 0;
pub const GOAL_STATE: StateHandle = 1;

/// Format a byte range in a human-readable way
fn format_byte_range(range: ByteRange) -> String {
    if range.start == range.end {
        // Single byte
        format_byte(range.start)
    } else if range.end == range.start + 1 {
        // Two consecutive bytes
        format!("{}, {}", format_byte(range.start), format_byte(range.end))
    } else {
        // Range of bytes
        format!("{}-{}", format_byte(range.start), format_byte(range.end))
    }
}

/// Format a single byte in a readable way
fn format_byte(byte: u8) -> String {
    match byte {
        b' ' => "'\\s'".to_string(),
        b'\t' => "'\\t'".to_string(),
        b'\n' => "'\\n'".to_string(),
        b'\r' => "'\\r'".to_string(),
        b'\\' => "'\\\\'".to_string(),
        b'\'' => "'\\''".to_string(),
        c if c.is_ascii_graphic() => format!("'{}'", byte as char),
        _ => format!("0x{:02X}", byte),
    }
}

/// Get a descriptive name for a Node variant
fn node_description(node: &Node) -> String {
    fn val_or_inf(v: Option<usize>) -> String {
        match v {
            Some(val) => val.to_string(),
            None => "∞".to_string(),
        }
    }

    match node {
        Node::Empty => "Empty".to_string(),
        Node::Goal => "Goal".to_string(),
        Node::Char { c, icase } => format!(
            "Char({}{})",
            if let Some(ch) = char::from_u32(*c) {
                format!("'{}'", ch)
            } else {
                format!("U+{:04X}", c)
            },
            if *icase { " case-insensitive" } else { "" }
        ),
        Node::ByteSequence(bytes) => format!("ByteSequence({:?})", bytes),
        Node::ByteSet(bytes) => format!("ByteSet({} bytes)", bytes.len()),
        Node::CharSet(chars) => format!("CharSet({} chars)", chars.len()),
        Node::Cat(nodes) => format!("Cat({} nodes)", nodes.len()),
        Node::Alt(_, _) => "Alt".to_string(),
        Node::MatchAny => "MatchAny".to_string(),
        Node::MatchAnyExceptLineTerminator => "MatchAnyExceptLineTerminator".to_string(),
        Node::Anchor(anchor) => format!("Anchor({:?})", anchor),
        Node::WordBoundary { invert } => format!("WordBoundary(invert={})", invert),
        Node::CaptureGroup(_, id) => format!("CaptureGroup({})", id),
        Node::NamedCaptureGroup(_, id, name) => format!("NamedCaptureGroup({}, {:?})", id, name),
        Node::BackRef(n) => format!("BackRef({})", n),
        Node::Bracket(_) => "Bracket".to_string(),
        Node::LookaroundAssertion {
            negate, backwards, ..
        } => format!(
            "LookaroundAssertion({}{})",
            if *negate { "negative " } else { "" },
            if *backwards {
                "lookbehind"
            } else {
                "lookahead"
            }
        ),
        Node::Loop { quant, .. } => {
            format!("Loop(min={}, max={})", quant.min, val_or_inf(quant.max))
        }
        Node::Loop1CharBody { quant, .. } => {
            format!(
                "Loop1CharBody(min={}, max={})",
                quant.min,
                val_or_inf(quant.max)
            )
        }
    }
}

struct Builder {
    // States indexed by handle.
    states: Vec<State>,
    state_budget: usize,
}

#[derive(Debug)]
pub enum Error {
    UnsupportedInstruction(String),
    BudgetExceeded,
}
pub type Result<T> = core::result::Result<T, Error>;

impl Builder {
    fn new(state_budget: usize) -> Self {
        Builder {
            states: vec![State::dead(), State::goal()],
            state_budget,
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
            Node::ByteSequence(seq) => self.build_sequence(seq),
            Node::Cat(seq) => self.build_cat(seq),
            Node::Alt(left, right) => self.build_alt(left, right),
            Node::Goal => self.build_goal(),
            Node::Loop1CharBody { loopee, quant } => self.build_loop(loopee, quant),
            Node::Loop {
                loopee,
                quant,
                enclosed_groups,
            } => {
                if !enclosed_groups.is_empty() {
                    return Err(Error::UnsupportedInstruction(format!(
                        "Loop with enclosed groups: {:?}",
                        enclosed_groups
                    )));
                }
                self.build_loop(loopee, quant)
            }

            // All other node types are unsupported
            unsupported => Err(Error::UnsupportedInstruction(node_description(unsupported))),
        }
    }

    /// Try adding a new state, returning its handle.
    fn make(&mut self) -> Result<StateHandle> {
        if self.states.len() < self.state_budget {
            self.states.push(State::default());
            Ok(self.states.len() as StateHandle - 1)
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

    /// Given a start node, emit a sequence of transitions for a byte sequence.
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

    /// Build a Loop node (handles both Loop and Loop1CharBody).
    fn build_loop(&mut self, loopee: &Node, quant: &crate::ir::Quantifier) -> Result<Fragment> {
        let start = self.make()?;
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
                let greedy = quant.greedy;

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
                    let mut next_ends = smallvec![];

                    for current_end in current_ends {
                        let body = self.build(loopee)?;
                        if quant.greedy {
                            // Greedy: prefer to continue
                            self.get(current_end).add_eps(body.start);
                            self.get(current_end).add_eps(exit);
                        } else {
                            // Non-greedy: prefer to exit
                            self.get(current_end).add_eps(exit);
                            self.get(current_end).add_eps(body.start);
                        }

                        next_ends.extend(body.ends);
                    }

                    // These new ends can also exit
                    for new_end in next_ends.clone() {
                        self.get(new_end).add_eps(exit);
                    }

                    current_ends = next_ends;
                }

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
}

/// Try converting a regular expression to a NFA.
/// \return the NFA on success, or none if the IR has unsupported instructions,
/// or we would exceed a budget.
impl Nfa {
    pub fn try_from(re: &ir::Regex) -> Result<Self> {
        let mut b: Builder = Builder::new(2048);

        // Create the start, capturing it.
        let Fragment { start, ends } = b.build_start()?;

        // Should have no ends on the pattern since it's top level - everything ends in goal or fail.
        let pattern_fragment = b.build(&re.node)?;
        assert!(pattern_fragment.ends.is_empty());

        // Connect start to pattern.
        b.join_eps(&ends, pattern_fragment.start);

        Ok(Nfa {
            start,
            states: b.states.into_boxed_slice(),
        })
    }

    pub fn at(&self, idx: StateHandle) -> &State {
        &self.states[idx as usize]
    }

    pub fn start(&self) -> StateHandle {
        self.start
    }

    /// Generate a human-readable representation of the NFA
    pub fn to_readable_string(&self) -> String {
        let mut result = String::new();
        result.push_str("NFA States:\n");
        result.push_str("===========\n\n");

        for (idx, state) in self.states.iter().enumerate() {
            let state_idx = idx as StateHandle;

            // Add special state markers
            let marker = match state_idx {
                DEAD_STATE => " (DEAD)",
                GOAL_STATE => " (GOAL)",
                idx if idx == self.start() => " (START)",
                _ => "",
            };

            result.push_str(&format!("State {}{}\n", state_idx, marker));

            // Show epsilon transitions
            if !state.eps.is_empty() {
                result.push_str("  ε-transitions:\n");
                for edge in &state.eps {
                    let dest = match edge.target {
                        idx if idx == self.start() => "START".to_string(),
                        GOAL_STATE => "GOAL".to_string(),
                        target => target.to_string(),
                    };
                    
                    if edge.ops.is_empty() {
                        result.push_str(&format!("    ε ──> {}\n", dest));
                    } else {
                        let ops_str = edge.ops.iter()
                            .map(|&reg| format!("r{}", reg))
                            .collect::<Vec<_>>()
                            .join(",");
                        result.push_str(&format!("    ε [{}] ──> {}\n", ops_str, dest));
                    }
                }
            }

            // Show byte transitions
            if !state.transitions.is_empty() {
                result.push_str("  Byte transitions:\n");
                for &(range, target) in &state.transitions {
                    let range_str = format_byte_range(range);
                    result.push_str(&format!("    {} ──> {}\n", range_str, target));
                }
            }

            // Empty state indicator
            if state.eps.is_empty()
                && state.transitions.is_empty()
                && !matches!(state_idx, GOAL_STATE | DEAD_STATE)
            {
                result.push_str("  (no transitions)\n");
            }

            result.push('\n');
        }

        result
    }
}

impl fmt::Display for ByteRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format_byte_range(*self))
    }
}

impl fmt::Display for Nfa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "NFA({} states)", self.states.len())?;
        for (idx, state) in self.states.iter().enumerate() {
            let handle = idx as StateHandle;
            let marker = match handle {
                DEAD_STATE => "D",
                GOAL_STATE => "G",
                idx if idx == self.start() => "S",
                _ => " ",
            };

            write!(f, "[{}{}]", marker, handle)?;

            // Epsilon transitions
            for edge in &state.eps {
                write!(f, " ε→{}", edge.target)?;
            }

            // Byte transitions (concise)
            for &(range, target) in &state.transitions {
                write!(f, " {}→{}", range, target)?;
            }

            if idx < self.states.len() - 1 {
                writeln!(f)?;
            }
        }
        Ok(())
    }
}
