//! UTF-8 trie construction for matching code-point sets in the NFA.
//!
//! Turns a `CodePointSet` into a chain of byte transitions on the NFA builder:
//! one entry state, one exit state, and intermediate states wired so that any
//! UTF-8 encoding of a code point in the set walks from entry to exit and
//! nothing else does.
//!
//! The construction does both prefix sharing (paths grouped by leading byte
//! range at each depth) and suffix sharing (subtrees with identical transition
//! vectors are deduped). Built into a scratch builder first so the orphan
//! states from eager allocation don't pollute the real NFA; only reachable
//! states get copied over.

use crate::automata::nfa::{Builder, ByteRange, Fragment, Result, StateHandle};
use crate::automata::utf8::{ByteRangePath, utf8_paths_from_code_point_set};
use crate::codepointset::CodePointSet;
use crate::literal::{Piece, lower_code_point_sequence};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use smallvec::{SmallVec, smallvec};
use std::collections::{HashMap, HashSet, hash_map::Entry};

/// Push `state` onto `set` unless already present (small linear scan; the
/// insertion frontier is tiny in practice).
fn push_unique(set: &mut SmallVec<[StateHandle; 4]>, state: StateHandle) {
    if !set.contains(&state) {
        set.push(state);
    }
}

impl Builder {
    pub(super) fn build_from_code_point_set(&mut self, cps: &CodePointSet) -> Result<Fragment> {
        let paths = utf8_paths_from_code_point_set(cps);
        // No paths means nothing is matchable: either the set is empty, or it
        // contained only surrogates (which fall in the gap between UTF-8 buckets
        // and so produce no paths). Either way, a state with no exits.
        if paths.is_empty() {
            let fail = self.make()?;
            return Ok(Fragment::new(fail, []));
        }
        self.build_trie(&paths)
    }

    /// Build the minimal acyclic DFA over a `Node::StringSet`'s alternatives.
    ///
    /// A `StringSet` is a prioritized alternation of literal code-point
    /// sequences. We share common prefixes *and* suffixes, so the result is the
    /// unique minimal deterministic automaton for the set: byte transitions stay
    /// deterministic, no epsilons are introduced, and each accepting state
    /// becomes a loose end of the fragment (a node may be both internal and
    /// accepting, e.g. `©` is a prefix of `©️`). Suffix sharing in particular
    /// collapses all the maximal sequences into one shared accept-leaf, which
    /// keeps the `ends` set — and any surrounding loop's epsilon wiring — small.
    ///
    /// Built in a scratch builder (like `build_trie`) so the prefix-trie phase's
    /// transient states don't pollute the real NFA; only the minimized, reachable
    /// states are copied over.
    pub(super) fn build_string_set(
        &mut self,
        alternatives: &[Box<[u32]>],
        icase: bool,
    ) -> Result<Fragment> {
        // Phase 1: build a prefix trie in scratch, recording accepting states.
        let mut scratch = Builder::new(self.state_budget, self.unicode, self.num_capture_tags);
        let scratch_start = scratch.make()?;
        let mut accept: HashSet<StateHandle> = HashSet::new();
        for alt in alternatives {
            // The frontier holds every trie state reachable after consuming the
            // pieces lowered so far; folds (sets) can make it branch.
            let mut frontier: SmallVec<[StateHandle; 4]> = smallvec![scratch_start];
            for piece in lower_code_point_sequence(alt, icase, self.unicode) {
                frontier = scratch.step_piece(&frontier, &piece)?;
            }
            accept.extend(frontier);
        }

        // Phase 2: minimize by merging states with identical (accept, outgoing)
        // signatures, bottom-up.
        let mut memo: HashMap<StateHandle, StateHandle> = HashMap::new();
        let mut dedup: HashMap<(bool, Vec<(ByteRange, StateHandle)>), StateHandle> = HashMap::new();
        let canon_start = scratch.minimize_trie(scratch_start, &accept, &mut memo, &mut dedup);

        // Phase 3: copy the minimized, reachable states into `self`.
        let mut state_map: HashMap<StateHandle, StateHandle> = HashMap::new();
        let self_start = self.take_from_scratch(&mut scratch, canon_start, &mut state_map)?;

        // The fragment's ends are the (canonical, then renumbered) accept states.
        // Sorted for deterministic state numbering; order doesn't affect runtime
        // priority since the automaton is deterministic.
        let mut ends: SmallVec<[StateHandle; 2]> = accept
            .iter()
            .map(|t| state_map[&memo[t]])
            .collect();
        ends.sort_unstable();
        ends.dedup();
        Ok(Fragment::new(self_start, ends))
    }

