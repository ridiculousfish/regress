//! Conversion of IR to non-deterministic finite automata.

use crate::automata::nfa::{ByteRange, Nfa, StateHandle, GOAL_STATE};
use crate::ir::Node;
use core::fmt;

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
pub(super) fn node_description(node: &Node) -> String {
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
            if state.eps.is_empty() && state.transitions.is_empty() && !state_idx != GOAL_STATE {
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
