//! Conversion of IR to non-deterministic finite automata.

use crate::automata::nfa_optimize::optimize_states;
use crate::codepointset::{CodePointSet, Interval};
use crate::ir::{self, Node};
use crate::types::{BracketContents, CaptureGroupID};
use crate::unicode;
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::ToString, vec::Vec};
use core::iter::once;
use smallvec::{SmallVec, smallvec};

#[derive(Debug)]
pub struct Nfa {
    pub(super) start: StateHandle,
    pub(super) states: Box<[State]>,
    pub(super) num_tags: usize,
    /// Capture-group names, indexed by group id (the same order used by
    /// `regress::Match::captures`). Groups without a name carry an empty
    /// string. Empty when the regex has no named groups (matches the
    /// convention used by `CompiledRegex::group_names`).
    pub(super) group_names: Box<[Box<str>]>,
}

// A handle to a State in the NFA.
// This is implemented as an index but is remapped to a dense vector later.
pub type StateHandle = u32;

// A semantic capture-position index ("tag" in the TDFA literature). Each
// capture group has two tags, for open and close. The first pair is reserved
// for the full match, so the first explicit capture group has index 2 for
// open and 3 for close.
pub type TagIdx = u32;

pub const FULL_MATCH_START: TagIdx = 0;
pub const FULL_MATCH_END: TagIdx = 1;

// A captured position in the text.
pub type TextPos = usize;

// Sentinel position meaning no match.
pub const TEXT_POS_NO_MATCH: TextPos = usize::MAX;

// A closed range of bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    pub start: u8,
    pub end: u8,
}

impl core::fmt::Debug for ByteRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.start == self.end {
            write!(f, "{:#04X}", self.start)
        } else {
            write!(f, "{:#04X}..={:#04X}", self.start, self.end)
        }
    }
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

// A piece of an NFA, with a start node handle and a set of "loose ends."
// These loose ends need epsilon transitions to the next start.
pub(super) struct Fragment {
    pub(super) start: StateHandle,
    pub(super) ends: SmallVec<[StateHandle; 2]>,
}

impl Fragment {
    // Construct a Fragment from a start and from loose ends.
    #[inline]
    pub(super) fn new(start: StateHandle, ends: impl IntoIterator<Item = StateHandle>) -> Self {
        Self {
            start,
            ends: ends.into_iter().collect(),
        }
    }
}

/// A predicate gating traversal of an epsilon edge. `Always` is the common
/// case (zero-width writes for capture groups, alternation branches, loop
/// structure, etc.). The `StartOfLine` / `EndOfLine` variants encode `^` and
/// `$` anchors; their `holds` evaluation is in `super::anchors`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum EpsCondition {
    #[default]
    Always,
    StartOfLine {
        multiline: bool,
    },
    EndOfLine {
        multiline: bool,
    },
}

// An epsilon edge. Transitions on empty input (subject to `cond`),
// optionally writing the current input position to one or more tags.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct EpsEdge {
    // Target of the transition.
    pub target: StateHandle,
    // Tags to write on traversal.
    pub ops: SmallVec<[TagIdx; 2]>,
    // Predicate gating traversal; defaults to `Always`.
    pub cond: EpsCondition,
}

