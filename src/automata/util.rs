//! Conversion of IR to non-deterministic finite automata.

use crate::automata::nfa::{ByteRange, GOAL_STATE, Nfa, StateHandle};
use core::fmt;

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
                            .map(|&tag| format!("t{}", tag))
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
