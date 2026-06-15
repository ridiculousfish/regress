//! Conversion of IR to non-deterministic finite automata.

use crate::automata::nfa_optimize::optimize_states;
use crate::codepointset::{CodePointSet, Interval};
use crate::ir::{self, Node};
use crate::types::{BracketContents, CaptureGroupID};
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::ToString, vec::Vec};
use core::iter::once;
use smallvec::{SmallVec, smallvec};

#[derive(Debug)]
pub struct Nfa {
    pub(super) start: StateHandle,
    pub(super) states: Box<[State]>,
    pub(super) num_tags: usize,
    /// Number of tags reserved for user-visible captures. Tags `0..num_capture_tags`
    /// participate in `tags_to_captures`; tags `num_capture_tags..num_tags`
    /// are sentinel tags used by `ProgressSince` predicates.
    pub(super) num_capture_tags: usize,
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
    /// Word boundary `\b` (`invert == false`) or `\B` (`invert == true`).
    /// True iff "previous codepoint is a word char" XOR "next codepoint is
    /// a word char" — possibly inverted. `unicode_icase` widens the
    /// word-char set with U+017F and U+212A (the only non-ASCII codepoints
    /// that fold to ASCII word chars). Implementation in `super::anchors`.
    WordBoundary {
        invert: bool,
        unicode_icase: bool,
    },
    /// Iteration progressed: `thread.tags[idx] == NoMatch || thread.tags[idx] < current_pos`.
    /// Used to gate iteration-exit and back-edge eps on loops whose body
    /// can match empty — implements ES2015 RepeatMatcher's "if min == 0
    /// and y.endIndex == x.endIndex, return failure" rule by suppressing
    /// the eps when the iteration didn't advance position. The sentinel
    /// tag is written at iteration entry via a CurrentPos `TagOp` and
    /// is internal to the NFA (excluded from user-visible captures).
    /// TDFA can't evaluate this predicate statically; construction bails.
    ProgressSince(TagIdx),
}

/// What value an eps-edge traversal writes into a tag slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OpKind {
    /// Write the current input position.
    #[default]
    CurrentPos,
    /// Reset to "unset": `TEXT_POS_NO_MATCH` in the NFA executor's thread
    /// tags, `None` in the TDFA's tag_map. Used at loop-iteration entries
    /// to clear inner-loop capture groups per ES2015 RepeatMatcher
    /// semantics.
    Nil,
}

/// A single tag-write operation attached to an eps edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TagOp {
    pub tag: TagIdx,
    pub kind: OpKind,
}

impl TagOp {
    #[inline]
    pub const fn current_pos(tag: TagIdx) -> Self {
        Self {
            tag,
            kind: OpKind::CurrentPos,
        }
    }
    #[inline]
    pub const fn nil(tag: TagIdx) -> Self {
        Self {
            tag,
            kind: OpKind::Nil,
        }
    }
}

