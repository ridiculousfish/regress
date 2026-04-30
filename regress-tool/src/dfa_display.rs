use regress::automata::dfa::{DEAD_STATE, Dfa, DfaStateId};
use regress::automata::nfa::ByteRange;
use regress::automata::util::format_byte_range;

/// Map each byte class back to the contiguous byte ranges it covers.
fn class_ranges(dfa: &Dfa) -> Vec<Vec<(u8, u8)>> {
    let num_classes = dfa.num_classes();
    let byte_to_class = dfa.byte_to_class();
    let mut result = vec![Vec::new(); num_classes];
    let mut class = byte_to_class[0];
    let mut start = 0u8;
    for byte in 1u8..=255 {
        let c = byte_to_class[byte as usize];
        if c != class {
            result[class as usize].push((start, byte - 1));
            class = c;
            start = byte;
        }
    }
    result[class as usize].push((start, 255));
    result
}

/// Format the byte ranges for a class as a human-readable string.
fn format_class(ranges: &[(u8, u8)]) -> String {
    ranges
        .iter()
        .map(|&(s, e)| format_byte_range(ByteRange::new(s, e)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Generate a verbose human-readable representation of the DFA.
#[allow(dead_code)]
pub fn to_readable_string(dfa: &Dfa) -> String {
    let mut out = String::new();
    let class_ranges = class_ranges(dfa);
    let num_states = dfa.num_states();
    let num_classes = dfa.num_classes();
    let transitions = dfa.transitions();
    let accepting = dfa.accepting();
    let start = dfa.start();

    out.push_str(&format!(
        "DFA States ({} states, {} byte classes):\n",
        num_states, num_classes
    ));
    out.push_str(&"=".repeat(40));
    out.push_str("\n\n");

    for state in 0..num_states as DfaStateId {
        if state == DEAD_STATE {
            continue;
        }
        let mut markers = Vec::new();
        if state == start {
            markers.push("START");
        }
        if accepting[state as usize] {
            markers.push("ACCEPT");
        }
        let marker = if markers.is_empty() {
            String::new()
        } else {
            format!(" ({})", markers.join(", "))
        };
        out.push_str(&format!("State {}{}\n", state, marker));

        let row = state as usize * num_classes;
        let mut by_target: Vec<(DfaStateId, Vec<usize>)> = Vec::new();
        for class in 0..num_classes {
            let target = transitions[row + class];
            if target == DEAD_STATE {
                continue;
            }
            match by_target.iter_mut().find(|(t, _)| *t == target) {
                Some(entry) => entry.1.push(class),
                None => by_target.push((target, vec![class])),
            }
        }

        for (target, classes) in &by_target {
            let all_ranges: Vec<(u8, u8)> = classes
                .iter()
                .flat_map(|&c| class_ranges[c].iter().copied())
                .collect();
            let label = format_class(&all_ranges);
            out.push_str(&format!("  {} ──> {}\n", label, target));
        }
        out.push('\n');
    }
    out
}

/// Generate a Graphviz DOT representation of the DFA.
pub fn to_dot_string(dfa: &Dfa) -> String {
    let mut out = String::new();
    let class_ranges = class_ranges(dfa);
    let num_states = dfa.num_states();
    let num_classes = dfa.num_classes();
    let transitions = dfa.transitions();
    let accepting = dfa.accepting();
    let start = dfa.start();

    out.push_str("digraph DFA {\n");
    out.push_str("    rankdir=LR;\n");
    out.push_str("    node [shape=circle];\n\n");

    out.push_str("    start [shape=point label=\"\"];\n");
    out.push_str(&format!("    start -> {};\n\n", start));

    for state in 0..num_states as DfaStateId {
        if state == DEAD_STATE {
            continue;
        }
        let shape = if accepting[state as usize] {
            "doublecircle"
        } else {
            "circle"
        };
        out.push_str(&format!(
            "    {} [shape={} label=\"{}\"];\n",
            state, shape, state
        ));
    }
    out.push('\n');

    for state in 0..num_states as DfaStateId {
        if state == DEAD_STATE {
            continue;
        }
        let row = state as usize * num_classes;
        let mut by_target: Vec<(DfaStateId, Vec<usize>)> = Vec::new();
        for class in 0..num_classes {
            let target = transitions[row + class];
            if target == DEAD_STATE {
                continue;
            }
            match by_target.iter_mut().find(|(t, _)| *t == target) {
                Some(entry) => entry.1.push(class),
                None => by_target.push((target, vec![class])),
            }
        }

        for (target, classes) in &by_target {
            let all_ranges: Vec<(u8, u8)> = classes
                .iter()
                .flat_map(|&c| class_ranges[c].iter().copied())
                .collect();
            let label = format_class(&all_ranges);
            let label = label.replace('\\', "\\\\").replace('"', "\\\"");
            out.push_str(&format!(
                "    {} -> {} [label=\"{}\"];\n",
                state, target, label
            ));
        }
    }

    out.push_str("}\n");
    out
}