impl EpsEdge {
    // Create a new epsilon edge going to a target state, with no tag writes.
    fn to_target(target: StateHandle) -> Self {
        Self {
            target,
            ops: smallvec![],
            cond: EpsCondition::Always,
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
    // Add an epsilon transition to another state, with no tag writes.
    pub fn add_eps(&mut self, target: StateHandle) {
        self.eps.push(EpsEdge {
            target,
            ops: smallvec![],
            cond: EpsCondition::Always,
        });
    }

    // Add an epsilon transition to another state, with one tag write.
    pub fn add_eps_with_write(&mut self, target: StateHandle, tag: TagIdx) {
        self.eps.push(EpsEdge {
            target,
            ops: smallvec![tag],
            cond: EpsCondition::Always,
        });
    }

    // Add a predicated epsilon transition (for `^` / `$`). The predicate is
    // evaluated against the input slice at the current position when the
    // executor reaches this edge during eps closure.
    pub fn add_eps_anchor(&mut self, target: StateHandle, cond: EpsCondition) {
        self.eps.push(EpsEdge {
            target,
            ops: smallvec![],
            cond,
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
    pub(super) num_tags: usize,
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
    pub(super) fn new(state_budget: usize, unicode: bool) -> Self {
        Builder {
            states: vec![State::goal()],
            state_budget,
            num_tags: 2, // FULL_MATCH_START and FULL_MATCH_END
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
            &Node::Char { c, icase } => {
                if icase {
                    self.build_icase_char(c)
                } else {
                    self.build_char(c)
                }
            }
            Node::MatchAny => self.build_match_any(false),
            Node::MatchAnyExceptLineTerminator => self.build_match_any(true),
            Node::ByteSequence(seq) => self.build_sequence(seq),
            Node::ByteSet(bytes) => self.build_byte_set(bytes),
            Node::CharSet(chars) => self.build_char_set(chars),
            Node::Cat(seq) => self.build_cat(seq),
            Node::Alt(left, right) => self.build_alt(left, right),
            Node::CaptureGroup {
                id: group_id,
                contents,
                ..
            } => self.build_capture_group(contents, *group_id),
            Node::BackRef { .. } => Err(Error::UnsupportedInstruction(
                "Backreferences not supported by NFAs".to_string(),
            )),
            Node::Bracket(contents) => self.build_bracket(contents),
            Node::LookaroundAssertion { .. } => Err(Error::UnsupportedInstruction(
                "Lookaround assertions not supported by NFAs".to_string(),
            )),
            Node::Loop1CharBody { loopee, quant } => self.build_loop(loopee, quant),
            Node::Loop { loopee, quant, .. } => self.build_loop(loopee, quant),
            Node::Anchor {
                anchor_type,
                multiline,
            } => self.build_anchor(*anchor_type, *multiline),
            Node::WordBoundary { .. } => Err(Error::UnsupportedInstruction(
                "Word boundaries not supported by NFAs".to_string(),
            )),
        }
    }

    /// Try adding a new state, returning its handle.
    pub(super) fn make(&mut self) -> Result<StateHandle> {
        if self.states.len() < self.state_budget {
            self.states.push(State::default());
            let new_handle = self.states.len() as StateHandle - 1;
            Ok(new_handle)
        } else {
            Err(Error::BudgetExceeded)
        }
    }

    /// Access a state by handle.
    pub(super) fn get(&mut self, idx: StateHandle) -> &mut State {
        &mut self.states[idx as usize]
    }

    /// Add epsilons from a handle to more handles, without tag writes.
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

    /// Add a single tag write on an epsilon transition from a list of handles, producing a new handle.
    /// Callers are responsible for not passing an empty `from`: handle the
    /// unmatchable-branch case before calling (see `build_capture_group`).
    fn join_with_write(&mut self, from: &[StateHandle], tag: TagIdx) -> Result<StateHandle> {
        debug_assert!(!from.is_empty());
        let target = self.make()?;
        // Join the existing states into one first, if we have more than one.
        let joined: StateHandle = if from.len() == 1 {
            from[0]
        } else {
            let joined = self.make()?;
            self.join_eps(from, joined);
            joined
        };
        self.get(joined).add_eps_with_write(target, tag);
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

    /// Build a `^` or `$` anchor as a single predicated epsilon edge.
    /// The predicate is evaluated at runtime against the input slice; see
    /// `super::anchors`.
    fn build_anchor(&mut self, anchor_type: ir::AnchorType, multiline: bool) -> Result<Fragment> {
        let start = self.make()?;
        let end = self.make()?;
        let cond = match anchor_type {
            ir::AnchorType::StartOfLine => EpsCondition::StartOfLine { multiline },
            ir::AnchorType::EndOfLine => EpsCondition::EndOfLine { multiline },
        };
        self.get(start).add_eps_anchor(end, cond);
        Ok(Fragment::new(start, [end]))
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

    fn build_match_any(&mut self, except_line_terminator: bool) -> Result<Fragment> {
        let mut cps = CodePointSet::all_unicode();
        if except_line_terminator {
            // ES9 11.3 - see matchers::is_line_terminator().
            cps.remove(&[
                Interval::new(0x000A, 0x000A),
                Interval::new(0x000D, 0x000D),
                Interval::new(0x2028, 0x2029),
            ]);
        };
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
        // Thompson/subset-construction engines produce wrong captures for
        // loops whose body can match empty (rust-lang/regex#779). Rather than
        // rewrite the IR, reject the pattern here — the NFA backend and any
        // future TDFA built from it are then safe by construction.
        if loopee.can_match_empty() {
            return Err(Error::UnsupportedInstruction(
                "Loop over empty-matching body not supported by NFAs".to_string(),
            ));
        }
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
        // The group ID is the capture group index — value 0 is the first capture
        // group, NOT the entire match. Tags 0 and 1 are reserved for the full
        // match, so group N occupies tags (N+1)*2 (open) and +1 (close).
        let open_tag = (group_id as TagIdx + 1) * 2;
        let close_tag = open_tag + 1;

        // Record if we have a new largest number of tags.
        self.num_tags = self.num_tags.max(close_tag as usize + 1);

        let open_group = self.make()?;
        let body = self.build(contents)?;

        self.get(open_group)
            .add_eps_with_write(body.start, open_tag);
        if body.ends.is_empty() {
            // Body is unmatchable (e.g. an empty character class). The
            // capture group has no closing point; propagate empty ends so
            // any surrounding construct sees this branch as dead.
            return Ok(Fragment::new(open_group, []));
        }
        let close_group = self.join_with_write(&body.ends, close_tag)?;
        Ok(Fragment::new(open_group, [close_group]))
    }
}

/// Collect capture-group names from the IR in id order. Mirrors what
/// `emit::emit` does when populating `CompiledRegex::group_names`: visit
/// every `CaptureGroup` node and push its (possibly-empty) name. Returns an
/// empty slice if no group is named — same convention as `CompiledRegex`.
fn collect_group_names(node: &Node) -> Box<[Box<str>]> {
    let mut names: Vec<Box<str>> = Vec::new();
    ir::walk(/* postorder */ false, /* unicode */ false, node, &mut |n, _| {
        if let Node::CaptureGroup { id, name, .. } = n {
            let idx = *id as usize;
            if names.len() <= idx {
                names.resize(idx + 1, Box::<str>::from(""));
            }
            names[idx] = name.as_deref().unwrap_or("").into();
        }
    });
    if names.iter().any(|s| !s.is_empty()) {
        names.into_boxed_slice()
    } else {
        Box::new([])
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
            num_tags: b.num_tags,
            group_names: collect_group_names(&re.node),
        })
    }

    pub(super) fn at(&self, idx: StateHandle) -> &State {
        &self.states[idx as usize]
    }

    pub fn start(&self) -> StateHandle {
        self.start
    }

    pub fn num_tags(&self) -> usize {
        self.num_tags
    }

    /// Capture-group names indexed by capture-group id (groups without a
    /// name have an empty string). Empty when no groups are named, matching
    /// the convention used by `CompiledRegex`.
    pub fn group_names(&self) -> &[Box<str>] {
        &self.group_names
    }
}
