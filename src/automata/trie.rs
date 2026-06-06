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
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use smallvec::SmallVec;
use std::collections::{HashMap, hash_map::Entry};

impl Builder {
    pub(super) fn build_from_code_point_set(
        &mut self,
        cps: &CodePointSet,
    ) -> Result<Fragment> {
        if cps.is_empty() {
            // Can't match anything - a state with no exits.
            let fail = self.make()?;
            return Ok(Fragment::new(fail, []));
        }

        let paths = utf8_paths_from_code_point_set(cps);
        self.build_trie(&paths)
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