// An epsilon edge. Transitions on empty input (subject to `cond`),
// optionally writing tag values on traversal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct EpsEdge {
    // Target of the transition.
    pub target: StateHandle,
    // Tag writes performed on traversal, in source order.
    pub ops: SmallVec<[TagOp; 2]>,
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

    // Add an epsilon transition to another state, with one CurrentPos
    // tag write.
    pub fn add_eps_with_write(&mut self, target: StateHandle, tag: TagIdx) {
        self.eps.push(EpsEdge {
            target,
            ops: smallvec![TagOp::current_pos(tag)],
            cond: EpsCondition::Always,
        });
    }

    // Add an epsilon transition with a list of tag-write ops, in source
    // (priority) order. Used by `build_loop` to attach Nil resets to
    // iteration-entry eps edges.
    pub fn add_eps_with_writes(
        &mut self,
        target: StateHandle,
        ops: impl IntoIterator<Item = TagOp>,
    ) {
        self.eps.push(EpsEdge {
            target,
            ops: ops.into_iter().collect(),
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
    /// Capture tags occupy indices `0..num_capture_tags`; sentinel tags
    /// (used for `ProgressSince` predicates on nullable-body loops) live
    /// at `num_capture_tags..num_tags`. The user-visible captures live
    /// only in the capture range.
    pub(super) num_capture_tags: usize,
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
    pub(super) fn new(state_budget: usize, unicode: bool, num_capture_tags: usize) -> Self {
        Builder {
            states: vec![State::goal()],
            state_budget,
            num_tags: num_capture_tags,
            num_capture_tags,
            unicode,
        }
    }

    /// Allocate a fresh sentinel tag for use in a `ProgressSince` predicate.
    /// Returns the tag index, which sits above the capture-tag range so
    /// `tags_to_captures` ignores it.
    pub(super) fn make_sentinel(&mut self) -> TagIdx {
        let idx = self.num_tags as TagIdx;
        self.num_tags += 1;
        idx
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
            Node::Char { c } => self.build_char(*c),
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
            Node::StringSet {
                alternatives,
                icase,
            } => self.build_string_set(alternatives, *icase),
            Node::LookaroundAssertion { .. } => Err(Error::UnsupportedInstruction(
                "Lookaround assertions not supported by NFAs".to_string(),
            )),
            Node::Loop1CharBody { loopee, quant } => self.build_loop(loopee, quant),
            Node::Loop { loopee, quant, .. } => self.build_loop(loopee, quant),
            Node::Anchor {
                anchor_type,
                multiline,
            } => self.build_anchor(*anchor_type, *multiline),
            &Node::WordBoundary {
                invert,
                unicode_icase,
            } => self.build_word_boundary(invert, unicode_icase),
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
    #[allow(dead_code)]
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

    /// Build a `\b` or `\B` word-boundary as a single predicated eps edge.
    /// Predicate evaluated against the input slice at runtime — same shape
    /// as `build_anchor`.
    fn build_word_boundary(&mut self, invert: bool, unicode_icase: bool) -> Result<Fragment> {
        let start = self.make()?;
        let end = self.make()?;
        self.get(start).add_eps_anchor(
            end,
            EpsCondition::WordBoundary {
                invert,
                unicode_icase,
            },
        );
        Ok(Fragment::new(start, [end]))
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

    fn build_char(&mut self, c: u32) -> Result<Fragment> {
        // Convert u32 to char
        let ch = match char::from_u32(c) {
            Some(ch) => ch,
            // A surrogate (or otherwise non-scalar) code point cannot occur in
            // UTF-8 input, so it never matches — a state with no exits.
            None => {
                let fail = self.make()?;
                return Ok(Fragment::new(fail, []));
            }
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
    ///
    /// Dispatches on whether the body can match empty. Non-nullable bodies
    /// take the straightforward construction. Nullable bodies need to
    /// implement ES2015 RepeatMatcher's "if min == 0 and y.endIndex ==
    /// x.endIndex, return failure" rule — encoded as `ProgressSince(sentinel)`
    /// predicates on iteration-exit and back-edge eps. See `build_loop_nullable`.
    fn build_loop(&mut self, loopee: &Node, quant: &crate::ir::Quantifier) -> Result<Fragment> {
        if loopee.can_match_empty() {
            self.build_loop_nullable(loopee, quant)
        } else {
            self.build_loop_inner(loopee, quant)
        }
    }

    /// Build a loop whose body can match empty. Implements ES2015's
    /// "reject empty iteration once min satisfied" rule:
    ///
    /// - First `quant.min` iterations are unrolled and ungated (d's
    ///   `min` is > 0 for these, so the spec doesn't reject empty here).
    /// - Iterations `min..max` (or unbounded) form a "gated section":
    ///   the iteration-entry eps writes `current_pos` to a sentinel tag;
    ///   iteration-exit and back-edge eps are gated on
    ///   `ProgressSince(sentinel)` so a thread whose body matched empty
    ///   can't exit the gated section. That thread dies, the leftmost-
    ///   first ordering then yields to a thread that took fewer
    ///   iterations (or skipped entry altogether).
    fn build_loop_nullable(
        &mut self,
        loopee: &Node,
        quant: &crate::ir::Quantifier,
    ) -> Result<Fragment> {
        let reset_ops = collect_loop_reset_ops(loopee);
        let start = self.make()?;
        let greedy = quant.greedy;
        let mut current_ends: SmallVec<[StateHandle; 2]> = smallvec![start];

        // Unroll minimum iterations. Ungated — d's min > 0 here.
        for _ in 0..quant.min {
            let mut next_ends: SmallVec<[StateHandle; 2]> = smallvec![];
            for current_end in current_ends {
                let body = self.build(loopee)?;
                self.get(current_end)
                    .add_eps_with_writes(body.start, reset_ops.iter().copied());
                next_ends.extend(body.ends);
            }
            current_ends = next_ends;
        }

        match quant.max {
            Some(max) if max == quant.min => {
                // Exact count — no optional iterations.
                Ok(Fragment {
                    start,
                    ends: current_ends,
                })
            }
            None => {
                // Unbounded gated section. Build one body; back-edge to itself.
                let sentinel = self.make_sentinel();
                let body = self.build(loopee)?;
                let exit = self.make()?;
                // First gated entry from current_ends → body.start (ungated, writes sentinel) | exit (ungated skip).
                add_gated_entry_eps(
                    self,
                    &current_ends,
                    body.start,
                    exit,
                    greedy,
                    &reset_ops,
                    sentinel,
                    /* gated = */ false,
                );
                // Back-edge from body.ends → body.start (gated, re-writes sentinel) | exit (gated).
                add_gated_entry_eps(
                    self, &body.ends, body.start, exit, greedy, &reset_ops, sentinel,
                    /* gated = */ true,
                );
                Ok(Fragment::new(start, [exit]))
            }
            Some(max) => {
                debug_assert!(max > quant.min);
                // Bounded gated section. Iters quant.min..max each get their
                // own body. The first gated entry (from the post-unroll ends,
                // or from `start` if min == 0) is ungated; subsequent entries
                // and the final exit are gated.
                let sentinel = self.make_sentinel();
                let exit = self.make()?;
                for i in quant.min..max {
                    let body = self.build(loopee)?;
                    let gated = i != quant.min;
                    add_gated_entry_eps(
                        self,
                        &current_ends,
                        body.start,
                        exit,
                        greedy,
                        &reset_ops,
                        sentinel,
                        gated,
                    );
                    current_ends = body.ends;
                }
                // Final exit from the last body's ends — gated.
                for &src in &current_ends {
                    self.get(src)
                        .add_eps_anchor(exit, EpsCondition::ProgressSince(sentinel));
                }
                Ok(Fragment::new(start, [exit]))
            }
        }
    }

    /// Loop construction for non-nullable bodies — no sentinel needed.
    fn build_loop_inner(
        &mut self,
        loopee: &Node,
        quant: &crate::ir::Quantifier,
    ) -> Result<Fragment> {
        // ES2015 §22.2.2.5: at every iteration entry, capture groups *inside*
        // the loop body reset to "undefined". Pre-compute the Nil-write ops
        // and attach them to every eps edge that enters the body (whether
        // first entry, optional iteration entry, or back-edge from a body
        // end). This is also what lets canonicalize collapse subsets that
        // differ only in stale-but-soon-reset marks — see the discussion
        // in `apply_eps_ops` in tdfa.rs.
        let reset_ops = collect_loop_reset_ops(loopee);
        let start = self.make()?;
        let greedy = quant.greedy;
        let mut current_ends = smallvec![start];
        // The start handle of the most-recently-built body. For min >= 1
        // we'll reuse it as the back-edge target rather than building a
        // fresh body copy — matches rust-regex's `c_at_least(1)` shape
        // and keeps the eps-closure dedup effective across iterations.
        let mut last_body_start: Option<StateHandle> = None;

        // Unroll minimum iterations. Finite automata can't count.
        for _i in 0..quant.min {
            let mut next_ends: SmallVec<[StateHandle; 2]> = smallvec![];
            for current_end in current_ends {
                let body = self.build(loopee)?;
                last_body_start = Some(body.start);
                self.get(current_end)
                    .add_eps_with_writes(body.start, reset_ops.iter().copied());
                next_ends.extend(body.ends);
            }
            current_ends = next_ends;
        }

        match quant.max {
            None => {
                // Unbounded. Reuse the last-unrolled body for the back-edge
                // when possible; only build a fresh copy if min == 0 (no
                // body has been built yet).
                let exit: u32 = self.make()?;
                let recur = if let Some(s) = last_body_start {
                    s
                } else {
                    let body = self.build(loopee)?;
                    add_loop_iter_eps(self, &current_ends, body.start, exit, greedy, &reset_ops);
                    current_ends = body.ends;
                    body.start
                };

                // Body back-edge from the last body's ends.
                add_loop_iter_eps(self, &current_ends, recur, exit, greedy, &reset_ops);

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

                    add_loop_iter_eps(self, &current_ends, recur, exit, greedy, &reset_ops);
                    current_ends = body.ends;
                }

                // Last iteration goes to exit (no further iteration entry,
                // so no resets).
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
        let (open_tag, close_tag) = capture_tags(group_id);

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

/// Tags reserved for capture group `id`'s open/close positions. Tags 0
/// and 1 are reserved for the full match, so group N occupies
/// `((N+1)*2, (N+1)*2 + 1)`.
#[inline]
fn capture_tags(id: CaptureGroupID) -> (TagIdx, TagIdx) {
    let open = (id as TagIdx + 1) * 2;
    (open, open + 1)
}

/// Enumerate Nil-write ops for every capture group inside `loopee`.
/// Used by `build_loop` to attach iteration-entry resets. Nested loops
/// contribute their captures transitively because `ir::walk` recurses;
/// the inner loop's own resets will fire redundantly on top, which is
/// harmless.
fn collect_loop_reset_ops(loopee: &Node) -> SmallVec<[TagOp; 8]> {
    let mut ops: SmallVec<[TagOp; 8]> = SmallVec::new();
    ir::walk(
        /* postorder */ false,
        /* unicode */ false,
        loopee,
        &mut |n, _| {
            if let Node::CaptureGroup { id, .. } = n {
                let (open, close) = capture_tags(*id);
                ops.push(TagOp::nil(open));
                ops.push(TagOp::nil(close));
            }
        },
    );
    ops
}

/// Wire up an iteration-entry split: from each source state, add two eps
/// edges — one to `recur` (carrying the loop body's reset ops) and one
/// to `exit` (no resets, since exiting the loop preserves the last
/// iteration's captures). Greedy quantifiers want `recur` higher
/// priority; non-greedy want `exit` higher.
/// Emit a pair of "continue / exit" eps edges for a nullable-body loop's
/// gated section. Each source state gets two eps in priority order
/// (greedy: continue preferred). The continue eps carries iteration
/// resets plus a `CurrentPos` write to `sentinel`. When `gated`, both
/// eps are guarded by `ProgressSince(sentinel)` — encoding the spec
/// rule that iters past `min` may not match empty.
fn add_gated_entry_eps(
    builder: &mut Builder,
    sources: &[StateHandle],
    recur: StateHandle,
    exit: StateHandle,
    greedy: bool,
    reset_ops: &[TagOp],
    sentinel: TagIdx,
    gated: bool,
) {
    let mut continue_ops: SmallVec<[TagOp; 2]> = SmallVec::new();
    continue_ops.extend(reset_ops.iter().copied());
    continue_ops.push(TagOp::current_pos(sentinel));
    let cond = if gated {
        EpsCondition::ProgressSince(sentinel)
    } else {
        EpsCondition::Always
    };
    for &src in sources {
        let state = builder.get(src);
        let continue_edge = EpsEdge {
            target: recur,
            ops: continue_ops.clone(),
            cond: cond.clone(),
        };
        let exit_edge = EpsEdge {
            target: exit,
            ops: smallvec![],
            cond: cond.clone(),
        };
        if greedy {
            state.eps.push(continue_edge);
            state.eps.push(exit_edge);
        } else {
            state.eps.push(exit_edge);
            state.eps.push(continue_edge);
        }
    }
}

fn add_loop_iter_eps(
    builder: &mut Builder,
    sources: &[StateHandle],
    recur: StateHandle,
    exit: StateHandle,
    greedy: bool,
    reset_ops: &[TagOp],
) {
    for &src in sources {
        let state = builder.get(src);
        if greedy {
            state.add_eps_with_writes(recur, reset_ops.iter().copied());
            state.add_eps(exit);
        } else {
            state.add_eps(exit);
            state.add_eps_with_writes(recur, reset_ops.iter().copied());
        }
    }
}

/// Count the highest capture-group id + 1 in `node`. Used by `Nfa::try_from`
/// to size the capture-tag range *before* construction so sentinel tags
/// can be allocated above it.
fn count_capture_groups(node: &Node) -> usize {
    let mut max_id: i64 = -1;
    ir::walk(
        /* postorder */ false,
        /* unicode */ false,
        node,
        &mut |n, _| {
            if let Node::CaptureGroup { id, .. } = n {
                max_id = max_id.max(*id as i64);
            }
        },
    );
    (max_id + 1) as usize
}

/// Collect capture-group names from the IR in id order. Mirrors what
/// `emit::emit` does when populating `CompiledRegex::group_names`: visit
/// every `CaptureGroup` node and push its (possibly-empty) name. Returns an
/// empty slice if no group is named — same convention as `CompiledRegex`.
fn collect_group_names(node: &Node) -> Box<[Box<str>]> {
    let mut names: Vec<Box<str>> = Vec::new();
    ir::walk(
        /* postorder */ false,
        /* unicode */ false,
        node,
        &mut |n, _| {
            if let Node::CaptureGroup { id, name, .. } = n {
                let idx = *id as usize;
                if names.len() <= idx {
                    names.resize(idx + 1, Box::<str>::from(""));
                }
                names[idx] = name.as_deref().unwrap_or("").into();
            }
        },
    );
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
        // Pre-compute the capture-tag count so any sentinel tags allocated
        // during construction land *above* the capture range.
        let num_capture_tags = 2 + 2 * count_capture_groups(&re.node);
        let mut b: Builder = Builder::new(2048 * 16, re.flags.unicode, num_capture_tags);

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
            num_capture_tags: b.num_capture_tags,
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

    pub fn num_capture_tags(&self) -> usize {
        self.num_capture_tags
    }

    /// Capture-group names indexed by capture-group id (groups without a
    /// name have an empty string). Empty when no groups are named, matching
    /// the convention used by `CompiledRegex`.
    pub fn group_names(&self) -> &[Box<str>] {
        &self.group_names
    }
}
