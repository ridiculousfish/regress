//! Conversion of IR to non-deterministic finite automata.

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

use crate::automata::nfa::{ByteRange, EpsCondition, GOAL_STATE, Nfa, OpKind, StateHandle, TagOp};
use core::fmt;

/// Render an eps-edge tag op for debug formatters: `t3` for CurrentPos,
/// `t3=Nil` for Nil.
fn format_tag_op(op: &TagOp) -> String {
    match op.kind {
        OpKind::CurrentPos => format!("t{}", op.tag),
        OpKind::Nil => format!("t{}=Nil", op.tag),
    }
}

/// Format a byte range in a human-readable way
pub fn format_byte_range(range: ByteRange) -> String {
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
pub(super) fn format_byte(byte: u8) -> String {
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

impl Nfa {
    /// Generate a human-readable representation of the NFA
    pub fn to_readable_string(&self) -> String {
        let mut result = String::new();
        result.push_str("NFA States:\n");
        result.push_str("===========\n\n");

        for (idx, state) in self.states.iter().enumerate() {
            let state_idx = idx as StateHandle;

            // Add special state markers
            let marker = match state_idx {
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
                        let ops_str = edge
                            .ops
                            .iter()
                            .map(format_tag_op)
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
            if state.eps.is_empty() && state.transitions.is_empty() && state_idx != GOAL_STATE {
                result.push_str("  (no transitions)\n");
            }

            result.push('\n');
        }

        result
    }

    /// Generate a compact summary of the NFA's size and shape, instead of the
    /// full state-by-state dump.
    pub fn to_stats_string(&self) -> String {
        let mut byte_transitions = 0usize;
        let mut eps_transitions = 0usize;
        let mut eps_with_writes = 0usize;
        let mut eps_predicated = 0usize;
        let mut dead_states = 0usize;

        for (idx, state) in self.states.iter().enumerate() {
            byte_transitions += state.transitions.len();
            eps_transitions += state.eps.len();
            for edge in &state.eps {
                if !edge.ops.is_empty() {
                    eps_with_writes += 1;
                }
                if !matches!(edge.cond, EpsCondition::Always) {
                    eps_predicated += 1;
                }
            }
            if state.eps.is_empty()
                && state.transitions.is_empty()
                && idx as StateHandle != GOAL_STATE
            {
                dead_states += 1;
            }
        }

        let mut result = String::new();
        result.push_str("NFA stats:\n");
        result.push_str("==========\n");
        result.push_str(&format!("  states:            {}\n", self.states.len()));
        result.push_str(&format!("  dead states:       {}\n", dead_states));
        result.push_str(&format!("  byte transitions:  {}\n", byte_transitions));
        result.push_str(&format!("  eps transitions:   {}\n", eps_transitions));
        result.push_str(&format!("    with tag writes: {}\n", eps_with_writes));
        result.push_str(&format!("    predicated:      {}\n", eps_predicated));
        result.push_str(&format!("  tags:              {}\n", self.num_tags()));
        result.push_str(&format!("  capture tags:      {}\n", self.num_capture_tags()));
        result
    }
}

impl Nfa {
    /// Generate a Graphviz DOT representation of the NFA. Epsilon transitions
    /// are shown as dashed edges; tag writes on epsilon edges are labeled
    /// `ε [t0,t1,...]`. Byte transitions are solid edges labeled with their
    /// byte range. Accepting (goal) state is a double circle; start state is
    /// pointed to by a `start` marker.
    pub fn to_dot_string(&self) -> String {
        let mut out = String::new();
        out.push_str("digraph NFA {\n");
        out.push_str("    rankdir=LR;\n");
        out.push_str("    node [shape=circle];\n\n");

        out.push_str("    start [shape=point label=\"\"];\n");
        out.push_str(&format!("    start -> {};\n\n", self.start()));

        for (idx, _state) in self.states.iter().enumerate() {
            let handle = idx as StateHandle;
            let shape = if handle == GOAL_STATE {
                "doublecircle"
            } else {
                "circle"
            };
            out.push_str(&format!(
                "    {} [shape={} label=\"{}\"];\n",
                handle, shape, handle
            ));
        }
        out.push('\n');

        for (idx, state) in self.states.iter().enumerate() {
            let handle = idx as StateHandle;
            for edge in &state.eps {
                let label = if edge.ops.is_empty() {
                    "ε".to_string()
                } else {
                    let ops = edge
                        .ops
                        .iter()
                        .map(format_tag_op)
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("ε [{}]", ops)
                };
                out.push_str(&format!(
                    "    {} -> {} [style=dashed label=\"{}\"];\n",
                    handle, edge.target, label
                ));
            }
            for &(range, target) in &state.transitions {
                out.push_str(&format!(
                    "    {} -> {} [label=\"{}\"];\n",
                    handle,
                    target,
                    format_byte_range(range)
                ));
            }
        }
        out.push_str("}\n");
        out
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

/// A fixed bitset.
/// This doesn't separately track the size of the set: it rounds up to the nearest multiple of 64.
pub struct BitSet(Box<[u64]>);

impl BitSet {
    // Construct a new zeroed bitset.
    pub fn new(num_bits: usize) -> Self {
        let words = num_bits.div_ceil(64);
        // TODO: avoid potential double allocation.
        Self(vec![0u64; words].into_boxed_slice())
    }

    // Test if the bit at the given index is set.
    #[inline(always)]
    pub fn test(&self, idx: usize) -> bool {
        let word_idx = idx / 64;
        let bit_idx = idx % 64;
        (self.0[word_idx] & (1 << bit_idx)) != 0
    }

    // Set the bit at the given index.
    #[inline(always)]
    pub fn set(&mut self, idx: usize) {
        let word_idx = idx / 64;
        let bit_idx = idx % 64;
        self.0[word_idx] |= 1 << bit_idx;
    }

    // Clear the bit at the given index.
    #[inline(always)]
    pub fn clear(&mut self, idx: usize) {
        let word_idx = idx / 64;
        let bit_idx = idx % 64;
        self.0[word_idx] &= !(1 << bit_idx);
    }

    // Clear every bit.
    #[inline(always)]
    pub fn clear_all(&mut self) {
        self.0.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitset() {
        let interesting = [0, 1, 32, 62, 63, 64, 65, 127, 128, 191];
        let mut bs = BitSet::new(200);
        for i in 0..200 {
            assert!(!bs.test(i), "bit {} should start cleared", i);
        }
        for &i in &interesting {
            bs.set(i);
        }
        for i in 0..200 {
            assert_eq!(bs.test(i), interesting.contains(&i), "bit {}", i);
        }

        bs.set(63);
        bs.clear(63);
        assert!(!bs.test(63));
        assert!(bs.test(62));
        assert!(bs.test(64));
        bs.clear(50);
        assert!(!bs.test(50));

        // Set all, then clear all.
        let mut bs = BitSet::new(200);
        for i in 0..200 {
            bs.set(i);
        }
        for i in 0..200 {
            assert!(bs.test(i));
        }
        for i in 0..200 {
            bs.clear(i);
        }
        for i in 0..200 {
            assert!(!bs.test(i));
        }
    }
}