    /// Minimize the acyclic trie rooted at `state`: merge any two states with the
    /// same accept-status and the same outgoing transitions (to already-merged
    /// targets). Returns the canonical handle for `state`; `memo` maps every
    /// visited state to its canonical representative. The representative keeps its
    /// transitions rewired to canonical targets; collapsed states are left
    /// orphaned (dropped by the later reachable-only copy).
    fn minimize_trie(
        &mut self,
        state: StateHandle,
        accept: &HashSet<StateHandle>,
        memo: &mut HashMap<StateHandle, StateHandle>,
        dedup: &mut HashMap<(bool, Vec<(ByteRange, StateHandle)>), StateHandle>,
    ) -> StateHandle {
        if let Some(&canon) = memo.get(&state) {
            return canon;
        }
        // Canonicalize children first (post-order). Transitions are already
        // sorted by byte range, and canonicalization preserves that order.
        let transitions = self.get(state).transitions.clone();
        let mut canon_transitions: Vec<(ByteRange, StateHandle)> = Vec::with_capacity(transitions.len());
        for (range, child) in transitions {
            let target = self.minimize_trie(child, accept, memo, dedup);
            // Coalesce with the previous edge when it shares this target and its
            // range is contiguous — neighbouring bytes only converge once
            // minimization has merged their (formerly distinct) targets, so this
            // is the first point where it can happen. `checked_add` guards 0xFF.
            match canon_transitions.last_mut() {
                Some((prev, prev_target))
                    if *prev_target == target
                        && prev.end.checked_add(1) == Some(range.start) =>
                {
                    prev.end = range.end;
                }
                _ => canon_transitions.push((range, target)),
            }
        }

        let key = (accept.contains(&state), canon_transitions.clone());
        let canon = match dedup.entry(key) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                // First state with this signature: make it the representative.
                self.get(state).transitions = canon_transitions;
                *e.insert(state)
            }
        };
        memo.insert(state, canon);
        canon
    }

    /// Advance every state in `frontier` by one lowered `Piece`, returning the
    /// deduplicated set of resulting states.
    fn step_piece(
        &mut self,
        frontier: &[StateHandle],
        piece: &Piece,
    ) -> Result<SmallVec<[StateHandle; 4]>> {
        let mut next: SmallVec<[StateHandle; 4]> = SmallVec::new();
        for &from in frontier {
            match piece {
                // A literal run: walk its bytes, staying on a single path.
                Piece::ByteSequence(bytes) => {
                    let mut state = from;
                    for &b in bytes {
                        state = self.trie_step(state, b)?;
                    }
                    push_unique(&mut next, state);
                }
                // A folded ASCII position: each byte is its own one-byte branch.
                Piece::ByteSet(bytes) => {
                    for &b in bytes {
                        push_unique(&mut next, self.trie_step(from, b)?);
                    }
                }
                // A folded non-ASCII position: each variant walks its UTF-8.
                Piece::CharSet(chars) => {
                    for &c in chars {
                        // A surrogate variant can't occur in UTF-8 input; skip it
                        // (that fold variant simply never matches).
                        let Some(ch) = char::from_u32(c) else {
                            continue;
                        };
                        let mut buf = [0; 4];
                        let mut state = from;
                        for &b in ch.encode_utf8(&mut buf).as_bytes() {
                            state = self.trie_step(state, b)?;
                        }
                        push_unique(&mut next, state);
                    }
                }
                // A code point with no UTF-8 encoding (surrogate): unmatchable in
                // UTF-8, so this position kills the alternative — contribute
                // nothing, collapsing its frontier to empty (never matches).
                Piece::Char(_) => {}
            }
        }
        Ok(next)
    }

    /// Follow the transition on byte `b` from `from`, creating it (and a fresh
    /// destination state) if absent. Transitions stay deterministic: at most
    /// one target per byte.
    fn trie_step(&mut self, from: StateHandle, b: u8) -> Result<StateHandle> {
        if let Some(next) = self.get(from).transition_for_byte(b) {
            return Ok(next);
        }
        let next = self.make()?;
        self.get(from).add_transition(ByteRange::new(b, b), next);
        Ok(next)
    }

    fn build_trie(&mut self, paths: &[ByteRangePath]) -> Result<Fragment> {
        // Our recursive implementation constructs a lot of orphan nodes (todo: avoid this),
        // so build into scratch and then renumber.
        let mut scratch = Builder::new(self.state_budget, self.unicode, self.num_capture_tags);
        let scratch_from = scratch.make()?;
        let scratch_to = scratch.make()?;
        let indices: Vec<usize> = (0..paths.len()).collect();
        scratch.build_trie_impl(
            scratch_from,
            scratch_to,
            paths,
            &indices,
            0,
            &mut HashMap::new(),
        )?;

        let mut scratch_to_self = HashMap::new();
        let self_from = self.take_from_scratch(&mut scratch, scratch_from, &mut scratch_to_self)?;
        let self_to = scratch_to_self[&scratch_to];
        Ok(Fragment::new(self_from, [self_to]))
    }

    /// Build a trie from `paths` rooted at `from`, all terminating at `to`.
    /// Only paths whose index is contained in `indices` are considered.
    /// The `depth` parameter tracks how many bytes of the paths have already been consumed.
    /// `dedup` enables suffix-sharing.
    ///
    /// Invariants relied upon:
    /// - Within a UTF-8 bucket, `segment_interval_for_utf8` emits rectangular
    ///   paths whose byte ranges at any depth are either identical or
    ///   disjoint. Across buckets, leading bytes are disjoint by UTF-8
    ///   structure. So equality-grouping in step (1) below is sound — no two
    ///   groups in the same call can have overlapping ranges.
    /// - All paths in a single group have the same length, because they all
    ///   came from the same recursion lineage (and ultimately the same bucket).
    fn build_trie_impl(
        &mut self,
        from: StateHandle,
        to: StateHandle,
        paths: &[ByteRangePath],
        indices: &[usize],
        depth: usize,
        dedup: &mut HashMap<Vec<(ByteRange, StateHandle)>, StateHandle>,
    ) -> Result<()> {
        // Group live paths by their byte range at `depth`.
        let mut groups: SmallVec<[(ByteRange, Vec<usize>); 16]> = SmallVec::new();
        for &i in indices {
            let range = paths[i][depth];
            match groups.iter_mut().find(|(r, _)| *r == range) {
                Some(g) => g.1.push(i),
                None => groups.push((range, vec![i])),
            }
        }

        // Assert that all paths within a group have the same length.
        // This reflects the structure of UTF-8: the first byte disambiguates
        // the path lengths.
        for (_, sub_indices) in &groups {
            debug_assert!(
                sub_indices
                    .iter()
                    .all(|&i| paths[i].len() == paths[sub_indices[0]].len()),
                "Paths in a group should all have the same length",
            );
        }

        // Assert that groups are ascending and not overlapping.
        // This is enforced by the disjoint-or-identical invariant.
        for i in 1..groups.len() {
            debug_assert!(
                groups[i - 1].0.end < groups[i].0.start,
                "Groups should never overlap",
            );
        }

        // ----- Step 2: for each group, wire one outgoing edge from `from`.
        for (range, sub_indices) in groups {
            // All paths in the group have the same length so
            // checking the first path's length is enough.
            let is_last = depth + 1 == paths[sub_indices[0]].len();

            if is_last {
                // Base case: every path in this group ends right after this
                // byte. The transition goes straight to the trie's `to` state;
                // no intermediate needed.
                self.get(from).add_transition(range, to);
            } else {
                // Recursive case: paths in this group still have more bytes
                // to consume. We need an intermediate state `mid` that will
                // carry their depth+1 transitions.
                //
                // Allocate `mid` eagerly. We'll fill in its transitions via
                // the recursive call, then check whether some earlier subtree
                // produced the same transition vector — if so, we drop `mid`
                // and reuse the earlier state.
                let mid_state = self.make()?;
                self.build_trie_impl(mid_state, to, paths, &sub_indices, depth + 1, dedup)?;

                // A state is defined by its transitions. Apply deduplication
                // for any state with the same transition vector.
                let actual = *dedup
                    .entry(self.get(mid_state).transitions.clone())
                    .or_insert(mid_state);

                // Wire the edge from our parent to whichever state we ended
                // up with (`mid_state` if fresh, the deduped one otherwise).
                self.get(from).add_transition(range, actual);
            }
        }
        Ok(())
    }

    // Copy reachable scratch states to our states, renumbering using the given cache.
    // Returns the handle corresponding to `scratch_handle` in our states.
    // Prefer DFS so adjacent states get adjacent handles.
    // This is only used in build_trie: we expect no epsilon states.
    fn take_from_scratch(
        &mut self,
        scratch: &mut Builder,
        scratch_handle: StateHandle,
        state_map: &mut HashMap<StateHandle, StateHandle>, // from scratch to self
    ) -> Result<StateHandle> {
        match state_map.entry(scratch_handle) {
            Entry::Occupied(e) => Ok(*e.get()),
            Entry::Vacant(e) => {
                let new_handle = self.make()?;
                e.insert(new_handle);
                let scratch_state = scratch.get(scratch_handle);

                // We should never need to remap epsilons, because build_trie should produce none.
                debug_assert!(scratch_state.eps.is_empty(), "Should have no epsilons");

                // Take the non-epsilon transitions outright! And remap their targets in-place.
                let mut transitions = std::mem::take(&mut scratch_state.transitions);
                for (_, target) in &mut transitions {
                    *target = self.take_from_scratch(scratch, *target, state_map)?;
                }
                self.get(new_handle).transitions = transitions;

                Ok(new_handle)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::automata::nfa::{Builder, State, StateHandle};

    /// A `StringSet` alternative: one code point per `char`.
    fn seq(s: &str) -> Box<[u32]> {
        s.chars().map(u32::from).collect()
    }

    /// Walk `bytes` from `start`, returning the state reached (or `None` if some
    /// byte has no transition).
    fn walk(states: &[State], start: StateHandle, bytes: &[u8]) -> Option<StateHandle> {
        let mut s = start;
        for &b in bytes {
            s = states[s as usize].transition_for_byte(b)?;
        }
        Some(s)
    }

    /// "a", "ab", "abc": each is a prefix of the next, so the trie is a single
    /// shared chain and every intermediate node also accepts.
    #[test]
    fn prefix_sharing() {
        let mut b = Builder::new(usize::MAX, true, 2);
        let frag = b
            .build_string_set(&[seq("a"), seq("ab"), seq("abc")], false)
            .unwrap();

        let s_a = walk(&b.states, frag.start, b"a").unwrap();
        let s_ab = walk(&b.states, frag.start, b"ab").unwrap();
        let s_abc = walk(&b.states, frag.start, b"abc").unwrap();

        // All three terminals accept; "abcd" runs off the end of the trie.
        assert!(frag.ends.contains(&s_a));
        assert!(frag.ends.contains(&s_ab));
        assert!(frag.ends.contains(&s_abc));
        assert_eq!(frag.ends.len(), 3);
        assert_eq!(walk(&b.states, frag.start, b"abcd"), None);

        // The shared prefix means no branching along the chain.
        for s in [frag.start, s_a, s_ab] {
            assert_eq!(b.states[s as usize].transitions.len(), 1);
        }
    }

    /// "ab" and "ac" share the "a" prefix, then branch. Their terminal leaves
    /// are equivalent (accepting, no transitions), so minimization merges them
    /// into a single shared accept-leaf.
    #[test]
    fn shared_prefix_then_branch() {
        let mut b = Builder::new(usize::MAX, true, 2);
        let frag = b.build_string_set(&[seq("ab"), seq("ac")], false).unwrap();

        let s_a = walk(&b.states, frag.start, b"a").unwrap();
        assert_eq!(b.states[frag.start as usize].transitions.len(), 1);
        // 'b' and 'c' both lead to the merged leaf and are adjacent bytes, so
        // they coalesce into a single 'b'-'c' range edge.
        assert_eq!(b.states[s_a as usize].transitions.len(), 1);
        // Both branches land on the same merged leaf → a single end.
        assert_eq!(frag.ends.len(), 1);
        let s_ab = walk(&b.states, frag.start, b"ab").unwrap();
        let s_ac = walk(&b.states, frag.start, b"ac").unwrap();
        assert_eq!(s_ab, s_ac);
        assert!(frag.ends.contains(&s_ab));
    }

    /// © (U+00A9) is a prefix of ©️ (U+00A9 U+FE0F): the © terminal must both
    /// accept *and* continue into the VS16 bytes. Mirrors `\p{Basic_Emoji}`.
    #[test]
    fn multibyte_prefix_overlap() {
        let mut b = Builder::new(usize::MAX, true, 2);
        let frag = b
            .build_string_set(&[seq("\u{00A9}"), seq("\u{00A9}\u{FE0F}")], false)
            .unwrap();

        let s_c = walk(&b.states, frag.start, "\u{00A9}".as_bytes()).unwrap();
        let s_v = walk(&b.states, frag.start, "\u{00A9}\u{FE0F}".as_bytes()).unwrap();

        assert!(frag.ends.contains(&s_c));
        assert!(frag.ends.contains(&s_v));
        // The © state is internal as well as accepting.
        assert!(!b.states[s_c as usize].transitions.is_empty());
    }

    /// icase folds "a" into {A, a}: two single-byte branches from start, no
    /// cartesian blowup. Both land on the same merged accept-leaf.
    #[test]
    fn icase_fold_branches() {
        let mut b = Builder::new(usize::MAX, true, 2);
        let frag = b.build_string_set(&[seq("a")], true).unwrap();

        assert_eq!(b.states[frag.start as usize].transitions.len(), 2);
        let upper = walk(&b.states, frag.start, b"A").unwrap();
        let lower = walk(&b.states, frag.start, b"a").unwrap();
        assert_eq!(upper, lower);
        assert_eq!(frag.ends.len(), 1);
        assert!(frag.ends.contains(&upper));
    }

    /// Suffix sharing: "ax" and "bx" share no prefix but a common "x" tail.
    /// Minimization merges the trailing leaf *and* the two "middle" states
    /// (both non-accepting with an identical `x`→leaf edge), collapsing to
    /// `start —a,b→ M —x→ X`. Total states: GOAL + start + M + X = 4.
    #[test]
    fn suffix_sharing() {
        let mut b = Builder::new(usize::MAX, true, 2);
        let frag = b.build_string_set(&[seq("ax"), seq("bx")], false).unwrap();

        let s_ax = walk(&b.states, frag.start, b"ax").unwrap();
        let s_bx = walk(&b.states, frag.start, b"bx").unwrap();
        assert_eq!(s_ax, s_bx);
        assert!(b.states[s_ax as usize].transitions.is_empty());
        // 'a' and 'b' lead to the same merged middle state, and are adjacent
        // bytes, so they coalesce into a single 'a'-'b' range edge.
        assert_eq!(walk(&b.states, frag.start, b"a"), walk(&b.states, frag.start, b"b"));
        assert_eq!(b.states[frag.start as usize].transitions.len(), 1);
        assert_eq!(frag.ends.len(), 1);
        assert_eq!(b.states.len(), 4);
    }

    /// Adjacent single-byte alternatives that share a suffix (here the empty
    /// suffix: all three are single chars) collapse to one accept-leaf, and the
    /// `a`/`b`/`c` edges out of start coalesce into a single `a`-`c` range edge.
    #[test]
    fn coalesce_adjacent_transitions() {
        let mut b = Builder::new(usize::MAX, true, 2);
        let frag = b
            .build_string_set(&[seq("a"), seq("b"), seq("c")], false)
            .unwrap();

        let start_trans = &b.states[frag.start as usize].transitions;
        assert_eq!(start_trans.len(), 1);
        assert_eq!(start_trans[0].0.start, b'a');
        assert_eq!(start_trans[0].0.end, b'c');

        let leaf = walk(&b.states, frag.start, b"a").unwrap();
        assert_eq!(walk(&b.states, frag.start, b"b"), Some(leaf));
        assert_eq!(walk(&b.states, frag.start, b"c"), Some(leaf));
        assert_eq!(frag.ends.len(), 1);
        // GOAL + start + shared leaf.
        assert_eq!(b.states.len(), 3);
    }

    /// An empty alternative makes the start state itself accept.
    #[test]
    fn empty_alternative_accepts_start() {
        let mut b = Builder::new(usize::MAX, true, 2);
        let frag = b.build_string_set(&[seq(""), seq("a")], false).unwrap();
        assert!(frag.ends.contains(&frag.start));
    }

    /// No alternatives: a start state with no ends and no transitions, so it
    /// matches nothing.
    #[test]
    fn empty_set_is_unmatchable() {
        let mut b = Builder::new(usize::MAX, true, 2);
        let frag = b.build_string_set(&[], false).unwrap();
        assert!(frag.ends.is_empty());
        assert!(b.states[frag.start as usize].transitions.is_empty());
    }

    /// A code-point set of only surrogates is unmatchable (no UTF-8 encoding):
    /// the fragment has no exits, and building it must not panic.
    #[test]
    fn surrogate_only_set_unmatchable() {
        use crate::codepointset::{CodePointSet, Interval};
        let mut b = Builder::new(usize::MAX, true, 2);
        let mut cps = CodePointSet::new();
        cps.add(Interval::new(0xD800, 0xDFFF));
        let frag = b.build_from_code_point_set(&cps).unwrap();
        assert!(frag.ends.is_empty());
        assert!(b.states[frag.start as usize].transitions.is_empty());
    }

    /// A set mixing a valid char with surrogates drops the surrogates and keeps
    /// the valid path.
    #[test]
    fn set_with_surrogate_keeps_valid() {
        use crate::codepointset::{CodePointSet, Interval};
        let mut b = Builder::new(usize::MAX, true, 2);
        let mut cps = CodePointSet::new();
        cps.add_one(b'a' as u32);
        cps.add(Interval::new(0xD800, 0xDFFF));
        let frag = b.build_from_code_point_set(&cps).unwrap();
        assert_eq!(frag.ends.len(), 1);
        assert_eq!(walk(&b.states, frag.start, b"a"), Some(frag.ends[0]));
        assert_eq!(b.states[frag.start as usize].transitions.len(), 1);
    }

    /// A StringSet alternative that is a lone surrogate never matches; the other
    /// alternatives are unaffected.
    #[test]
    fn string_set_surrogate_alternative_is_dead() {
        let mut b = Builder::new(usize::MAX, true, 2);
        let alts: [Box<[u32]>; 2] = [Box::from([b'a' as u32]), Box::from([0xD800u32])];
        let frag = b.build_string_set(&alts, false).unwrap();
        assert_eq!(frag.ends.len(), 1);
        assert_eq!(walk(&b.states, frag.start, b"a"), Some(frag.ends[0]));
        assert_eq!(b.states[frag.start as usize].transitions.len(), 1);
    }
}
