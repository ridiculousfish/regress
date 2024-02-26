//! Optimizations on regex IR

use crate::insn::{MAX_BYTE_SET_LENGTH, MAX_CHAR_SET_LENGTH};
use crate::ir::*;
use crate::types::BracketContents;
use crate::unicode;
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

/// When unrolling a loop, the largest minimum count we will unroll.
const LOOP_UNROLL_THRESHOLD: usize = 5;

/// Things that a Pass may do.
pub enum PassAction {
    // Do nothing to the given node.
    Keep,

    // Notes that we modified the node in-place.
    Modified,

    // Remove the given node outright, effectively replacing it with empty.
    Remove,

    /// Replace the given node with a new Node.
    Replace(Node),
}

#[derive(Debug)]
struct Pass<'a, F>
where
    F: FnMut(&mut Node, &Walk) -> PassAction,
{
    // The function.
    func: &'a mut F,

    // Whether this pass has changed anything.
    changed: bool,

    // If the regex is in unicode mode.
    unicode: bool,
}

impl<'a, F> Pass<'a, F>
where
    F: FnMut(&mut Node, &Walk) -> PassAction,
{
    fn new(func: &'a mut F, unicode: bool) -> Self {
        Pass {
            func,
            changed: false,
            unicode,
        }
    }

    fn run_postorder(&mut self, start: &mut Node) {
        walk_mut(
            true,
            self.unicode,
            start,
            &mut |n: &mut Node, walk: &mut Walk| match (self.func)(n, walk) {
                PassAction::Keep => {}
                PassAction::Modified => {
                    self.changed = true;
                }
                PassAction::Remove => {
                    *n = Node::Empty;
                    self.changed = true;
                }
                PassAction::Replace(newnode) => {
                    *n = newnode;
                    self.changed = true;
                }
            },
        )
    }

    fn run_to_fixpoint(&mut self, n: &mut Node) {
        debug_assert!(!self.changed, "Pass has already been run");
        loop {
            self.changed = false;
            self.run_postorder(n);
            if !self.changed {
                break;
            }
        }
    }
}

/// Run a "pass" on a regex, which is a function that takes a Node and maybe
/// returns a new node. \return true if something changed, false if nothing did.
fn run_pass<F>(r: &mut Regex, func: &mut F) -> bool
where
    F: FnMut(&mut Node, &Walk) -> PassAction,
{
    let mut p = Pass::new(func, r.flags.unicode);
    p.run_to_fixpoint(&mut r.node);
    p.changed
}

// Here are some optimizations we support.

// Remove empty Nodes.
fn remove_empties(n: &mut Node, _w: &Walk) -> PassAction {
    match n {
        Node::Empty | Node::Goal | Node::Char { .. } => PassAction::Keep,
        Node::ByteSequence(v) => {
            if v.is_empty() {
                PassAction::Remove
            } else {
                PassAction::Keep
            }
        }

        // Note: do not remove empty sets. These always match against one character; an empty set
        // should just fail.
        Node::ByteSet(..) | Node::CharSet(..) => PassAction::Keep,
        Node::Cat(nodes) => {
            let blen = nodes.len();
            nodes.retain(|nn| !nn.is_empty());
            if nodes.len() == blen {
                // Nothing was removed.
                PassAction::Keep
            } else {
                match nodes.len() {
                    0 => PassAction::Remove,
                    1 => PassAction::Replace(nodes.pop().unwrap()),
                    _ => PassAction::Modified,
                }
            }
        }
        Node::Alt(left, right) => {
            // Empty alt may match the empty string.
            // Remove it only if both sides are empty.
            if left.is_empty() && right.is_empty() {
                PassAction::Remove
            } else {
                PassAction::Keep
            }
        }
        Node::MatchAny | Node::MatchAnyExceptLineTerminator | Node::Anchor { .. } => {
            PassAction::Keep
        }
        Node::Loop {
            quant,
            loopee,
            enclosed_groups,
        } => {
            // A loop is empty if it has an empty body, or 0 max iters.
            // But do not remove contained capture groups.
            if loopee.is_empty() || (quant.max == 0 && enclosed_groups.start == enclosed_groups.end)
            {
                PassAction::Remove
            } else {
                PassAction::Keep
            }
        }
        Node::Loop1CharBody { .. } => PassAction::Keep,
        Node::CaptureGroup(..) | Node::NamedCaptureGroup(..) => {
            // Capture groups could in principle be optimized if they only match empties.
            PassAction::Keep
        }
        Node::WordBoundary { .. } | Node::BackRef { .. } | Node::Bracket { .. } => PassAction::Keep,
        Node::LookaroundAssertion {
            negate, contents, ..
        } => {
            // Negative arounds that match empties could in principle be optimized to always
            // fail. Here we only optimize positive ones.
            if !*negate && contents.is_empty() {
                PassAction::Remove
            } else {
                PassAction::Keep
            }
        }
    }
}

