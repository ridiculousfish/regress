use regress::automata::nfa::ByteRange;
use regress::automata::tdfa::{
    FinalCommand, MarkValue, TDFA_DEAD_STATE, TagCommand, Tdfa, TdfaStateId,
};
use regress::automata::util::format_byte_range;

fn class_ranges(tdfa: &Tdfa) -> Vec<Vec<(u8, u8)>> {
    let num_classes = tdfa.num_classes();
    let byte_to_class = tdfa.byte_to_class();
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

fn format_class(ranges: &[(u8, u8)]) -> String {
    ranges
        .iter()
        .map(|&(s, e)| format_byte_range(ByteRange::new(s, e)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_mark_src(src: &MarkValue) -> String {
    match src {
        MarkValue::CurrentPos => "pos".to_string(),
        MarkValue::Copy(m) => format!("m{}", m.0),
    }
}

fn format_tag_cmds(cmds: &[TagCommand]) -> String {
    cmds.iter()
        .map(|c| format!("m{} := {}", c.dst.0, format_mark_src(&c.src)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_finals(finals: &[FinalCommand]) -> String {
    finals
        .iter()
        .map(|c| format!("t{} := {}", c.tag, format_mark_src(&c.src)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Generate a compact summary of the TDFA's size and shape, instead of the
/// full state-by-state dump.
pub fn to_stats_string(tdfa: &Tdfa) -> String {
    let num_states = tdfa.num_states();
    let num_classes = tdfa.num_classes();
    let accepting = tdfa.accepting().iter().filter(|&&a| a).count();
    let transitions = tdfa.transitions();
    let live_transitions = transitions
        .iter()
        .filter(|&&t| t != TDFA_DEAD_STATE)
        .count();

    let mut out = String::new();
    out.push_str("TDFA stats:\n");
    out.push_str("===========\n");
    out.push_str(&format!("  states:           {}\n", num_states));
    out.push_str(&format!("  accepting states: {}\n", accepting));
    out.push_str(&format!("  byte classes:     {}\n", num_classes));
    out.push_str(&format!(
        "  transitions:      {} live / {} total\n",
        live_transitions,
        transitions.len()
    ));
    out.push_str(&format!("  tags:             {}\n", tdfa.num_tags()));
    out.push_str(&format!("  marks:            {}\n", tdfa.num_marks()));
    out
}

pub fn to_readable_string(tdfa: &Tdfa) -> String {
    let mut out = String::new();
    let class_ranges = class_ranges(tdfa);
    let num_states = tdfa.accepting().len();
    let num_classes = tdfa.num_classes();
    let transitions = tdfa.transitions();
    let trans_cmds = tdfa.transition_commands();
    let accepting = tdfa.accepting();
    let finals = tdfa.finals();
    let start = tdfa.start(0);

    out.push_str(&format!(
        "TDFA ({} states, {} byte classes, {} tags, {} marks):\n",
        num_states,
        num_classes,
        tdfa.num_tags(),
        tdfa.num_marks()
    ));
    out.push_str(&"=".repeat(40));
    out.push('\n');

    if !tdfa.entry_commands(0).is_empty() {
        out.push_str(&format!(
            "Entry commands: [{}]\n",
            format_tag_cmds(tdfa.entry_commands(0))
        ));
    }
    out.push('\n');

    for state in 0..num_states as TdfaStateId {
        if state == TDFA_DEAD_STATE {
            continue;
        }
        let mut markers = Vec::new();
        if state == start {
            markers.push("START".to_string());
        }
        if accepting[state as usize] {
            markers.push("ACCEPT".to_string());
        }
        let marker = if markers.is_empty() {
            String::new()
        } else {
            format!(" ({})", markers.join(", "))
        };
        out.push_str(&format!("State {}{}\n", state, marker));

        if accepting[state as usize] && !finals[state as usize].is_empty() {
            out.push_str(&format!(
                "  finals: [{}]\n",
                format_finals(&finals[state as usize])
            ));
        }

        // Group transitions by (target, command list) so identical edges
        // collapse into a single line.
        let row = state as usize * num_classes;
        let mut by_edge: Vec<(TdfaStateId, &[TagCommand], Vec<usize>)> = Vec::new();
        for class in 0..num_classes {
            let target = transitions[row + class];
            if target == TDFA_DEAD_STATE {
                continue;
            }
            let cmds: &[TagCommand] = &trans_cmds[row + class];
            match by_edge
                .iter_mut()
                .find(|(t, c, _)| *t == target && *c == cmds)
            {
                Some(entry) => entry.2.push(class),
                None => by_edge.push((target, cmds, vec![class])),
            }
        }

        for (target, cmds, classes) in &by_edge {
            let all_ranges: Vec<(u8, u8)> = classes
                .iter()
                .flat_map(|&c| class_ranges[c].iter().copied())
                .collect();
            let label = format_class(&all_ranges);
            if cmds.is_empty() {
                out.push_str(&format!("  {} ──> {}\n", label, target));
            } else {
                out.push_str(&format!(
                    "  {} ──> {}  [{}]\n",
                    label,
                    target,
                    format_tag_cmds(cmds)
                ));
            }
        }
        out.push('\n');
    }
    out
}
