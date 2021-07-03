use std::fs::File;
use std::io::{self, BufRead};

// Parse line from `DerivedCoreProperties.txt` with the following syntax:
// `0061..007A    ; ID_Start # L&  [26] LATIN SMALL LETTER A..LATIN SMALL LETTER Z`
// `00AA          ; ID_Start # Lo       FEMININE ORDINAL INDICATOR`
fn parse_line(line: &str, chars: &mut Vec<String>, property: &str) {
    let split_str = format!("; {}", property);
    let mut line_iter = line.split(&split_str);

    if let Some(codepoint_hexes) = line_iter.next() {
        if line_iter.next().is_none() {
            return;
        }

        let mut iter = codepoint_hexes.split("..");
        if let Some(first_hex) = iter.next() {
            if let Some(second_hex) = iter.next() {
                let first_int = u32::from_str_radix(first_hex.trim(), 16).unwrap();
                let second_int = u32::from_str_radix(second_hex.trim(), 16).unwrap();
                chars.push(format!("({}, {}),", first_int, second_int));
            } else {
                let i = u32::from_str_radix(first_hex.trim(), 16).unwrap();
                chars.push(format!("({}, {}),", i, i));
            }
        }
    }
}

pub(crate) fn generate_id_start() -> String {
    let file =
        File::open("DerivedCoreProperties.txt").expect("could not open DerivedCoreProperties.txt");
    let lines = io::BufReader::new(file).lines();
    let mut chars = Vec::new();

    for line in lines {
        parse_line(&line.unwrap(), &mut chars, "ID_Start");
    }

    let result = format!(
        r#"pub(crate) fn is_id_start(c: char) -> bool {{
    let i = c as u32;
    ID_START
        .binary_search_by(|&(start, end)| {{
            if i < start {{
                core::cmp::Ordering::Greater
            }} else if i > end {{
                core::cmp::Ordering::Less
            }} else {{
                core::cmp::Ordering::Equal
            }}
        }})
        .is_ok()
}}

static ID_START: &[(u32, u32)] = &[
    {}
];"#,
        chars.join("\n    ")
    );

    result
}

pub(crate) fn generate_id_continue() -> String {
    let file =
        File::open("DerivedCoreProperties.txt").expect("could not open DerivedCoreProperties.txt");
    let lines = io::BufReader::new(file).lines();
    let mut chars = Vec::new();

    for line in lines {
        parse_line(&line.unwrap(), &mut chars, "ID_Continue");
    }

    let result = format!(
        r#"pub(crate) fn is_id_continue(c: char) -> bool {{
    let i = c as u32;
    ID_CONTINUE
        .binary_search_by(|&(start, end)| {{
            if i < start {{
                core::cmp::Ordering::Greater
            }} else if i > end {{
                core::cmp::Ordering::Less
            }} else {{
                core::cmp::Ordering::Equal
            }}
        }})
        .is_ok()
}}

static ID_CONTINUE: &[(u32, u32)] = &[
    {}
];"#,
        chars.join("\n    ")
    );

    result
}