// Remove excess cats.
fn decat(n: &mut Node, _w: &Walk) -> PassAction {
    match n {
        Node::Cat(nodes) => {
            if nodes.is_empty() {
                PassAction::Remove
            } else if nodes.len() == 1 {
                PassAction::Replace(nodes.pop().unwrap())
            } else if nodes.iter().any(|nn| nn.is_cat()) {
                // Flatmap child cats.
                // Unfortunately we can't use flatmap() because there's no single iterator type
                // we can return.
                // Avoid copying nodes by switching them into owned vec.
                let mut catted = Vec::new();
                core::mem::swap(nodes, &mut catted);

                // Decat them.
                let mut decatted = Vec::new();
                for nn in catted {
                    match nn {
                        Node::Cat(mut nnodes) => {
                            decatted.append(&mut nnodes);
                        }
                        _ => decatted.push(nn),
                    }
                }
                PassAction::Replace(Node::Cat(decatted))
            } else {
                PassAction::Keep
            }
        }
        _ => PassAction::Keep,
    }
}

/// Unfold icase chars.
/// That means for case-insensitive characters, figure out everything that they
/// could match.
/// TODO: should cache unfolding.
fn unfold_icase_chars(n: &mut Node, w: &Walk) -> PassAction {
    match *n {
        Node::Char { c, icase } if icase && !w.unicode => {
            let unfolded = unicode::unfold_uppercase_char(c);
            debug_assert!(
                unfolded.contains(&c),
                "Char should always unfold to at least itself"
            );
            match unfolded.len() {
                0 => panic!("Char should always unfold to at least itself"),
                1 => {
                    // Character does not fold or unfold at all.
                    PassAction::Replace(Node::Char { c, icase: false })
                }
                2..=MAX_BYTE_SET_LENGTH => {
                    // We unfolded to 2+ characters.
                    PassAction::Replace(Node::CharSet(unfolded))
                }
                _ => panic!("Unfolded to more characters than we believed possible"),
            }
        }
        Node::Char { c, icase } if icase => {
            let unfolded = unicode::unfold_char(c);
            debug_assert!(
                unfolded.contains(&c),
                "Char should always unfold to at least itself"
            );
            match unfolded.len() {
                0 => panic!("Char should always unfold to at least itself"),
                1 => {
                    // Character does not fold or unfold at all.
                    PassAction::Replace(Node::Char { c, icase: false })
                }
                2..=MAX_BYTE_SET_LENGTH => {
                    // We unfolded to 2+ characters.
                    PassAction::Replace(Node::CharSet(unfolded))
                }
                _ => panic!("Unfolded to more characters than we believed possible"),
            }
        }
        _ => PassAction::Keep,
    }
}

// Perform simple unrolling of loops that have a minimum.
fn unroll_loops(n: &mut Node, _w: &Walk) -> PassAction {
    match n {
        Node::Loop {
            loopee,
            quant,
            enclosed_groups,
        } => {
            // TODO: consider ignoring loops with nested sub-loops?

            // Do not unroll loops with enclosed groups.
            if enclosed_groups.start < enclosed_groups.end {
                return PassAction::Keep;
            }
            // Do not unroll large loops, or loops which may execute zero times.
            if quant.min == 0 || quant.min > LOOP_UNROLL_THRESHOLD {
                return PassAction::Keep;
            }

            // We made it through. Replace us with a cat.
            let mut unrolled = Vec::new();
            for _ in 0..quant.min {
                let Some(node) = loopee.try_duplicate(0) else {
                    return PassAction::Keep;
                };
                unrolled.push(node);
            }

            // We unrolled 'min' elements.
            // Maybe our loop is now empty.
            quant.max -= quant.min;
            quant.min = 0;
            if quant.max > 0 {
                // Move the loop to the end of unrolled.
                let mut loop_node = Node::Empty;
                core::mem::swap(&mut loop_node, n);
                unrolled.push(loop_node);
            }
            *n = Node::Cat(unrolled);
            PassAction::Modified
        }
        _ => PassAction::Keep,
    }
}

/// Replace Loops with 1Char loops whenever possible.
fn promote_1char_loops(n: &mut Node, _w: &Walk) -> PassAction {
    match n {
        Node::Loop {
            loopee,
            quant,
            enclosed_groups,
        } => {
            // Must be 1Char.
            if !loopee.matches_exactly_one_char() {
                return PassAction::Keep;
            }

            // The above check should be sufficient to ensure we have no enclosed groups.
            assert!(
                enclosed_groups.start >= enclosed_groups.end,
                "Should have no enclosed groups"
            );

            // This feels hackish?
            let mut new_loopee = Box::new(Node::Empty);
            core::mem::swap(&mut new_loopee, loopee);

            *n = Node::Loop1CharBody {
                loopee: new_loopee,
                quant: *quant,
            };
            PassAction::Modified
        }
        _ => PassAction::Keep,
    }
}

