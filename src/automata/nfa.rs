//! Conversion of IR to non-deterministic finite automata.

use crate::ir::{self, Node};
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::ToString, vec::Vec};
use core::{fmt, iter::once};
use smallvec::{smallvec, SmallVec};

#[derive(Debug)]
pub struct Nfa {
    start: StateHandle,
    states: Box<[State]>,
}

// A handle to a State in the NFA.
// This is implemented as an index but is remapped to a dense vector later.
pub type StateHandle = u32;

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

#[derive(Debug, Default)]
pub struct State {
    // Epsilon transitions to other states, in priority order.
    pub eps: Vec<StateHandle>,

    // Transitions to other states, indexed by sorted byte ranges.
    pub transitions: Vec<(ByteRange, StateHandle)>,
}

impl State {
    // Add an epsilon transition to another state.
    pub fn add_eps(&mut self, target: StateHandle) {
        self.eps.push(target);
    }

    // Add a byte transition to another state.
    pub fn add_transition(&mut self, range: ByteRange, dest: StateHandle) {
        assert!(range.start <= range.end);

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

    fn build(&mut self, node: &Node) -> Result<Fragment> {
        match node {
            Node::ByteSequence(seq) => self.build_sequence(seq),
            Node::Cat(seq) => self.build_cat(seq),
            Node::Alt(left, right) => self.build_alt(left, right),
            Node::Goal => Ok(Fragment::new(GOAL_STATE, [])),
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

    /// Build an alternation of nodes.
    fn build_alt(&mut self, left: &Node, right: &Node) -> Result<Fragment> {
        let start = self.make()?;
        let left = self.build(left)?;
        let right = self.build(right)?;
        // Note: left has priority.
        self.get(start).add_eps(left.start);
        self.get(start).add_eps(right.start);

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
            for end in ends {
                self.get(end).add_eps(next.start);
            }
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
                let body = self.build(loopee)?;
                let end = self.make()?;
                for current_end in current_ends {
                    if quant.greedy {
                        // Greedy: prefer to continue looping
                        self.get(current_end).add_eps(body.start);
                        self.get(current_end).add_eps(end);

                        // Body ends loop back to themselves or exit
                        for &body_end in &body.ends {
                            self.get(body_end).add_eps(body.start);
                            self.get(body_end).add_eps(end);
                        }
                    } else {
                        // Non-greedy: prefer to exit
                        self.get(current_end).add_eps(end);
                        self.get(current_end).add_eps(body.start);

                        // Body ends exit or loop back to themselves
                        for &body_end in &body.ends {
                            self.get(body_end).add_eps(end);
                            self.get(body_end).add_eps(body.start);
                        }
                    }
                }

                Ok(Fragment::new(start, [end]))
            }
            Some(max) if max > quant.min => {
                // Bounded loop with optional iterations from min to max
                let end = self.make()?;

                // All current ends can exit to end
                for current_end in current_ends.clone() {
                    self.get(current_end).add_eps(end);
                }

                // Add optional iterations from min to max
                for _i in quant.min..max {
                    let mut next_ends: SmallVec<[StateHandle; 2]> = smallvec![];

                    for current_end in current_ends {
                        let body_fragment = self.build(loopee)?;

                        if quant.greedy {
                            // Greedy: prefer to continue
                            self.get(current_end).add_eps(body_fragment.start);
                            self.get(current_end).add_eps(end);
                        } else {
                            // Non-greedy: prefer to exit
                            self.get(current_end).add_eps(end);
                            self.get(current_end).add_eps(body_fragment.start);
                        }

                        next_ends.extend(body_fragment.ends);
                    }

                    // These new ends can also exit
                    for new_end in next_ends.clone() {
                        self.get(new_end).add_eps(end);
                    }

                    current_ends = next_ends;
                }

                Ok(Fragment::new(start, [end]))
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
        let Fragment { start, ends } = b.build(&re.node)?;
        assert!(ends.is_empty(), "Should not have dangling ends");
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
                for &target in &state.eps {
                    let dest = match target {
                        idx if idx == self.start() => "START".to_string(),
                        GOAL_STATE => "GOAL".to_string(),
                        _ => target.to_string(),
                    };
                    result.push_str(&format!("    ε ──> {}\n", dest));
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
            for &target in &state.eps {
                write!(f, " ε→{}", target)?;
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