/// Replace Cat(Char) with ByteSeq.
/// Also replace chars with literal bytes.
/// TODO: this seems to do too much; consider breaking this up.
fn form_literal_bytes(n: &mut Node, walk: &Walk) -> PassAction {
    // Helper to return a mutable reference to the nodes of a literal bytes.
    fn get_literal_bytes(n: &mut Node) -> Option<&mut Vec<u8>> {
        match n {
            Node::ByteSequence(v) => Some(v),
            _ => None,
        }
    }
    match n {
        #[cfg(not(feature = "utf16"))]
        Node::Char { c, icase } if !*icase => {
            if let Some(c) = char::from_u32(*c) {
                let mut buff = [0; 4];
                PassAction::Replace(Node::ByteSequence(
                    c.encode_utf8(&mut buff).as_bytes().to_vec(),
                ))
            } else {
                PassAction::Keep
            }
        }
        Node::CharSet(chars) if chars.iter().all(|&c| c <= 0x7F) => {
            // All of our chars are ASCII; use a byte set instead.
            PassAction::Replace(Node::ByteSet(chars.iter().map(|&c| c as u8).collect()))
        }
        Node::Cat(nodes) => {
            // Find and merge adjacent ByteSeq.
            let mut modified = false;
            for idx in 1..nodes.len() {
                let (prev_slice, curr_slice) = nodes.split_at_mut(idx);
                match (
                    get_literal_bytes(prev_slice.last_mut().unwrap()),
                    get_literal_bytes(curr_slice.first_mut().unwrap()),
                ) {
                    (Some(prev_bytes), Some(curr_bytes))
                        if !prev_bytes.is_empty() && !curr_bytes.is_empty() =>
                    {
                        if walk.in_lookbehind {
                            // Our characters were already reversed; we need to reverse them again
                            // as literal bytes are always forwards. For
                            // example, if we have (<=ab), then we will get Cat(b, a) but we want
                            // literal bytes "ab".
                            curr_bytes.append(prev_bytes);
                        } else {
                            prev_bytes.append(curr_bytes);
                            core::mem::swap(prev_bytes, curr_bytes);
                        }
                        modified = true;
                    }
                    _ => (),
                }
            }
            if modified {
                PassAction::Modified
            } else {
                PassAction::Keep
            }
        }
        _ => PassAction::Keep,
    }
}

/// Try to reduce a bracket to something simpler.
fn try_reduce_bracket(bc: &BracketContents) -> Option<Node> {
    if bc.invert {
        // Give up.
        return None;
    }

    // Count the number of code points.
    let mut cps_count = 0;
    for iv in bc.cps.intervals() {
        cps_count += iv.count_codepoints();
    }
    if cps_count > MAX_CHAR_SET_LENGTH {
        // Too many code points.
        return None;
    }

    // Ok, we want to make a char set.
    // Note we cannot make a char out of surrogates; should char conversion fail we
    // just give up.
    let mut res = Vec::new();
    for iv in bc.cps.intervals() {
        for cp in iv.codepoints() {
            res.push(cp);
        }
    }
    debug_assert!(res.len() <= MAX_CHAR_SET_LENGTH, "Unexpectedly many chars");
    Some(Node::CharSet(res))
}

/// Optimize brackets like [a-b].
/// Optimize certain stupid brackets like [a] to a single char.
/// Invert a bracket if it would *reduce* the number of ranges.
/// Note we only run this once.
fn simplify_brackets(n: &mut Node, _walk: &Walk) -> PassAction {
    match n {
        Node::Bracket(bc) => {
            if let Some(new_node) = try_reduce_bracket(bc) {
                return PassAction::Replace(new_node);
            }

            // TODO: does this ever help anything?
            if bc.cps.intervals().len() > bc.cps.inverted_interval_count() {
                bc.cps = bc.cps.inverted();
                bc.invert = !bc.invert;
                PassAction::Modified
            } else {
                PassAction::Keep
            }
        }
        _ => PassAction::Keep,
    }
}

pub fn optimize(r: &mut Regex) {
    run_pass(r, &mut simplify_brackets);
    loop {
        let mut changed = false;
        changed |= run_pass(r, &mut decat);
        if r.flags.icase {
            changed |= run_pass(r, &mut unfold_icase_chars);
        }
        changed |= run_pass(r, &mut unroll_loops);
        changed |= run_pass(r, &mut promote_1char_loops);
        changed |= run_pass(r, &mut form_literal_bytes);
        changed |= run_pass(r, &mut remove_empties);
        if !changed {
            break;
        }
    }
}
